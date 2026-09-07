use super::*;
use crate::orchestration::ContentSha256;
use crate::persistence_service::{PersistenceService, PersistenceServiceError as Error};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::de::DeserializeOwned;
use std::collections::HashSet;

pub(crate) fn invalid(error: impl std::fmt::Display) -> Error {
    Error::Validation(error.to_string())
}
pub(crate) fn json(value: &impl Serialize) -> Result<String, Error> {
    serde_json::to_string(value).map_err(invalid)
}
pub(crate) fn decode<T: DeserializeOwned>(value: &str) -> Result<T, Error> {
    serde_json::from_str(value).map_err(invalid)
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
    /// completing FeatureAgent turn cannot mix old scope with a new reply.
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
        let dependencies=db.prepare("SELECT d.feature_id,d.prerequisite_id,d.rationale FROM forge_feature_dependencies d JOIN forge_features f ON f.id=d.feature_id WHERE f.project_id=?1 ORDER BY d.feature_id,d.prerequisite_id")?.query_map([project],|r|Ok(FeatureDependency{feature_id:r.get(0)?,prerequisite_id:r.get(1)?,rationale:r.get(2)?}))?.collect::<Result<Vec<_>,_>>()?;
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
        Ok(ForgeSnapshot {
            integration_evidence,
            navigation,
            features,
            tasks: tasks(db, project)?,
            dependencies,
            current_wave_limit,
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
        validate_ready_brief(&f.brief)?;
        for source in &f.brief.sources {
            check_source(&tx, project, source)?;
        }
        tx.execute("UPDATE work_tasks SET state='superseded',updated_at=?2 WHERE feature_id=?1 AND state='ready'",params![feature_id,now()])?;
        // Allocate every identity first so proposals can depend on later proposals.
        let mut proposed = Vec::new();
        for draft in &f.brief.tasks {
            let mut draft = draft.clone();
            draft.feature_id = Some(feature_id.into());
            draft.dependencies.clear();
            proposed.push(insert_task(&tx, project, &draft, Some(expected))?);
        }
        for (index, draft) in f.brief.tasks.iter().enumerate() {
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
            validate_dependencies(&tx, project, &proposed[index].id, &persisted.dependencies)?;
            replace_task_dependencies(&tx, &proposed[index].id, &persisted.dependencies)?;
            tx.execute(
                "UPDATE work_tasks SET draft_json=?2 WHERE id=?1",
                params![proposed[index].id, json(&persisted)?],
            )?;
            tx.execute(
                "UPDATE work_task_revisions SET draft_json=?2 WHERE task_id=?1 AND revision=1",
                params![proposed[index].id, json(&persisted)?],
            )?;
        }
        tx.execute("UPDATE forge_features SET approved_revision=revision,state='approved',updated_at=?2 WHERE id=?1",params![feature_id,now()])?;
        tx.execute(
            "UPDATE forge_feature_revisions SET approved_at=?3 WHERE feature_id=?1 AND revision=?2",
            params![feature_id, expected, now()],
        )?;
        let result = feature(&tx, project, feature_id)?;
        super::publication::queue_plan(&tx, &result)?;
        tx.commit()?;
        self.reconcile_plan_publications(project)?;
        Ok(result)
    }
    pub fn evaluate(
        &self,
        project: &str,
        feature_id: &str,
        expected: u64,
        evaluation: FeatureEvaluation,
        structural_level: u8,
    ) -> Result<Feature, Error> {
        if structural_level > 3
            || [evaluation.impact, evaluation.confidence, evaluation.effort]
                .into_iter()
                .flatten()
                .any(|v| !(1..=5).contains(&v))
        {
            return Err(invalid("Ratings must be 1-5 and structural level 0-3"));
        }
        let mut db = self.persistence.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let f = feature(&tx, project, feature_id)?;
        editable_feature(&tx, &f, expected)?;
        tx.execute("UPDATE forge_features SET evaluation_json=?2,structural_level=?3,state=CASE WHEN state='incubating' THEN 'in_triage' ELSE state END,updated_at=?4 WHERE id=?1",params![feature_id,json(&evaluation)?,structural_level,now()])?;
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
            let cycle=feature_id==prerequisite || tx.query_row("WITH RECURSIVE deps(id) AS (SELECT prerequisite_id FROM forge_feature_dependencies WHERE feature_id=?1 UNION SELECT d.prerequisite_id FROM forge_feature_dependencies d JOIN deps ON d.feature_id=deps.id) SELECT EXISTS(SELECT 1 FROM deps WHERE id=?2)",params![prerequisite,feature_id],|r|r.get::<_,bool>(0))?;
            if cycle {
                return Err(invalid("Feature dependency would create a cycle"));
            }
            tx.execute("INSERT INTO forge_feature_dependencies(feature_id,prerequisite_id,rationale) VALUES(?1,?2,?3) ON CONFLICT(feature_id,prerequisite_id) DO UPDATE SET rationale=excluded.rationale",params![feature_id,prerequisite,rationale])?;
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
pub(crate) fn editable_feature(db: &Connection, f: &Feature, expected: u64) -> Result<(), Error> {
    release_failed(db, &f.project_id)?;
    let active:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM work_tasks WHERE feature_id=?1 AND active_pulsation_id IS NOT NULL) OR EXISTS(SELECT 1 FROM forge_turns WHERE feature_id=?1 AND status='running')",[&f.id],|r|r.get(0))?;
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
