//! Developer-started approval, wave planning and dispatch with recoverable progress.
use super::{
    BatchManifest, CreatePulsationFromTasksRequest, DependencyOrigin, Feature, FeatureDependency,
    FeatureForgeService, FeatureState, PathConfirmationSource, TaskSelection, Wave, WorkTask,
    service::{
        admit_preflight, apply_wave_move, check_project, check_source, feature, invalid, json,
        record, replay, tasks, text, validate_ready_brief,
    },
};
use crate::orchestration::ContentSha256;
use crate::persistence_service::{PersistenceService, PersistenceServiceError as Error};
use rusqlite::{Connection, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApproveAndStartRequest {
    pub project_id: String,
    pub feature_id: String,
    pub expected_revision: u64,
    pub request_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeatureStartBlocker {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeatureStartPreview {
    pub feature_id: String,
    pub expected_revision: u64,
    pub approved: bool,
    pub planned: bool,
    pub blockers: Vec<FeatureStartBlocker>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeatureStartResult {
    pub approved: bool,
    pub planned: bool,
    pub pulsation_id: Option<String>,
    pub blockers: Vec<FeatureStartBlocker>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartCommand {
    result: FeatureStartResult,
    /// Frozen before dispatch so retries use the same input even after reservation.
    tasks: Option<Vec<TaskSelection>>,
    title: String,
}

fn blocker(code: &str, message: impl ToString) -> FeatureStartBlocker {
    FeatureStartBlocker {
        code: code.into(),
        message: message.to_string(),
    }
}

impl FeatureForgeService {
    /// Advisory only: the command repeats every guard before committing each step.
    /// Reads prospective approved tasks without publishing a revision or reserving work.
    pub fn preview_approve_and_start(
        &self,
        request: &ApproveAndStartRequest,
    ) -> Result<FeatureStartPreview, Error> {
        let db = self.persistence.connection()?;
        let tx = db.unchecked_transaction()?;
        check_project(&tx, &request.project_id)?;
        let f = feature(&tx, &request.project_id, &request.feature_id)?;
        if f.revision != request.expected_revision {
            return Err(invalid("Feature revision changed"));
        }
        let mut blockers = Vec::new();
        let approved = f.approved_revision == Some(f.revision);
        let planned = approved && f.wave == Some(Wave::Current);
        let active: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM forge_turns WHERE feature_id=?1 AND status='running') OR EXISTS(SELECT 1 FROM work_tasks WHERE feature_id=?1 AND active_pulsation_id IS NOT NULL)", [&f.id], |r| r.get(0))?;
        if active
            || matches!(
                f.state,
                FeatureState::Archived
                    | FeatureState::Done
                    | FeatureState::ReadyForReview
                    | FeatureState::Executing
            )
        {
            blockers.push(blocker(
                "feature_unavailable",
                "Feature is archived, complete, awaiting delivery review, or has active work.",
            ));
            return Ok(FeatureStartPreview {
                feature_id: f.id,
                expected_revision: f.revision,
                approved,
                planned,
                blockers,
            });
        }
        if !approved {
            if let Err(error) = validate_ready_brief(&f.brief)
                .and_then(|_| admit_preflight(&tx, &request.project_id, &f, f.revision))
            {
                blockers.push(blocker("approval", error));
            }
            for source in &f.brief.sources {
                if let Err(error) = check_source(&tx, &request.project_id, source) {
                    blockers.push(blocker("source_changed", error));
                    break;
                }
            }
        }
        if let Some(issue) = wave_blocker(&tx, &f)? {
            blockers.push(issue);
        }
        let needs_review: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM project_pipeline_state WHERE project_id=?1 AND base_state='needs_review')", [&request.project_id], |r| r.get(0))?;
        if needs_review {
            blockers.push(blocker(
                "base_needs_review",
                "Reconcile the project before starting tasks.",
            ));
        }
        let preflight = self.persistence.stage_preflight(&tx, &request.project_id)?;
        if let Some(message) = crate::orchestration::stage_preflight::blocking_message(&preflight) {
            blockers.push(blocker("model_configuration", message));
        }

        let proposed = prospective_tasks(&tx, &f)?;
        if proposed.is_empty() {
            blockers.push(blocker(
                "no_tasks",
                "This revision has no outstanding tasks to start.",
            ));
        }
        let selected: HashSet<&str> = proposed.iter().map(|t| t.id.as_str()).collect();
        for t in &proposed {
            if t.state != "ready" || t.active_pulsation_id.is_some() {
                blockers.push(blocker(
                    "task_unavailable",
                    format!(
                        "Task '{}' is already reserved or unavailable.",
                        t.draft.title
                    ),
                ));
                break;
            }
            for dependency in &t.draft.dependencies {
                if !selected.contains(dependency.as_str())
                    && super::service::task(&tx, &request.project_id, dependency)
                        .map_or(true, |t| t.state != "done")
                {
                    blockers.push(blocker(
                        "task_dependencies",
                        "Task prerequisites must be delivered or included in this feature.",
                    ));
                    break;
                }
            }
        }
        let mut remaining: HashSet<&str> = selected.clone();
        loop {
            let ready: Vec<_> = proposed
                .iter()
                .filter(|t| {
                    remaining.contains(t.id.as_str())
                        && t.draft
                            .dependencies
                            .iter()
                            .all(|d| !remaining.contains(d.as_str()))
                })
                .map(|t| t.id.as_str())
                .collect();
            if ready.is_empty() {
                break;
            }
            for id in ready {
                remaining.remove(id);
            }
        }
        if !remaining.is_empty() {
            blockers.push(blocker(
                "task_dependencies",
                "Task dependencies contain a cycle.",
            ));
        }
        let composition = PersistenceService::composition_settings_for(&tx, &request.project_id)?;
        if composition.enabled && proposed.iter().any(|task| task.draft.criteria.is_empty()) {
            blockers.push(blocker(
                "acceptance_criteria",
                "Composed pulsations require acceptance criteria for every task.",
            ));
        }
        let pipeline = crate::orchestration::PipelineVersion::for_new_batch(composition.enabled);
        let destination_plan = if composition.enabled && !proposed.is_empty() {
            match PersistenceService::resolve_batch_destinations(
                &tx,
                &request.project_id,
                &HashMap::new(),
                &proposed,
                std::slice::from_ref(&f),
            ) {
                Ok(plan) => Some(plan),
                Err(error) => {
                    blockers.push(blocker("destinations", error));
                    None
                }
            }
        } else {
            None
        };
        let feature_dependencies = tx.prepare("SELECT prerequisite_id,rationale,origin FROM forge_feature_dependencies WHERE feature_id=?1 ORDER BY prerequisite_id")?
            .query_map([&f.id],|row|Ok(FeatureDependency {feature_id:f.id.clone(),prerequisite_id:row.get(0)?,rationale:row.get(1)?,origin:DependencyOrigin::from_db(&row.get::<_,String>(2)?)}))?
            .collect::<Result<Vec<_>,_>>()?;
        let manifest = BatchManifest {
            schema_version: 1,
            title: f.title.clone(),
            tasks: proposed,
            features: vec![f.clone()],
            feature_dependencies,
            path_confirmation_source: if destination_plan.is_some() {
                PathConfirmationSource::ForgeGate
            } else {
                PathConfirmationSource::None
            },
            task_destinations: destination_plan
                .as_ref()
                .map(|p| p.task_destinations.clone())
                .unwrap_or_default(),
            destination_notices: destination_plan.map(|p| p.notices).unwrap_or_default(),
        };
        let config = PersistenceService::runtime_config_from(&tx)?;
        match super::batch_budget::assess_batch_context(
            &manifest,
            pipeline,
            config.context_chars_limit,
        ) {
            Ok(budget) if !budget.fits() => blockers.push(blocker(
                "context_budget",
                format!(
                    "Batch needs {} characters; {} are available.",
                    budget.required_chars, budget.limit_chars
                ),
            )),
            Err(error) => blockers.push(blocker("context_budget", error)),
            _ => {}
        }
        Ok(FeatureStartPreview {
            feature_id: f.id,
            expected_revision: f.revision,
            approved,
            planned,
            blockers,
        })
    }

    /// Approvals and wave placement remain committed when a later step is blocked.
    /// The existing batch command provides the exactly-once dispatch boundary.
    pub fn approve_and_start(
        &self,
        request: ApproveAndStartRequest,
    ) -> Result<FeatureStartResult, Error> {
        self.run_approve_and_start(request, || {})
    }

    #[cfg(test)]
    pub(crate) fn approve_and_start_after_selection(
        &self,
        request: ApproveAndStartRequest,
        after_selection: impl FnOnce(),
    ) -> Result<FeatureStartResult, Error> {
        self.run_approve_and_start(request, after_selection)
    }

    fn run_approve_and_start(
        &self,
        request: ApproveAndStartRequest,
        after_selection: impl FnOnce(),
    ) -> Result<FeatureStartResult, Error> {
        text(&request.request_id, 200)?;
        let mut command = {
            let mut db = self.persistence.connection()?;
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            check_project(&tx, &request.project_id)?;
            if let Some(command) = replay::<StartCommand>(
                &tx,
                &request.project_id,
                &request.request_id,
                "approve_and_start",
                &request,
            )? {
                command
            } else {
                let f = feature(&tx, &request.project_id, &request.feature_id)?;
                if f.revision != request.expected_revision {
                    return Err(invalid("Feature revision changed"));
                }
                let command = StartCommand {
                    result: FeatureStartResult::default(),
                    tasks: None,
                    title: f.title,
                };
                record(
                    &tx,
                    &request.project_id,
                    &request.request_id,
                    "approve_and_start",
                    &request,
                    &command,
                )?;
                tx.commit()?;
                command
            }
        };
        if command.result.pulsation_id.is_some() {
            return Ok(command.result);
        }
        if let Err(error) = self.approve(
            &request.project_id,
            &request.feature_id,
            request.expected_revision,
        ) {
            let approved = self
                .get(&request.project_id, &request.feature_id)
                .is_ok_and(|f| {
                    f.revision == request.expected_revision
                        && f.approved_revision == Some(request.expected_revision)
                });
            return self.finish_start_step(
                &request,
                approved,
                false,
                None,
                Some(blocker("approval", error)),
            );
        }
        self.finish_start_step(&request, true, false, None, None)?;

        // Freeze selection under the same lock as the durable command. Concurrent
        // callers reuse it rather than inspecting already-queued tasks differently.
        command = {
            let mut db = self.persistence.connection()?;
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let mut command = replay::<StartCommand>(
                &tx,
                &request.project_id,
                &request.request_id,
                "approve_and_start",
                &request,
            )?
            .ok_or_else(|| invalid("Start request disappeared"))?;
            if command.result.pulsation_id.is_some() {
                return Ok(command.result);
            }
            let f = feature(&tx, &request.project_id, &request.feature_id)?;
            if f.revision != request.expected_revision {
                return Err(invalid("Feature revision changed"));
            }
            if command.tasks.is_none() {
                command.tasks = Some(
                    tasks(&tx, &request.project_id)?
                        .into_iter()
                        .filter(|t| {
                            t.draft.feature_id.as_deref() == Some(f.id.as_str())
                                && t.feature_revision == Some(f.revision)
                                && t.state != "done"
                                && t.state != "superseded"
                        })
                        .map(|t| TaskSelection {
                            task_id: t.id,
                            expected_revision: t.revision,
                            manual_path_hint: None,
                        })
                        .collect(),
                );
                store_command(&tx, &request, &command)?;
            }
            tx.commit()?;
            command
        };
        after_selection();
        let wave_issue = match self.plan_start_wave(&request) {
            Ok(issue) => issue,
            Err(error) => Some(blocker("wave_planning", error)),
        };
        if let Some(issue) = wave_issue {
            return self.finish_start_step(&request, true, false, None, Some(issue));
        }
        self.finish_start_step(&request, true, true, None, None)?;
        let selection = command.tasks.unwrap_or_default();
        if selection.is_empty() {
            return self.finish_start_step(
                &request,
                true,
                true,
                None,
                Some(blocker(
                    "no_tasks",
                    "This revision has no outstanding tasks to start.",
                )),
            );
        }
        let subrequest = format!(
            "feature-start:{}",
            ContentSha256::of(json(&request)?.as_bytes()).as_str()
        );
        match self
            .persistence
            .create_pulsation_from_tasks(CreatePulsationFromTasksRequest {
                project_id: request.project_id.clone(),
                request_id: subrequest,
                title: command.title,
                tasks: selection,
            }) {
            Ok(pulse) => self.finish_start_step(&request, true, true, Some(pulse.id), None),
            Err(error) => {
                self.finish_start_step(&request, true, true, None, Some(blocker("dispatch", error)))
            }
        }
    }

    fn plan_start_wave(
        &self,
        request: &ApproveAndStartRequest,
    ) -> Result<Option<FeatureStartBlocker>, Error> {
        let mut db = self.persistence.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_project(&tx, &request.project_id)?;
        let f = feature(&tx, &request.project_id, &request.feature_id)?;
        if f.revision != request.expected_revision {
            return Ok(Some(blocker(
                "stale_revision",
                "The feature revision changed after approval; review the newer scope before starting it.",
            )));
        }
        if f.approved_revision != Some(request.expected_revision) {
            return Ok(Some(blocker(
                "approval",
                "Approve this feature revision before planning its wave.",
            )));
        }
        // The fence applies even when another actor already placed a revision
        // in Current; that placement cannot be attributed to an older request.
        if f.wave == Some(Wave::Current) {
            tx.commit()?;
            return Ok(None);
        }
        if let Some(issue) = wave_blocker(&tx, &f)? {
            return Ok(Some(issue));
        }
        apply_wave_move(&tx, &request.project_id, &f, Wave::Current, f.position)?;
        tx.commit()?;
        Ok(None)
    }

    fn finish_start_step(
        &self,
        request: &ApproveAndStartRequest,
        approved: bool,
        planned: bool,
        pulse: Option<String>,
        issue: Option<FeatureStartBlocker>,
    ) -> Result<FeatureStartResult, Error> {
        let mut db = self.persistence.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut command = replay::<StartCommand>(
            &tx,
            &request.project_id,
            &request.request_id,
            "approve_and_start",
            request,
        )?
        .ok_or_else(|| invalid("Start request disappeared"))?;
        if command.result.pulsation_id.is_none() {
            command.result.approved |= approved;
            command.result.planned |= planned;
            command.result.pulsation_id = pulse;
            command.result.blockers = issue.into_iter().collect();
            store_command(&tx, request, &command)?;
        }
        tx.commit()?;
        Ok(command.result)
    }
}

fn store_command(
    db: &Connection,
    request: &ApproveAndStartRequest,
    command: &StartCommand,
) -> Result<(), Error> {
    db.execute("UPDATE forge_commands SET result_json=?3 WHERE project_id=?1 AND request_id=?2 AND kind='approve_and_start'", params![request.project_id, request.request_id, json(command)?])?;
    Ok(())
}

fn wave_blocker(db: &Connection, f: &Feature) -> Result<Option<FeatureStartBlocker>, Error> {
    let unfinished: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM forge_feature_dependencies d JOIN forge_features p ON p.id=d.prerequisite_id WHERE d.feature_id=?1 AND p.state<>'done')", [&f.id], |r|r.get(0))?;
    if unfinished {
        return Ok(Some(blocker(
            "feature_dependencies",
            "Deliver the feature's prerequisites before starting it alone.",
        )));
    }
    if f.wave == Some(Wave::Current) {
        return Ok(None);
    }
    let (used, limit): (u32,u32) = db.query_row("SELECT (SELECT count(*) FROM forge_features WHERE project_id=?1 AND id<>?2 AND wave='current' AND state NOT IN ('done','archived')), COALESCE((SELECT current_wave_limit FROM forge_project_settings WHERE project_id=?1),3)", params![f.project_id,f.id], |r|Ok((r.get(0)?,r.get(1)?)))?;
    Ok((used >= limit).then(|| {
        blocker(
            "wave_capacity",
            "Current wave capacity reached; finish work or adjust the wave first.",
        )
    }))
}

fn prospective_tasks(db: &Connection, f: &Feature) -> Result<Vec<WorkTask>, Error> {
    let current = tasks(db, &f.project_id)?;
    if f.approved_revision == Some(f.revision) {
        return Ok(current
            .into_iter()
            .filter(|t| {
                t.draft.feature_id.as_deref() == Some(f.id.as_str())
                    && t.feature_revision == Some(f.revision)
                    && !matches!(t.state.as_str(), "done" | "superseded")
            })
            .collect());
    }
    let done: HashMap<_, _> = current
        .iter()
        .filter(|t| t.draft.feature_id.as_deref() == Some(f.id.as_str()) && t.state == "done")
        .map(|t| (t.draft.title.trim().to_lowercase(), t.id.clone()))
        .collect();
    let identities: Vec<_> = f
        .brief
        .tasks
        .iter()
        .enumerate()
        .map(|(index, draft)| {
            done.get(&draft.title.trim().to_lowercase())
                .cloned()
                .unwrap_or_else(|| format!("proposal:{}", index + 1))
        })
        .collect();
    Ok(f.brief
        .tasks
        .iter()
        .enumerate()
        .filter(|(_, d)| !done.contains_key(&d.title.trim().to_lowercase()))
        .map(|(i, d)| {
            let mut draft = d.clone();
            draft.feature_id = Some(f.id.clone());
            for dependency in &mut draft.dependencies {
                if let Some(index) = dependency
                    .strip_prefix("proposal:")
                    .and_then(|n| n.parse::<usize>().ok())
                    .and_then(|n| n.checked_sub(1))
                    && let Some(identity) = identities.get(index)
                {
                    *dependency = identity.clone();
                }
            }
            WorkTask {
                id: identities[i].clone(),
                project_id: f.project_id.clone(),
                revision: 1,
                draft,
                feature_revision: Some(f.revision),
                state: "ready".into(),
                active_pulsation_id: None,
                completed_pulsation_id: None,
            }
        })
        .collect())
}
