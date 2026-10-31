use super::preflight::{
    self, ArchitectureConflict, ConflictSeverity, DecisionInputs, PreflightAction,
    PreflightFinding, PreflightResolution,
};
use super::{
    CreateFeatureRequest, CreatePulsationFromTasksRequest, CreateTasksRequest, DependencyOrigin,
    DispatchGateStatus, Feature, FeatureAgentRuntime, FeatureBrief, FeatureDependency,
    FeatureEvaluation, FeatureState, FeatureTurn, ForgeNavigation, ForgeSnapshot, Serialize,
    TaskDraft, TaskSelection, Wave, WorkTask,
};
use crate::orchestration::ContentSha256;
use crate::persistence_service::{PersistenceService, PersistenceServiceError as Error};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::de::DeserializeOwned;
use std::collections::{HashMap, HashSet};

pub(crate) fn invalid(error: impl std::fmt::Display) -> Error {
    Error::Validation(error.to_string())
}
pub(crate) fn json(value: &impl Serialize) -> Result<String, Error> {
    serde_json::to_string(value).map_err(invalid)
}
pub(crate) fn decode<T: DeserializeOwned>(value: &str) -> Result<T, Error> {
    let trimmed = value.trim();
    if let Ok(result) = serde_json::from_str(trimmed) {
        return Ok(result);
    }
    if trimmed.starts_with("```") {
        let without_fences = trimmed
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim();
        if let Ok(result) = serde_json::from_str(without_fences) {
            return Ok(result);
        }
    }
    if let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}'))
        && end > start
    {
        let slice = &trimmed[start..=end];
        if let Ok(result) = serde_json::from_str(slice) {
            return Ok(result);
        }
    }
    if let (Some(start), Some(end)) = (trimmed.find('['), trimmed.rfind(']'))
        && end > start
    {
        let slice = &trimmed[start..=end];
        if let Ok(result) = serde_json::from_str(slice) {
            return Ok(result);
        }
    }
    serde_json::from_str(trimmed).map_err(invalid)
}
pub(crate) fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}
pub(crate) fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
pub(crate) fn text(value: &str, limit: usize) -> Result<(), Error> {
    if value.trim().is_empty() || value.chars().count() > limit || value.contains('\0') {
        return Err(invalid("Text is empty or exceeds its allowed size"));
    }
    Ok(())
}
pub(crate) fn check_project(db: &Connection, project: &str) -> Result<(), Error> {
    if !db.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1 AND is_archived=0)",
        [project],
        |r| r.get::<_, bool>(0),
    )? {
        return Err(invalid("Project is unavailable"));
    }
    Ok(())
}
pub(crate) fn replay<T: DeserializeOwned>(
    db: &Connection,
    project: &str,
    request: &str,
    kind: &str,
    input: &impl Serialize,
) -> Result<Option<T>, Error> {
    text(request, 200)?;
    let prior: Option<(String,String,String)> = db.query_row("SELECT kind,input_sha256,result_json FROM forge_commands WHERE project_id=?1 AND request_id=?2",params![project,request],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    if let Some((prior_kind, hash, result)) = prior {
        if prior_kind != kind || hash != ContentSha256::of(json(input)?.as_bytes()).as_str() {
            return Err(invalid("Request ID was already used with different input"));
        }
        return Ok(Some(decode(&result)?));
    }
    Ok(None)
}
pub(crate) fn record(
    db: &Connection,
    project: &str,
    request: &str,
    kind: &str,
    input: &impl Serialize,
    output: &impl Serialize,
) -> Result<(), Error> {
    db.execute("INSERT INTO forge_commands(project_id,request_id,kind,input_sha256,result_json) VALUES(?1,?2,?3,?4,?5)",params![project,request,kind,ContentSha256::of(json(input)?.as_bytes()).as_str(),json(output)?])?;
    Ok(())
}

#[derive(Clone)]
pub struct TaskService {
    pub(crate) persistence: PersistenceService,
}
#[derive(Clone)]
pub struct FeatureForgeService {
    pub(crate) persistence: PersistenceService,
}

pub struct ForgeViewSnapshot {
    pub snapshot: ForgeSnapshot,
    pub selected_feature: Option<String>,
    pub history: Vec<FeatureTurn>,
}

impl TaskService {
    pub fn new(persistence: PersistenceService) -> Self {
        Self { persistence }
    }
    pub fn create(&self, request: CreateTasksRequest) -> Result<Vec<WorkTask>, Error> {
        let mut db = self.persistence.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_project(&tx, &request.project_id)?;
        if let Some(result) = replay(
            &tx,
            &request.project_id,
            &request.request_id,
            "create_tasks",
            &request,
        )? {
            return Ok(result);
        }
        if request.tasks.is_empty() || request.tasks.len() > 100 {
            return Err(invalid("Choose between 1 and 100 tasks"));
        }
        let mut result = Vec::new();
        for draft in &request.tasks {
            if draft.feature_id.is_some() {
                return Err(invalid(
                    "Feature tasks are created by approving a feature revision",
                ));
            }
            result.push(insert_task(&tx, &request.project_id, draft, None)?);
        }
        record(
            &tx,
            &request.project_id,
            &request.request_id,
            "create_tasks",
            &request,
            &result,
        )?;
        tx.commit()?;
        Ok(result)
    }
    pub fn list(&self, project: &str) -> Result<Vec<WorkTask>, Error> {
        tasks(&self.persistence.connection()?, project)
    }
    pub fn revise(
        &self,
        project: &str,
        task_id: &str,
        expected_revision: u64,
        draft: TaskDraft,
    ) -> Result<WorkTask, Error> {
        validate_task(&draft)?;
        let mut db = self.persistence.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        release_failed(&tx, project)?;
        let previous = task(&tx, project, task_id)?;
        if previous.revision != expected_revision
            || previous.state != "ready"
            || previous.active_pulsation_id.is_some()
        {
            return Err(invalid("Task changed or is already dispatched"));
        }
        if previous.draft.feature_id.is_some() || draft.feature_id.is_some() {
            return Err(invalid("Revise the feature scope to change its tasks"));
        }
        validate_dependencies(&tx, project, task_id, &draft.dependencies)?;
        tx.execute(
            "UPDATE work_tasks SET revision=revision+1,draft_json=?2,updated_at=?3 WHERE id=?1",
            params![task_id, json(&draft)?, now()],
        )?;
        tx.execute(
            "INSERT INTO work_task_revisions(task_id,revision,draft_json) VALUES(?1,?2,?3)",
            params![task_id, expected_revision + 1, json(&draft)?],
        )?;
        replace_task_dependencies(&tx, task_id, &draft.dependencies)?;
        let result = task(&tx, project, task_id)?;
        tx.commit()?;
        Ok(result)
    }
    pub fn suggest_feature(draft: &TaskDraft) -> bool {
        let value = format!("{} {}", draft.title, draft.description).to_lowercase();
        [
            "new feature",
            "nova funcionalidade",
            "new module",
            "novo módulo",
            "entire feature",
            "sistema completo",
        ]
        .iter()
        .any(|word| value.contains(word))
    }
    pub fn convert_to_feature(
        &self,
        project: &str,
        task_id: &str,
        revision: u64,
        request_id: &str,
        title: &str,
        idea: &str,
    ) -> Result<Feature, Error> {
        text(title, 200)?;
        text(idea, 20_000)?;
        let input = (task_id, revision, title, idea);
        let mut db = self.persistence.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(result) = replay(&tx, project, request_id, "convert_task", &input)? {
            return Ok(result);
        }
        release_failed(&tx, project)?;
        let t = task(&tx, project, task_id)?;
        if t.revision != revision
            || t.state != "ready"
            || t.active_pulsation_id.is_some()
            || t.draft.feature_id.is_some()
        {
            return Err(invalid(
                "Only an available standalone task can be converted",
            ));
        }
        let feature_id = id();
        let brief = FeatureBrief::default();
        let original = format!(
            "{idea}\n\nOriginal task: {}\n{}",
            t.draft.description,
            t.draft
                .criteria
                .iter()
                .map(|c| c.description.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        );
        tx.execute("INSERT INTO forge_features(id,project_id,title,original_idea,brief_json,evaluation_json,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?7)",params![feature_id,project,title,original,json(&brief)?,json(&FeatureEvaluation::default())?,now()])?;
        tx.execute(
            "INSERT INTO forge_feature_revisions(feature_id,revision,brief_json) VALUES(?1,1,?2)",
            params![feature_id, json(&brief)?],
        )?;
        tx.execute(
            "INSERT INTO forge_task_conversions(task_id,feature_id) VALUES(?1,?2)",
            params![task_id, feature_id],
        )?;
        tx.execute(
            "UPDATE work_tasks SET state='superseded',updated_at=?2 WHERE id=?1",
            params![task_id, now()],
        )?;
        let result = feature(&tx, project, &feature_id)?;
        record(&tx, project, request_id, "convert_task", &input, &result)?;
        tx.commit()?;
        Ok(result)
    }
    pub fn dispatch(
        &self,
        request: CreatePulsationFromTasksRequest,
    ) -> Result<crate::persistence_service::Pulsation, Error> {
        self.persistence.create_pulsation_from_tasks(request)
    }
    /// Read-only preview of the dispatch-time file-path anchoring gate
    /// (ADR-022 clarification loop, P1/A1.3), for the UI to check before
    /// attempting a dispatch.
    pub fn preview_dispatch_gate(
        &self,
        project: &str,
        tasks: &[TaskSelection],
    ) -> Result<DispatchGateStatus, Error> {
        self.persistence.preview_dispatch_gate(project, tasks)
    }
}

impl FeatureForgeService {
    pub fn new(persistence: PersistenceService) -> Self {
        Self { persistence }
    }
    pub fn create(&self, request: CreateFeatureRequest) -> Result<Feature, Error> {
        text(&request.title, 200)?;
        text(&request.idea, 20_000)?;
        let mut db = self.persistence.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_project(&tx, &request.project_id)?;
        if let Some(result) = replay(
            &tx,
            &request.project_id,
            &request.request_id,
            "create_feature",
            &request,
        )? {
            return Ok(result);
        }
        let feature_id = id();
        let brief = FeatureBrief::default();
        tx.execute("INSERT INTO forge_features(id,project_id,title,original_idea,brief_json,evaluation_json,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?7)",params![feature_id,request.project_id,request.title.trim(),request.idea,json(&brief)?,json(&FeatureEvaluation::default())?,now()])?;
        tx.execute(
            "INSERT INTO forge_feature_revisions(feature_id,revision,brief_json) VALUES(?1,1,?2)",
            params![feature_id, json(&brief)?],
        )?;
        let result = feature(&tx, &request.project_id, &feature_id)?;
        record(
            &tx,
            &request.project_id,
            &request.request_id,
            "create_feature",
            &request,
            &result,
        )?;
        tx.commit()?;
        Ok(result)
    }
    pub fn get(&self, project: &str, feature_id: &str) -> Result<Feature, Error> {
        feature(&self.persistence.connection()?, project, feature_id)
    }
    pub fn save_navigation(
        &self,
        project: &str,
        navigation: &ForgeNavigation,
    ) -> Result<(), Error> {
        let db = self.persistence.connection()?;
        check_project(&db, project)?;
        if navigation.section > 4 {
            return Err(invalid("Unknown Forge section"));
        }
        if let Some(id) = &navigation.selected_feature_id {
            feature(&db, project, id)?;
        }
        db.execute("INSERT INTO runtime_settings(key,value,updated_at) VALUES(?1,?2,?3) ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at",params![format!("forge.navigation.{project}"),json(navigation)?,now()])?;
        Ok(())
    }
    pub fn snapshot(&self, project: &str) -> Result<ForgeSnapshot, Error> {
        let db = self.persistence.connection()?;
        let tx = db.unchecked_transaction()?;
        Self::snapshot_from_connection(&tx, project)
    }

    /// Read backlog, scope and conversation from one SQLite snapshot, so a
    /// completing `FeatureAgent` turn cannot mix old scope with a new reply.
    pub fn view_snapshot(
        &self,
        project: &str,
        selected: Option<&str>,
    ) -> Result<ForgeViewSnapshot, Error> {
        let db = self.persistence.connection()?;
        let tx = db.unchecked_transaction()?;
        let snapshot = Self::snapshot_from_connection(&tx, project)?;
        let selected_feature = selected
            .or(snapshot.navigation.selected_feature_id.as_deref())
            .filter(|id| snapshot.features.iter().any(|f| f.id == *id))
            .map(str::to_owned)
            .or_else(|| snapshot.features.first().map(|f| f.id.clone()));
        let history = selected_feature
            .as_ref()
            .map(|id| FeatureAgentRuntime::history_from_connection(&tx, project, id))
            .transpose()?
            .unwrap_or_default();
        Ok(ForgeViewSnapshot {
            snapshot,
            selected_feature,
            history,
        })
    }

    fn snapshot_from_connection(db: &Connection, project: &str) -> Result<ForgeSnapshot, Error> {
        let ids = db
            .prepare(
                "SELECT id FROM forge_features WHERE project_id=?1 ORDER BY position,created_at,id",
            )?
            .query_map([project], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let mut features = ids
            .iter()
            .map(|id| feature(db, project, id))
            .collect::<Result<Vec<_>, _>>()?;
        features.sort_by(|a, b| {
            a.position.cmp(&b.position).then_with(|| {
                match (a.evaluation.score(), b.evaluation.score()) {
                    (Some(a), Some(b)) => b.total_cmp(&a),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    _ => std::cmp::Ordering::Equal,
                }
            })
        });
        let dependencies=db.prepare("SELECT d.feature_id,d.prerequisite_id,d.rationale,d.origin FROM forge_feature_dependencies d JOIN forge_features f ON f.id=d.feature_id WHERE f.project_id=?1 ORDER BY d.feature_id,d.prerequisite_id")?.query_map([project],|r|Ok(FeatureDependency{feature_id:r.get(0)?,prerequisite_id:r.get(1)?,rationale:r.get(2)?,origin:DependencyOrigin::from_db(&r.get::<_,String>(3)?)}))?.collect::<Result<Vec<_>,_>>()?;
        let current_wave_limit = db
            .query_row(
                "SELECT current_wave_limit FROM forge_project_settings WHERE project_id=?1",
                [project],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(3);
        let navigation = db
            .query_row(
                "SELECT value FROM runtime_settings WHERE key=?1",
                [format!("forge.navigation.{project}")],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .map(|s| decode(&s))
            .transpose()?
            .unwrap_or_default();
        let integration_evidence = db.prepare("SELECT e.evidence_json FROM task_evidence e JOIN work_tasks t ON t.id=e.task_id AND t.completed_pulsation_id=e.pulsation_id JOIN pulsations p ON p.id=e.pulsation_id JOIN checkpoints c ON c.id=p.integration_checkpoint_id AND c.producer_job_id=e.job_id AND c.generation=e.generation WHERE t.project_id=?1 AND t.state='done' AND e.stage='Integrator' AND c.status='approved' ORDER BY t.id")?
            .query_map([project], |r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?.iter().map(|s|decode(s)).collect::<Result<Vec<_>,Error>>()?;
        let composition_enabled = db
            .query_row(
                "SELECT enabled FROM project_composition_settings WHERE project_id=?1",
                [project],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(false);
        let preflight = latest_preflight_findings(db, project)?;
        Ok(ForgeSnapshot {
            integration_evidence,
            navigation,
            features,
            tasks: tasks(db, project)?,
            dependencies,
            current_wave_limit,
            composition_enabled,
            preflight,
        })
    }
    pub fn revise(
        &self,
        project: &str,
        feature_id: &str,
        expected: u64,
        brief: FeatureBrief,
    ) -> Result<Feature, Error> {
        if json(&brief)?.len() > 250_000 {
            return Err(invalid("Feature brief exceeds its budget"));
        }
        let mut db = self.persistence.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous = feature(&tx, project, feature_id)?;
        editable_feature(&tx, &previous, expected)?;
        save_brief(&tx, feature_id, expected, &brief)?;
        let result = feature(&tx, project, feature_id)?;
        tx.commit()?;
        Ok(result)
    }
    pub fn approve(
        &self,
        project: &str,
        feature_id: &str,
        expected: u64,
    ) -> Result<Feature, Error> {
        let mut db = self.persistence.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let f = feature(&tx, project, feature_id)?;
        if f.revision != expected {
            return Err(invalid("Feature revision changed"));
        }
        if f.approved_revision == Some(expected) {
            return Ok(f);
        }
        editable_feature(&tx, &f, expected)?;
        let result = apply_approval(&tx, project, &f, expected)?;
        tx.commit()?;
        self.reconcile_plan_publications(project)?;
        Ok(result)
    }
    /// ADR-034 M6: the developer's decision on a blocking pre-flight finding
    /// of the feature's current revision. Adjust and Propose ADR keep the
    /// approval blocked; only a dismissal with a reason releases it.
    pub fn resolve_preflight(
        &self,
        project: &str,
        feature_id: &str,
        finding_id: &str,
        action: PreflightAction,
    ) -> Result<PreflightFinding, Error> {
        let (f, finding) = {
            let db = self.persistence.connection()?;
            let f = feature(&db, project, feature_id)?;
            let finding = preflight_finding_for(&db, feature_id, f.revision)?
                .filter(|finding| finding.id == finding_id)
                .ok_or_else(|| {
                    invalid("This finding is not the current triage of the feature; refresh and run the triage analysis again")
                })?;
            (f, finding)
        };
        if finding.blocking == 0 {
            return Err(invalid("This finding has no blocking conflict to resolve"));
        }
        if finding.resolution == PreflightResolution::Dismissed {
            return Err(invalid("This finding was already dismissed"));
        }
        let (resolution, reason, proposal) = match action {
            PreflightAction::Adjust => (PreflightResolution::Adjusted, None, None),
            PreflightAction::ProposeAdr => {
                let mut idea = format!(
                    "Proposed from the triage of \"{}\" (revision {}). The feature conflicts with these accepted decisions:\n",
                    f.title, f.revision
                );
                for conflict in &finding.conflicts {
                    idea.push_str(&format!(
                        "- {} ({:?}): {}\n",
                        finding
                            .decision_path(conflict)
                            .unwrap_or(&conflict.decision_ref),
                        conflict.severity,
                        conflict.conflict
                    ));
                }
                idea.push_str("Decide whether the decision changes. Until then the original feature stays blocked.");
                let title: String = format!("Architecture decision for {}", f.title)
                    .chars()
                    .take(200)
                    .collect();
                let proposal = self.create(CreateFeatureRequest {
                    project_id: project.into(),
                    request_id: format!("preflight-adr-{finding_id}"),
                    title,
                    idea,
                })?;
                (PreflightResolution::AdrProposed, None, Some(proposal.id))
            }
            PreflightAction::Dismiss { reason } => {
                let reason = reason.trim().to_owned();
                let chars = reason.chars().count();
                if chars == 0 || chars > preflight::MAX_REASON_CHARS {
                    return Err(invalid(format!(
                        "Dismissing a blocking conflict needs a reason of 1 to {} characters",
                        preflight::MAX_REASON_CHARS
                    )));
                }
                (PreflightResolution::Dismissed, Some(reason), None)
            }
        };
        let mut db = self.persistence.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "UPDATE forge_preflight_findings SET resolution=?2,reason=?3,proposal_feature_id=COALESCE(?4,proposal_feature_id),resolved_at=?5 WHERE id=?1 AND resolution=?6",
            params![
                finding_id,
                resolution.as_str(),
                reason,
                proposal,
                now(),
                finding.resolution.as_str()
            ],
        )?;
        if changed != 1 {
            return Err(invalid(
                "The finding changed meanwhile; refresh and try again",
            ));
        }
        let updated = preflight_finding_for(&tx, feature_id, f.revision)?
            .ok_or_else(|| invalid("The finding is missing"))?;
        tx.commit()?;
        Ok(updated)
    }
    pub fn evaluate(
        &self,
        project: &str,
        feature_id: &str,
        expected: u64,
        evaluation: FeatureEvaluation,
        structural_level: u8,
    ) -> Result<Feature, Error> {
        validate_evaluation(&evaluation, structural_level)?;
        let mut db = self.persistence.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let f = feature(&tx, project, feature_id)?;
        editable_feature(&tx, &f, expected)?;
        apply_evaluation(&tx, feature_id, &evaluation, structural_level)?;
        let result = feature(&tx, project, feature_id)?;
        tx.commit()?;
        Ok(result)
    }
    pub fn set_dependency(
        &self,
        project: &str,
        feature_id: &str,
        prerequisite: &str,
        rationale: Option<&str>,
    ) -> Result<(), Error> {
        let mut db = self.persistence.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let f = feature(&tx, project, feature_id)?;
        feature(&tx, project, prerequisite)?;
        editable_feature(&tx, &f, f.revision)?;
        if f.state == FeatureState::Executing || f.state == FeatureState::Done {
            return Err(invalid(
                "Dependencies cannot change during or after execution",
            ));
        }
        if let Some(rationale) = rationale {
            text(rationale, 4000)?;
            if dependency_cycle(&tx, feature_id, prerequisite)? {
                return Err(invalid("Feature dependency would create a cycle"));
            }
            upsert_dependency(
                &tx,
                feature_id,
                prerequisite,
                rationale,
                DependencyOrigin::Human,
            )?;
        } else {
            tx.execute(
                "DELETE FROM forge_feature_dependencies WHERE feature_id=?1 AND prerequisite_id=?2",
                params![feature_id, prerequisite],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn move_to_wave(
        &self,
        project: &str,
        feature_id: &str,
        wave: Wave,
        position: i64,
    ) -> Result<Feature, Error> {
        let mut db = self.persistence.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let f = feature(&tx, project, feature_id)?;
        editable_feature(&tx, &f, f.revision)?;
        if f.approved_revision != Some(f.revision)
            || matches!(
                f.state,
                FeatureState::Archived
                    | FeatureState::Done
                    | FeatureState::Executing
                    | FeatureState::ReadyForReview
            )
        {
            return Err(invalid(
                "Approve the feature revision before planning its wave",
            ));
        }
        if wave == Wave::Current {
            check_feature_prerequisites(&tx, feature_id)?;
            let limit: u32 = tx
                .query_row(
                    "SELECT current_wave_limit FROM forge_project_settings WHERE project_id=?1",
                    [project],
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or(3);
            let used:u32=tx.query_row("SELECT count(*) FROM forge_features WHERE project_id=?1 AND wave='current' AND state NOT IN ('done','archived') AND id<>?2",params![project,feature_id],|r|r.get(0))?;
            if used >= limit {
                return Err(invalid("Current wave capacity reached"));
            }
        }
        tx.execute("UPDATE forge_features SET wave=?2,position=?3,state='planned',updated_at=?4 WHERE id=?1",params![feature_id,enum_string(&wave)?,position,now()])?;
        let result = feature(&tx, project, feature_id)?;
        tx.commit()?;
        Ok(result)
    }
    pub fn set_wave_limit(&self, project: &str, limit: u32) -> Result<(), Error> {
        let mut db = self.persistence.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_project(&tx, project)?;
        let used:u32=tx.query_row("SELECT count(*) FROM forge_features WHERE project_id=?1 AND wave='current' AND state NOT IN ('done','archived')",[project],|r|r.get(0))?;
        if limit == 0 || limit < used {
            return Err(invalid(
                "Capacity cannot be below the current occupied count",
            ));
        }
        tx.execute("INSERT INTO forge_project_settings(project_id,current_wave_limit) VALUES(?1,?2) ON CONFLICT(project_id) DO UPDATE SET current_wave_limit=excluded.current_wave_limit",params![project,limit])?;
        tx.commit()?;
        Ok(())
    }
    pub fn archive(&self, project: &str, feature_id: &str, reason: &str) -> Result<(), Error> {
        text(reason, 4000)?;
        let mut db = self.persistence.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let f = feature(&tx, project, feature_id)?;
        editable_feature(&tx, &f, f.revision)?;
        tx.execute("UPDATE forge_features SET state='archived',archive_reason=?2,wave=NULL,updated_at=?3 WHERE id=?1",params![feature_id,reason,now()])?;
        tx.commit()?;
        Ok(())
    }
    pub fn reopen(&self, project: &str, feature_id: &str) -> Result<(), Error> {
        let db = self.persistence.connection()?;
        let f = feature(&db, project, feature_id)?;
        if f.state != FeatureState::Archived {
            return Err(invalid("Only archived features can be reopened"));
        }
        db.execute("UPDATE forge_features SET state=CASE WHEN approved_revision=revision THEN 'approved' ELSE 'incubating' END,archive_reason=NULL,updated_at=?2 WHERE id=?1",params![feature_id,now()])?;
        Ok(())
    }
    /// The "ask the Feature Agent" escape hatch from the dispatch-time
    /// file-path anchoring gate (ADR-022 clarification loop, P1/A1.3): adds a
    /// pending decision naming the task and reopens Incubation, which blocks
    /// reapproval (`validate_ready_brief`) and re-analysis
    /// (`validate_analysis_gate`) until it is answered. Only feature-owned
    /// tasks need this -- `TaskService::revise` already lets a developer add a
    /// path directly to a task with no `feature_id`.
    pub fn request_path_clarification(
        &self,
        project: &str,
        feature_id: &str,
        task_title: &str,
    ) -> Result<(), Error> {
        text(task_title, 200)?;
        let mut db = self.persistence.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let f = feature(&tx, project, feature_id)?;
        editable_feature(&tx, &f, f.revision)?;
        let mut brief = f.brief;
        let decision = format!("Define the explicit file path(s) for task \"{task_title}\".");
        if brief.pending_decisions.contains(&decision) {
            return Ok(());
        }
        brief.pending_decisions.push(decision);
        save_brief(&tx, feature_id, f.revision, &brief)?;
        tx.commit()?;
        Ok(())
    }
    pub fn complete(&self, project: &str, feature_id: &str, expected: u64) -> Result<(), Error> {
        let mut db = self.persistence.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let f = feature(&tx, project, feature_id)?;
        if f.revision != expected || f.approved_revision != Some(expected) {
            return Err(invalid("Feature revision changed"));
        }
        let (total,pending):(u32,u32)=tx.query_row("SELECT count(*),COALESCE(sum(state<>'done'),0) FROM work_tasks WHERE feature_id=?1 AND feature_revision=?2",params![feature_id,expected],|r|Ok((r.get(0)?,r.get(1)?)))?;
        if total == 0 || pending > 0 {
            return Err(invalid(
                "All tasks must be integrated before reviewing feature acceptance",
            ));
        }
        tx.execute(
            "UPDATE forge_features SET state='done',updated_at=?2 WHERE id=?1",
            params![feature_id, now()],
        )?;
        tx.commit()?;
        Ok(())
    }
}

pub(crate) fn enum_string(value: &impl Serialize) -> Result<String, Error> {
    Ok(serde_json::to_value(value)
        .map_err(invalid)?
        .as_str()
        .ok_or_else(|| invalid("Expected enum string"))?
        .into())
}
pub(crate) fn feature(db: &Connection, project: &str, feature_id: &str) -> Result<Feature, Error> {
    let row=db.query_row("SELECT id,project_id,title,original_idea,revision,approved_revision,state,brief_json,evaluation_json,structural_level,wave,position,archive_reason FROM forge_features WHERE id=?1 AND project_id=?2",params![feature_id,project],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,u64>(4)?,r.get::<_,Option<u64>>(5)?,r.get::<_,String>(6)?,r.get::<_,String>(7)?,r.get::<_,String>(8)?,r.get::<_,u8>(9)?,r.get::<_,Option<String>>(10)?,r.get::<_,i64>(11)?,r.get::<_,Option<String>>(12)?))).optional()?.ok_or_else(||invalid("Feature not found in this project"))?;
    Ok(Feature {
        id: row.0,
        project_id: row.1,
        title: row.2,
        original_idea: row.3,
        revision: row.4,
        approved_revision: row.5,
        state: decode(&json(&row.6)?)?,
        brief: decode(&row.7)?,
        evaluation: decode(&row.8)?,
        structural_level: row.9,
        wave: row.10.map(|v| decode(&json(&v)?)).transpose()?,
        position: row.11,
        archive_reason: row.12,
    })
}
pub(crate) fn task(db: &Connection, project: &str, task_id: &str) -> Result<WorkTask, Error> {
    let row=db.query_row("SELECT id,project_id,revision,draft_json,feature_revision,state,active_pulsation_id,completed_pulsation_id FROM work_tasks WHERE id=?1 AND project_id=?2",params![task_id,project],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,u64>(2)?,r.get::<_,String>(3)?,r.get::<_,Option<u64>>(4)?,r.get::<_,String>(5)?,r.get::<_,Option<String>>(6)?,r.get::<_,Option<String>>(7)?))).optional()?.ok_or_else(||invalid("Task not found in this project"))?;
    Ok(WorkTask {
        id: row.0,
        project_id: row.1,
        revision: row.2,
        draft: decode(&row.3)?,
        feature_revision: row.4,
        state: row.5,
        active_pulsation_id: row.6,
        completed_pulsation_id: row.7,
    })
}
pub(crate) fn tasks(db: &Connection, project: &str) -> Result<Vec<WorkTask>, Error> {
    let ids = db
        .prepare("SELECT id FROM work_tasks WHERE project_id=?1 ORDER BY created_at,id")?
        .query_map([project], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    ids.iter().map(|id| task(db, project, id)).collect()
}
pub(crate) fn validate_task(draft: &TaskDraft) -> Result<(), Error> {
    text(&draft.title, 200)?;
    text(&draft.description, 20_000)?;
    if draft.criteria.is_empty() || draft.criteria.len() > 40 {
        return Err(invalid("Tasks require 1-40 acceptance criteria"));
    }
    let mut ids = HashSet::new();
    for c in &draft.criteria {
        text(&c.id, 80)?;
        text(&c.description, 4000)?;
        if !ids.insert(&c.id) {
            return Err(invalid("Duplicate criterion ID"));
        }
    }
    Ok(())
}
pub(crate) fn validate_ready_brief(brief: &FeatureBrief) -> Result<(), Error> {
    text(&brief.objective, 20_000)?;
    text(&brief.audience, 8000)?;
    text(&brief.behavior, 40_000)?;
    text(&brief.scope, 20_000)?;
    if !brief.pending_decisions.is_empty()
        || brief.tasks.is_empty()
        || brief.tasks.len() > 100
        || brief.criteria.is_empty()
    {
        return Err(invalid(
            "Resolve pending decisions and define criteria and tasks before approval",
        ));
    }
    for draft in &brief.tasks {
        validate_task(draft)?;
    }
    Ok(())
}
pub(crate) fn validate_dependencies(
    db: &Connection,
    project: &str,
    task_id: &str,
    dependencies: &[String],
) -> Result<(), Error> {
    let mut seen = HashSet::new();
    for dependency in dependencies {
        task(db, project, dependency)?;
        if dependency == task_id || !seen.insert(dependency) {
            return Err(invalid("Duplicate or self dependency"));
        }
        let cycle:bool=db.query_row("WITH RECURSIVE deps(id) AS (SELECT prerequisite_id FROM work_task_dependencies WHERE task_id=?1 UNION SELECT d.prerequisite_id FROM work_task_dependencies d JOIN deps ON d.task_id=deps.id) SELECT EXISTS(SELECT 1 FROM deps WHERE id=?2)",params![dependency,task_id],|r|r.get(0))?;
        if cycle {
            return Err(invalid("Task dependency would create a cycle"));
        }
    }
    Ok(())
}
fn replace_task_dependencies(
    db: &Connection,
    task_id: &str,
    dependencies: &[String],
) -> Result<(), Error> {
    db.execute(
        "DELETE FROM work_task_dependencies WHERE task_id=?1",
        [task_id],
    )?;
    for dependency in dependencies {
        db.execute(
            "INSERT INTO work_task_dependencies(task_id,prerequisite_id) VALUES(?1,?2)",
            params![task_id, dependency],
        )?;
    }
    Ok(())
}
pub(crate) fn insert_task(
    db: &Connection,
    project: &str,
    draft: &TaskDraft,
    revision: Option<u64>,
) -> Result<WorkTask, Error> {
    validate_task(draft)?;
    let task_id = id();
    validate_dependencies(db, project, &task_id, &draft.dependencies)?;
    db.execute("INSERT INTO work_tasks(id,project_id,draft_json,feature_id,feature_revision,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?6)",params![task_id,project,json(draft)?,draft.feature_id,revision,now()])?;
    db.execute(
        "INSERT INTO work_task_revisions(task_id,revision,draft_json) VALUES(?1,1,?2)",
        params![task_id, json(draft)?],
    )?;
    replace_task_dependencies(db, &task_id, &draft.dependencies)?;
    task(db, project, &task_id)
}
fn normalized_title(title: &str) -> String {
    title.trim().to_lowercase()
}
/// This feature's already-delivered tasks, keyed by normalized title. The
/// Feature Agent rewrites `brief.tasks` from scratch on every revision with no
/// id shared with its own prior proposals, so it cannot tell a task it is
/// re-describing from one still outstanding; this is the host-side backstop
/// against `apply_approval` recreating (and then failing to dispatch, since
/// the duplicate is never `done`) work that already shipped.
fn done_tasks_by_title(
    db: &Connection,
    project: &str,
    feature_id: &str,
) -> Result<HashMap<String, WorkTask>, Error> {
    let ids = db
        .prepare(
            "SELECT id FROM work_tasks WHERE project_id=?1 AND feature_id=?2 AND state='done'",
        )?
        .query_map(params![project, feature_id], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    ids.into_iter()
        .map(|id| task(db, project, &id).map(|t| (normalized_title(&t.draft.title), t)))
        .collect()
}
/// Everything approval does inside the transaction, up to and including
/// queueing the plan note.
///
/// Split out so the agent triage analysis can approve from inside its own turn
/// commit. Reconciling publications is deliberately *not* part of this: it
/// opens its own connection and writes to disk, so it has to run after the
/// caller commits.
pub(crate) fn apply_approval(
    db: &Connection,
    project: &str,
    f: &Feature,
    expected: u64,
) -> Result<Feature, Error> {
    // Approval happens once per revision. Running it twice supersedes the tasks
    // the first run created and inserts a fresh copy of every one of them, so a
    // caller that skips this check silently duplicates delivered work. `approve`
    // keeps its own copy of this guard: it returns before `editable_feature`, so
    // a feature whose tasks are mid-pulsation still answers idempotently instead
    // of erroring. Both are load-bearing -- do not collapse them into one.
    if f.approved_revision == Some(expected) {
        return Ok(f.clone());
    }
    // ADR-034 M6: both the developer's approval and the automatic one at the
    // end of a triage pass here.
    admit_preflight(db, project, f, expected)?;
    let feature_id = f.id.as_str();
    validate_ready_brief(&f.brief)?;
    for source in &f.brief.sources {
        check_source(db, project, source)?;
    }
    db.execute("UPDATE work_tasks SET state='superseded',updated_at=?2 WHERE feature_id=?1 AND state='ready'",params![feature_id,now()])?;
    let done_tasks = done_tasks_by_title(db, project, feature_id)?;
    // Allocate every identity first so proposals can depend on later proposals.
    // A proposal whose title matches an already-done task of this feature reuses
    // that task's real id instead of inserting a duplicate: `done_tasks` only
    // holds delivered work, so this never touches a task still `ready`/`queued`,
    // which the supersede statement above already replaces wholesale.
    let mut proposed = Vec::new();
    let mut reused = vec![false; f.brief.tasks.len()];
    for (index, draft) in f.brief.tasks.iter().enumerate() {
        if let Some(existing) = done_tasks.get(&normalized_title(&draft.title)) {
            proposed.push(existing.clone());
            reused[index] = true;
            continue;
        }
        let mut draft = draft.clone();
        draft.feature_id = Some(feature_id.into());
        draft.dependencies.clear();
        proposed.push(insert_task(db, project, &draft, Some(expected))?);
    }
    for (index, draft) in f.brief.tasks.iter().enumerate() {
        if reused[index] {
            // Already delivered under its own id; leave its record untouched.
            continue;
        }
        let dependencies = draft
            .dependencies
            .iter()
            .map(|dependency| {
                if let Some(position) = dependency.strip_prefix("proposal:") {
                    let position = position
                        .parse::<usize>()
                        .ok()
                        .and_then(|i| i.checked_sub(1));
                    position
                        .and_then(|i| proposed.get(i))
                        .map(|t| t.id.clone())
                        .ok_or_else(|| invalid("Unknown proposed task dependency"))
                } else {
                    Ok(dependency.clone())
                }
            })
            .collect::<Result<Vec<_>, Error>>()?;
        let mut persisted = proposed[index].draft.clone();
        persisted.dependencies = dependencies;
        validate_dependencies(db, project, &proposed[index].id, &persisted.dependencies)?;
        replace_task_dependencies(db, &proposed[index].id, &persisted.dependencies)?;
        db.execute(
            "UPDATE work_tasks SET draft_json=?2 WHERE id=?1",
            params![proposed[index].id, json(&persisted)?],
        )?;
        db.execute(
            "UPDATE work_task_revisions SET draft_json=?2 WHERE task_id=?1 AND revision=1",
            params![proposed[index].id, json(&persisted)?],
        )?;
    }
    db.execute("UPDATE forge_features SET approved_revision=revision,state='approved',updated_at=?2 WHERE id=?1",params![feature_id,now()])?;
    db.execute(
        "UPDATE forge_feature_revisions SET approved_at=?3 WHERE feature_id=?1 AND revision=?2",
        params![feature_id, expected, now()],
    )?;
    let result = feature(db, project, feature_id)?;
    super::publication::queue_plan(db, &result)?;
    Ok(result)
}

fn map_finding(row: &rusqlite::Row<'_>) -> rusqlite::Result<PreflightFinding> {
    let json_error = |index: usize, error: serde_json::Error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    };
    let inputs: String = row.get(3)?;
    let conflicts: String = row.get(5)?;
    let resolution: String = row.get(7)?;
    Ok(PreflightFinding {
        id: row.get(0)?,
        feature_id: row.get(1)?,
        feature_revision: row.get(2)?,
        inputs: serde_json::from_str(&inputs).map_err(|e| json_error(3, e))?,
        inputs_sha256: row.get(4)?,
        conflicts: serde_json::from_str(&conflicts).map_err(|e| json_error(5, e))?,
        blocking: row.get(6)?,
        resolution: PreflightResolution::parse(&resolution).unwrap_or(PreflightResolution::Pending),
        reason: row.get(8)?,
        proposal_feature_id: row.get(9)?,
        created_at: row.get(10)?,
    })
}

const FINDING_COLUMNS: &str = "id,feature_id,feature_revision,inputs_json,inputs_sha256,conflicts_json,blocking,resolution,reason,proposal_feature_id,created_at";

/// The latest pre-flight finding of every feature of `project`.
fn latest_preflight_findings(
    db: &Connection,
    project: &str,
) -> Result<Vec<PreflightFinding>, Error> {
    Ok(db
        .prepare(&format!(
            "SELECT {FINDING_COLUMNS} FROM forge_preflight_findings f WHERE f.project_id=?1
             AND f.rowid=(SELECT g.rowid FROM forge_preflight_findings g WHERE g.feature_id=f.feature_id ORDER BY g.created_at DESC,g.rowid DESC LIMIT 1)
             ORDER BY f.feature_id"
        ))?
        .query_map([project], map_finding)?
        .collect::<Result<Vec<_>, _>>()?)
}

/// The latest finding of one feature revision, if a triage recorded one.
pub(crate) fn preflight_finding_for(
    db: &Connection,
    feature_id: &str,
    revision: u64,
) -> Result<Option<PreflightFinding>, Error> {
    Ok(db
        .query_row(
            &format!(
                "SELECT {FINDING_COLUMNS} FROM forge_preflight_findings WHERE feature_id=?1 AND feature_revision=?2 ORDER BY created_at DESC,rowid DESC LIMIT 1"
            ),
            params![feature_id, revision],
            map_finding,
        )
        .optional()?)
}

/// Records one triage's finding. A blocking conflict leaves it pending.
pub(crate) fn insert_preflight_finding(
    db: &Connection,
    project: &str,
    feature: &Feature,
    turn: &str,
    inputs: &DecisionInputs,
    conflicts: &[ArchitectureConflict],
) -> Result<PreflightFinding, Error> {
    let blocking = conflicts
        .iter()
        .filter(|conflict| conflict.severity == ConflictSeverity::Blocking)
        .count() as u32;
    let resolution = if blocking > 0 {
        PreflightResolution::Pending
    } else {
        PreflightResolution::None
    };
    let finding = PreflightFinding {
        id: id(),
        feature_id: feature.id.clone(),
        feature_revision: feature.revision,
        inputs: inputs.clone(),
        inputs_sha256: inputs.sha256(),
        conflicts: conflicts.to_vec(),
        blocking,
        resolution,
        reason: None,
        proposal_feature_id: None,
        created_at: now(),
    };
    db.execute(
        "INSERT INTO forge_preflight_findings(id,project_id,feature_id,feature_revision,turn_id,inputs_state,inputs_json,inputs_sha256,conflicts_json,blocking,resolution,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
        params![
            finding.id,
            project,
            finding.feature_id,
            finding.feature_revision,
            turn,
            inputs.state.as_str(),
            json(&finding.inputs)?,
            finding.inputs_sha256,
            json(&finding.conflicts)?,
            finding.blocking,
            finding.resolution.as_str(),
            finding.created_at,
        ],
    )?;
    Ok(finding)
}

/// ADR-034 M6: a blocking conflict the developer has not dismissed, a
/// finding whose decisions changed since triage, or a feature flagged on an
/// earlier revision and not triaged again, refuses the approval.
pub(crate) fn admit_preflight(
    db: &Connection,
    project: &str,
    f: &Feature,
    revision: u64,
) -> Result<(), Error> {
    let Some(finding) = preflight_finding_for(db, &f.id, revision)? else {
        let flagged: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM forge_preflight_findings WHERE feature_id=?1 AND blocking>0)",
            [&f.id],
            |r| r.get(0),
        )?;
        return if flagged {
            Err(invalid(
                "preflight_required: an earlier revision of this feature had a blocking architecture conflict; run the triage analysis again before approving",
            ))
        } else {
            Ok(())
        };
    };
    if finding.blocking > 0 && !finding.resolution.allows_approval() {
        return Err(invalid(
            "preflight_conflict_unresolved: a blocking architecture conflict needs a decision: adjust the feature, propose a decision or dismiss the warning with a reason",
        ));
    }
    let root: String = db.query_row(
        "SELECT workspace_path FROM projects WHERE id=?1",
        [project],
        |r| r.get(0),
    )?;
    let current = preflight::read_decisions(std::path::Path::new(&root), &Default::default());
    if current.sha256() != finding.inputs_sha256 {
        return Err(invalid(
            "preflight_stale: the project's decisions changed since the triage analysis; run it again before approving",
        ));
    }
    Ok(())
}

/// ADR-034 M6: the blocking conflicts the developer dismissed at the triage
/// of the features whose tasks a pulsation carries, as the developer's
/// decisions in its Specification brief.
pub(crate) fn dismissed_conflicts(
    db: &Connection,
    pulsation: &str,
) -> Result<Vec<crate::orchestration::review_brief::JudgementItem>, Error> {
    use crate::orchestration::review_brief::{JudgementItem, JudgementKind};
    let columns = FINDING_COLUMNS
        .split(',')
        .map(|column| format!("f.{column}"))
        .collect::<Vec<_>>()
        .join(",");
    let rows = db
        .prepare(&format!(
            "SELECT DISTINCT {columns},ff.title FROM forge_preflight_findings f
             JOIN forge_features ff ON ff.id=f.feature_id
             JOIN work_tasks t ON t.feature_id=f.feature_id AND t.feature_revision=f.feature_revision
             JOIN pulsation_tasks pt ON pt.task_id=t.id
             WHERE pt.pulsation_id=?1 AND f.resolution='dismissed'
             ORDER BY ff.title,f.created_at"
        ))?
        .query_map([pulsation], |row| Ok((map_finding(row)?, row.get::<_, String>(11)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        .map(|(finding, title)| {
            let blocking = finding
                .conflicts
                .iter()
                .filter(|conflict| conflict.severity == ConflictSeverity::Blocking)
                .collect::<Vec<_>>();
            JudgementItem {
                kind: JudgementKind::DismissedConflict,
                label: title,
                text: finding.reason.clone().unwrap_or_default(),
                detail: Some(
                    blocking
                        .iter()
                        .map(|conflict| conflict.conflict.as_str())
                        .collect::<Vec<_>>()
                        .join(" "),
                ),
                refs: blocking
                    .iter()
                    .map(|conflict| {
                        finding
                            .decision_path(conflict)
                            .unwrap_or(&conflict.decision_ref)
                            .to_owned()
                    })
                    .collect(),
            }
        })
        .collect())
}

pub(crate) fn validate_evaluation(
    evaluation: &FeatureEvaluation,
    structural_level: u8,
) -> Result<(), Error> {
    if structural_level > 3
        || [evaluation.impact, evaluation.confidence, evaluation.effort]
            .into_iter()
            .flatten()
            .any(|v| !(1..=5).contains(&v))
    {
        return Err(invalid("Ratings must be 1-5 and structural level 0-3"));
    }
    Ok(())
}

/// Writes the triage verdict. Deliberately does not touch `revision`: the agent
/// analysis commits this while holding a turn admitted at a specific
/// `feature_revision`, and that revision has to still match at commit time.
pub(crate) fn apply_evaluation(
    db: &Connection,
    feature_id: &str,
    evaluation: &FeatureEvaluation,
    structural_level: u8,
) -> Result<(), Error> {
    db.execute("UPDATE forge_features SET evaluation_json=?2,structural_level=?3,state=CASE WHEN state='incubating' THEN 'in_triage' ELSE state END,updated_at=?4 WHERE id=?1",params![feature_id,json(evaluation)?,structural_level,now()])?;
    Ok(())
}

/// Whether adding `feature_id -> prerequisite` would close a cycle.
///
/// Run this immediately before each individual insert rather than batching the
/// checks up front: the walk runs on the same connection as the pending
/// transaction, so it sees edges inserted earlier in that same transaction and
/// each edge is therefore checked against the graph the previous ones built.
/// `UNION` (not `UNION ALL`) is what terminates the recursion if the stored
/// graph somehow already contains a cycle.
pub(crate) fn dependency_cycle(
    db: &Connection,
    feature_id: &str,
    prerequisite: &str,
) -> Result<bool, Error> {
    if feature_id == prerequisite {
        return Ok(true);
    }
    Ok(db.query_row("WITH RECURSIVE deps(id) AS (SELECT prerequisite_id FROM forge_feature_dependencies WHERE feature_id=?1 UNION SELECT d.prerequisite_id FROM forge_feature_dependencies d JOIN deps ON d.feature_id=deps.id) SELECT EXISTS(SELECT 1 FROM deps WHERE id=?2)",params![prerequisite,feature_id],|r|r.get::<_,bool>(0))?)
}

/// Stores an edge, stamping who authored it.
///
/// An agent pass must never clobber a relation a developer confirmed by hand,
/// so its upsert only updates rows that are already agent-authored -- on a
/// human edge it is a clean no-op. A human write is unconditional and adopts
/// the edge, because a developer editing an agent suggestion is taking
/// ownership of it.
pub(crate) fn upsert_dependency(
    db: &Connection,
    feature_id: &str,
    prerequisite: &str,
    rationale: &str,
    origin: DependencyOrigin,
) -> Result<(), Error> {
    let sql = match origin {
        DependencyOrigin::Human => {
            "INSERT INTO forge_feature_dependencies(feature_id,prerequisite_id,rationale,origin) VALUES(?1,?2,?3,'human') ON CONFLICT(feature_id,prerequisite_id) DO UPDATE SET rationale=excluded.rationale,origin='human'"
        }
        DependencyOrigin::Agent => {
            "INSERT INTO forge_feature_dependencies(feature_id,prerequisite_id,rationale,origin) VALUES(?1,?2,?3,'agent') ON CONFLICT(feature_id,prerequisite_id) DO UPDATE SET rationale=excluded.rationale WHERE forge_feature_dependencies.origin='agent'"
        }
    };
    db.execute(sql, params![feature_id, prerequisite, rationale])?;
    Ok(())
}

pub(crate) fn editable_feature(db: &Connection, f: &Feature, expected: u64) -> Result<(), Error> {
    editable_feature_excluding_turn(db, f, expected, "")
}

/// The same admission check, but ignoring one turn.
///
/// An agent analysis commits its own writes while its `forge_turns` row is
/// still `running`, so the plain check would reject the analysis on account of
/// itself. Passing the analysis' own turn id excludes exactly that row and
/// keeps every other guarantee -- a concurrent chat turn, an active pulsation,
/// an archived feature and a stale revision all still reject.
pub(crate) fn editable_feature_excluding_turn(
    db: &Connection,
    f: &Feature,
    expected: u64,
    turn: &str,
) -> Result<(), Error> {
    release_failed(db, &f.project_id)?;
    let active:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM work_tasks WHERE feature_id=?1 AND active_pulsation_id IS NOT NULL) OR EXISTS(SELECT 1 FROM forge_turns WHERE feature_id=?1 AND status='running' AND id<>?2)",params![f.id,turn],|r|r.get(0))?;
    if f.revision != expected || active || f.state == FeatureState::Archived {
        return Err(invalid("Feature changed, is archived, or has active work"));
    }
    Ok(())
}
pub(crate) fn save_brief(
    db: &Connection,
    feature_id: &str,
    expected: u64,
    brief: &FeatureBrief,
) -> Result<(), Error> {
    if db.execute("UPDATE forge_features SET revision=revision+1,brief_json=?3,state='incubating',wave=NULL,updated_at=?4 WHERE id=?1 AND revision=?2",params![feature_id,expected,json(brief)?,now()])?!=1 { return Err(invalid("Feature revision changed")); }
    db.execute(
        "INSERT INTO forge_feature_revisions(feature_id,revision,brief_json) VALUES(?1,?2,?3)",
        params![feature_id, expected + 1, json(brief)?],
    )?;
    Ok(())
}
pub(crate) fn release_failed(db: &Connection, project: &str) -> Result<(), Error> {
    db.execute("UPDATE work_tasks SET state='ready',active_pulsation_id=NULL WHERE project_id=?1 AND state='queued' AND active_pulsation_id IN (SELECT id FROM pulsations WHERE status='failed') AND NOT EXISTS(SELECT 1 FROM project_pipeline_state WHERE project_id=?1 AND base_state='needs_review')",[project])?;
    db.execute("UPDATE forge_features SET state='planned' WHERE project_id=?1 AND state='executing' AND NOT EXISTS(SELECT 1 FROM work_tasks WHERE feature_id=forge_features.id AND active_pulsation_id IS NOT NULL)",[project])?;
    Ok(())
}
pub(crate) fn check_feature_prerequisites(db: &Connection, feature_id: &str) -> Result<(), Error> {
    if db.query_row("SELECT EXISTS(SELECT 1 FROM forge_feature_dependencies d JOIN forge_features f ON f.id=d.prerequisite_id WHERE d.feature_id=?1 AND f.state<>'done' AND (f.wave IS NOT 'current' OR f.approved_revision IS NOT f.revision OR f.state='archived'))",[feature_id],|r|r.get::<_,bool>(0))? { return Err(invalid("Plan approved prerequisites in the current wave first")); }
    Ok(())
}
pub(crate) fn check_source(
    db: &Connection,
    project: &str,
    source: &crate::orchestration::SourceRevision,
) -> Result<(), Error> {
    if source.project_id != project {
        return Err(invalid("Source belongs to a different project"));
    }
    let root: String = db.query_row(
        "SELECT workspace_path FROM projects WHERE id=?1",
        [project],
        |r| r.get(0),
    )?;
    if crate::orchestration::publication::observed_hash(
        std::path::Path::new(&root),
        source.relative_path.as_str(),
    )?
    .as_ref()
        != Some(&source.content_sha256)
    {
        return Err(invalid(
            "Source changed; review the feature against the current repository",
        ));
    }
    Ok(())
}
