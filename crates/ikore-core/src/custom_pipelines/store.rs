//! Feature 25 (M2a, M2b): a project's pipelines and the global ones, their
//! versions and boxes, and the commands on their runs. A module of
//! `persistence_service`; scope changes, children and adoption are in
//! `scope.rs`.

use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use uuid::Uuid;

use super::{Connection, PersistenceService, PersistenceServiceError};
use crate::custom_pipelines::{
    AUTOMATION_MAX_CONCURRENT_DEFAULT, AUTOMATION_MAX_CONCURRENT_KEY,
    AUTOMATION_MAX_CONCURRENT_RANGE, CustomPipelineDefinitionV1, CustomPipelineView,
    CustomPipelinesSnapshot, DecidePipelineGateRequest, MAX_ACTIVE_PIPELINES,
    MAX_DESCRIPTION_CHARS, MAX_GATE_NOTE_BYTES, MAX_PROJECT_PIPELINES, PipelineRunStatus,
    PipelineRunView, PipelineStep, PipelineStepRunView, PipelineTrigger, PipelineValidationError,
    RECENT_RUNS_PER_PIPELINE, SaveCustomPipelineOutcome, SaveCustomPipelineRequest, StepRunStatus,
    StepTransport, child_changes, parse_child, parse_definition, resolve_child,
    validate_definition, validate_name,
};
use crate::custom_pipelines::{
    GlobalPipelineSummary, MAX_GLOBAL_PIPELINES, SetPipelineTriggerRequest, validate_trigger,
};

fn invalid(message: impl Into<String>) -> PersistenceServiceError {
    PersistenceServiceError::Validation(message.into())
}

pub(super) fn required<'a>(value: &'a str, name: &str) -> Result<&'a str, PersistenceServiceError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(invalid(format!("{name} is required.")));
    }
    Ok(value)
}

pub(super) fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn definition_json(
    definition: &CustomPipelineDefinitionV1,
) -> Result<String, PersistenceServiceError> {
    serde_json::to_string(definition).map_err(|error| invalid(error.to_string()))
}

pub(super) fn stored_definition(
    json: &str,
) -> Result<CustomPipelineDefinitionV1, PersistenceServiceError> {
    parse_definition(json).map_err(|error| PersistenceServiceError::Runtime(error.to_string()))
}

fn bad_column(index: usize, value: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        rusqlite::types::Type::Text,
        format!("unexpected value `{value}`").into(),
    )
}

const VIEW_SQL: &str = "SELECT p.id,p.scope,p.project_id,p.name,p.description,p.current_version,
        a.adopted_version,a.position,a.state,a.needs_edit_reason,p.archived,v.definition_json,
        p.updated_at,
        (SELECT count(*) FROM custom_pipeline_runs r WHERE r.pipeline_id=p.id),
        p.parent_pipeline_id,a.adopted_parent_version,
        (SELECT name FROM custom_pipelines m WHERE m.id=p.parent_pipeline_id),
        (SELECT count(*) FROM custom_pipelines c WHERE c.parent_pipeline_id=p.id),
        (SELECT count(*) FROM custom_pipeline_activations o WHERE o.pipeline_id=p.id),
        (SELECT current_version FROM custom_pipelines m WHERE m.id=p.parent_pipeline_id),
        a.trigger_override_json
   FROM custom_pipeline_activations a
   JOIN custom_pipelines p ON p.id=a.pipeline_id
   JOIN custom_pipeline_versions v ON v.pipeline_id=p.id AND v.version=a.adopted_version
  WHERE a.project_id=?1";

fn map_view(row: &rusqlite::Row<'_>) -> rusqlite::Result<(CustomPipelineView, String)> {
    let state: String = row.get(8)?;
    Ok((
        CustomPipelineView {
            id: row.get(0)?,
            scope: row.get(1)?,
            project_id: row.get(2)?,
            name: row.get(3)?,
            description: row.get(4)?,
            current_version: row.get(5)?,
            adopted_version: row.get(6)?,
            position: row.get(7)?,
            ready: state == "ready",
            needs_edit_reason: row.get(9)?,
            archived: row.get(10)?,
            definition: CustomPipelineDefinitionV1 {
                schema_version: 0,
                trigger: PipelineTrigger::Manual,
                steps: Vec::new(),
            },
            run_count: row.get(13)?,
            updated_at: row.get(12)?,
            parent_pipeline_id: row.get(14)?,
            adopted_parent_version: row.get::<_, Option<u32>>(15)?.or(row.get(19)?),
            parent_name: row.get(16)?,
            children: row.get(17)?,
            projects: row.get(18)?,
            trigger_override: row
                .get::<_, Option<String>>(20)?
                .and_then(|json| serde_json::from_str(&json).ok()),
        },
        row.get(11)?,
    ))
}

/// What a pipeline's version resolves to: its definition, or a child's
/// changes over its parent's version (P1), with what no longer fits.
pub(super) fn resolved_for(
    db: &Connection,
    pipeline_id: &str,
    version: u32,
    parent: Option<(&str, u32)>,
) -> Result<(CustomPipelineDefinitionV1, Vec<String>), PersistenceServiceError> {
    let version_json = |pipeline: &str, version: u32| -> Result<String, PersistenceServiceError> {
        db.query_row(
            "SELECT definition_json FROM custom_pipeline_versions WHERE pipeline_id=?1 AND version=?2",
            params![pipeline, version],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| PersistenceServiceError::NotFound("pipeline version".into()))
    };
    let json = version_json(pipeline_id, version)?;
    match parent {
        None => Ok((stored_definition(&json)?, Vec::new())),
        Some((parent, parent_version)) => {
            let changes = parse_child(&json)
                .map_err(|error| PersistenceServiceError::Runtime(error.to_string()))?;
            let parent = stored_definition(&version_json(parent, parent_version)?)?;
            let resolved = resolve_child(&parent, &changes);
            Ok((resolved.definition, resolved.conflicts))
        }
    }
}

fn load_views(
    db: &Connection,
    project_id: &str,
    pipeline_id: Option<&str>,
) -> Result<Vec<CustomPipelineView>, PersistenceServiceError> {
    let sql = format!(
        "{VIEW_SQL} AND (?2 IS NULL OR p.id=?2) ORDER BY p.archived,a.position,p.created_at,p.id"
    );
    let mut statement = db.prepare(&sql)?;
    let rows = statement
        .query_map(params![project_id, pipeline_id], map_view)?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(mut view, json)| {
            view.definition = match (&view.parent_pipeline_id, view.adopted_parent_version) {
                (Some(parent), Some(parent_version)) => {
                    resolved_for(
                        db,
                        &view.id,
                        view.adopted_version,
                        Some((parent, parent_version)),
                    )?
                    .0
                }
                _ => stored_definition(&json)?,
            };
            Ok(view)
        })
        .collect()
}

pub(super) fn load_view(
    db: &Connection,
    project_id: &str,
    pipeline_id: &str,
) -> Result<CustomPipelineView, PersistenceServiceError> {
    load_views(db, project_id, Some(pipeline_id))?
        .pop()
        .ok_or_else(|| PersistenceServiceError::NotFound("pipeline".into()))
}

pub(super) fn load_step_runs(
    db: &Connection,
    run_id: &str,
) -> Result<Vec<PipelineStepRunView>, PersistenceServiceError> {
    let mut statement = db.prepare_cached(
        "SELECT step_index,attempt,step_id,kind,status,terminal_run_id,llm_execution_id,
                git_operation_id,output_text,verdict_json,gate_decision,gate_feedback,
                started_at,finished_at,error
           FROM custom_pipeline_step_runs WHERE run_id=?1 ORDER BY step_index,attempt",
    )?;
    let rows = statement
        .query_map([run_id], |row| {
            let status: String = row.get(4)?;
            let verdict: Option<String> = row.get(9)?;
            Ok(PipelineStepRunView {
                step_index: row.get(0)?,
                attempt: row.get(1)?,
                step_id: row.get(2)?,
                kind: row.get(3)?,
                status: StepRunStatus::parse(&status).ok_or_else(|| bad_column(4, &status))?,
                terminal_run_id: row.get(5)?,
                llm_execution_id: row.get(6)?,
                git_operation_id: row.get(7)?,
                output_text: row.get(8)?,
                verdict: verdict.and_then(|json| serde_json::from_str(&json).ok()),
                gate_decision: row.get(10)?,
                gate_feedback: row.get(11)?,
                started_at: row.get(12)?,
                finished_at: row.get(13)?,
                error: row.get(14)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

const RUN_SQL: &str = "SELECT id,pipeline_id,version,trigger_kind,status,current_step,error,
        created_at,updated_at,finished_at,resolved_definition_json,source_pulsation_id,
        (SELECT u.feature_name FROM pulsations u WHERE u.id=source_pulsation_id) AS source_title,
        (SELECT u.status FROM pulsations u WHERE u.id=source_pulsation_id) AS source_status,
        trigger_payload_json
   FROM custom_pipeline_runs";

/// `Stage passed · Code` or `Pulsation done`, from a run's payload (M3).
fn payload_trigger_label(payload: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(payload).ok()?;
    let trigger = PipelineTrigger::DefaultEvent {
        event: match value.get("event")?.as_str()? {
            "stage_passed" => crate::custom_pipelines::DefaultEvent::StagePassed,
            "pulsation_done" => crate::custom_pipelines::DefaultEvent::PulsationDone,
            _ => return None,
        },
        stage: value
            .get("stage")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
    };
    Some(crate::custom_pipelines::trigger_label(&trigger))
}

fn map_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<(PipelineRunView, String)> {
    let status: String = row.get(4)?;
    Ok((
        PipelineRunView {
            id: row.get(0)?,
            pipeline_id: row.get(1)?,
            version: row.get(2)?,
            trigger_kind: row.get(3)?,
            status: PipelineRunStatus::parse(&status).ok_or_else(|| bad_column(4, &status))?,
            current_step: row.get(5)?,
            error: row.get(6)?,
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
            finished_at: row.get(9)?,
            definition: CustomPipelineDefinitionV1 {
                schema_version: 0,
                trigger: PipelineTrigger::Manual,
                steps: Vec::new(),
            },
            steps: Vec::new(),
            source_pulsation_id: row.get(11)?,
            source_pulsation_title: row.get(12)?,
            trigger_label: (row.get::<_, String>(3)? == "default_event")
                .then(|| row.get::<_, String>(14))
                .transpose()?
                .and_then(|payload| payload_trigger_label(&payload)),
            source_pulsation_failed: row.get::<_, Option<String>>(13)?.as_deref() == Some("failed"),
        },
        row.get(10)?,
    ))
}

fn complete_runs(
    db: &Connection,
    rows: Vec<(PipelineRunView, String)>,
) -> Result<Vec<PipelineRunView>, PersistenceServiceError> {
    rows.into_iter()
        .map(|(mut run, json)| {
            run.definition = stored_definition(&json)?;
            run.steps = load_step_runs(db, &run.id)?;
            Ok(run)
        })
        .collect()
}

pub(super) fn load_run(
    db: &Connection,
    project_id: &str,
    run_id: &str,
) -> Result<PipelineRunView, PersistenceServiceError> {
    let row = db
        .query_row(
            &format!("{RUN_SQL} WHERE id=?1 AND project_id=?2"),
            params![run_id, project_id],
            map_run,
        )
        .optional()?
        .ok_or_else(|| PersistenceServiceError::NotFound("pipeline run".into()))?;
    Ok(complete_runs(db, vec![row])?.remove(0))
}

pub(super) fn max_concurrent(db: &Connection) -> Result<u32, PersistenceServiceError> {
    let value: Option<String> = db
        .query_row(
            "SELECT value FROM runtime_settings WHERE key=?1",
            [AUTOMATION_MAX_CONCURRENT_KEY],
            |row| row.get(0),
        )
        .optional()?;
    Ok(value
        .and_then(|value| value.trim().parse::<u32>().ok())
        .filter(|value| AUTOMATION_MAX_CONCURRENT_RANGE.contains(value))
        .unwrap_or(AUTOMATION_MAX_CONCURRENT_DEFAULT))
}

/// A pipeline this project shows (its own or a global one): archived flag,
/// current version, name, scope and parent.
pub(super) struct Shown {
    archived: bool,
    current: u32,
    name: String,
    scope: String,
    parent: Option<String>,
}

fn own_pipeline(
    tx: &Transaction<'_>,
    project_id: &str,
    pipeline_id: &str,
) -> Result<Shown, PersistenceServiceError> {
    tx.query_row(
        "SELECT p.archived,p.current_version,p.name,p.scope,p.parent_pipeline_id
           FROM custom_pipelines p
           JOIN custom_pipeline_activations a ON a.pipeline_id=p.id AND a.project_id=?1
          WHERE p.id=?2",
        params![project_id, pipeline_id],
        |row| {
            Ok(Shown {
                archived: row.get(0)?,
                current: row.get(1)?,
                name: row.get(2)?,
                scope: row.get(3)?,
                parent: row.get(4)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| PersistenceServiceError::NotFound("pipeline".into()))
}

/// The limits a box that becomes visible must respect (R4, T9). Archived
/// pipelines do not count, so one being restored is not counted twice.
pub(super) fn limit_errors(
    tx: &Transaction<'_>,
    project_id: &str,
    scope: &str,
) -> Result<Vec<PipelineValidationError>, PersistenceServiceError> {
    let mut errors = Vec::new();
    if scope == "global" {
        let globals: u32 = tx.query_row(
            "SELECT count(*) FROM custom_pipelines WHERE scope='global' AND archived=0",
            [],
            |row| row.get(0),
        )?;
        if globals as usize >= MAX_GLOBAL_PIPELINES {
            errors.push(PipelineValidationError::new(
                "global_limit",
                None,
                format!(
                    "The app has at most {MAX_GLOBAL_PIPELINES} global pipelines. Archive one first."
                ),
            ));
        }
    }
    let own: u32 = tx.query_row(
        "SELECT count(*) FROM custom_pipelines
          WHERE scope='project' AND project_id=?1 AND archived=0",
        [project_id],
        |row| row.get(0),
    )?;
    if scope == "project" && own as usize >= MAX_PROJECT_PIPELINES {
        errors.push(PipelineValidationError::new(
            "project_limit",
            None,
            format!(
                "A project has at most {MAX_PROJECT_PIPELINES} pipelines of its own. Archive one first."
            ),
        ));
    }
    let active: u32 = tx.query_row(
        "SELECT count(*) FROM custom_pipeline_activations a
           JOIN custom_pipelines p ON p.id=a.pipeline_id
          WHERE a.project_id=?1 AND p.archived=0",
        [project_id],
        |row| row.get(0),
    )?;
    if active as usize >= MAX_ACTIVE_PIPELINES {
        errors.push(PipelineValidationError::new(
            "active_limit",
            None,
            format!("A project shows at most {MAX_ACTIVE_PIPELINES} pipelines. Archive one first."),
        ));
    }
    Ok(errors)
}

/// Names are unique among the boxes the project shows (T8).
pub(super) fn name_taken(
    tx: &Transaction<'_>,
    project_id: &str,
    name: &str,
    except: Option<&str>,
) -> Result<bool, PersistenceServiceError> {
    Ok(tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM custom_pipeline_activations a
           JOIN custom_pipelines p ON p.id=a.pipeline_id
          WHERE a.project_id=?1 AND p.archived=0 AND lower(trim(p.name))=lower(trim(?2))
            AND p.id<>COALESCE(?3,''))",
        params![project_id, name, except],
        |row| row.get(0),
    )?)
}

/// Another pipeline with this name in a project that shows `pipeline_id`.
fn name_taken_where_shown(
    tx: &Transaction<'_>,
    pipeline_id: &str,
    name: &str,
) -> Result<bool, PersistenceServiceError> {
    Ok(tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM custom_pipeline_activations mine
           JOIN custom_pipeline_activations a ON a.project_id=mine.project_id
           JOIN custom_pipelines p ON p.id=a.pipeline_id
          WHERE mine.pipeline_id=?1 AND p.id<>?1 AND p.archived=0
            AND lower(trim(p.name))=lower(trim(?2)))",
        params![pipeline_id, name],
        |row| row.get(0),
    )?)
}

pub(super) fn name_taken_error() -> PipelineValidationError {
    PipelineValidationError::new(
        "name_taken",
        None,
        "Another pipeline in this project has this name.",
    )
}

/// Everything a definition must satisfy to be saved or run (Feature 25 M5 enables host tools).
pub(super) fn definition_errors(
    tx: &Transaction<'_>,
    definition: &CustomPipelineDefinitionV1,
) -> Result<Vec<PipelineValidationError>, PersistenceServiceError> {
    let mut errors = validate_definition(definition, true);
    for step in &definition.steps {
        let PipelineStep::Agent(agent) = step else {
            continue;
        };
        if agent.agent.transport != StepTransport::Api {
            continue;
        }
        let Some(route) = agent
            .agent
            .api_route_id
            .as_deref()
            .map(str::trim)
            .filter(|route| !route.is_empty())
        else {
            continue;
        };
        let enabled: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM provider_routes WHERE id=?1 AND enabled=1)",
            [route],
            |row| row.get(0),
        )?;
        if !enabled {
            errors.push(PipelineValidationError::new(
                "api_route_unknown",
                Some(&agent.id),
                "This API route does not exist or is disabled.",
            ));
        }
    }
    Ok(errors)
}

pub(super) fn ensure_project(
    tx: &Transaction<'_>,
    project_id: &str,
) -> Result<(), PersistenceServiceError> {
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1 AND is_archived=0)",
        [project_id],
        |row| row.get(0),
    )?;
    if exists {
        Ok(())
    } else {
        Err(PersistenceServiceError::NotFound("project".into()))
    }
}

/// A run in progress of the pipeline, in one project or in any.
pub(super) fn has_active_run_in(
    tx: &Transaction<'_>,
    pipeline_id: &str,
    project_id: Option<&str>,
) -> Result<bool, PersistenceServiceError> {
    Ok(tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM custom_pipeline_runs
          WHERE pipeline_id=?1 AND (?2 IS NULL OR project_id=?2)
            AND status IN ('queued','running','waiting_gate'))",
        params![pipeline_id, project_id],
        |row| row.get(0),
    )?)
}

/// Opens the step after `from` (or the first one when `from` is `None`), or
/// finishes the run when there is none left.
pub(super) fn open_next_step(
    tx: &Transaction<'_>,
    run_id: &str,
    definition: &CustomPipelineDefinitionV1,
    from: Option<u32>,
    now: &str,
) -> Result<(), PersistenceServiceError> {
    let next = from.map_or(0, |index| index + 1);
    let Some(step) = definition.steps.get(next as usize) else {
        tx.execute(
            "UPDATE custom_pipeline_runs SET status='done',finished_at=?2,updated_at=?2
              WHERE id=?1",
            params![run_id, now],
        )?;
        return Ok(());
    };
    let (step_status, run_status) = match step {
        PipelineStep::Agent(_) | PipelineStep::HostTool(_) => (StepRunStatus::Queued, None),
        PipelineStep::Gate(_) => (
            StepRunStatus::WaitingGate,
            Some(PipelineRunStatus::WaitingGate),
        ),
    };
    let attempt: u32 = tx.query_row(
        "SELECT COALESCE(max(attempt),0)+1 FROM custom_pipeline_step_runs
          WHERE run_id=?1 AND step_index=?2",
        params![run_id, next],
        |row| row.get(0),
    )?;
    tx.execute(
        "INSERT INTO custom_pipeline_step_runs(run_id,step_index,attempt,step_id,kind,status)
         VALUES(?1,?2,?3,?4,?5,?6)",
        params![
            run_id,
            next,
            attempt,
            step.id(),
            step.kind(),
            step_status.as_str()
        ],
    )?;
    // A run keeps `queued` until its first step starts, and `running` while
    // a later agent step waits for the lane.
    tx.execute(
        "UPDATE custom_pipeline_runs
            SET current_step=?2,
                status=COALESCE(?3,CASE WHEN status='waiting_gate' THEN 'running' ELSE status END),
                updated_at=?4
          WHERE id=?1",
        params![run_id, next, run_status.map(PipelineRunStatus::as_str), now],
    )?;
    Ok(())
}

/// What starts a run: `Run` on the box, or an event of the Ikore pipeline
/// (feature 25, M3).
pub(super) enum RunTrigger {
    Manual {
        payload: String,
    },
    DefaultEvent {
        payload: String,
        pulsation_id: String,
        outbox_id: String,
    },
}

/// A new run of the version the project's box adopted, with the definition
/// it resolves to now (T4). `Ok(Err(message))` when the box cannot run.
pub(super) fn insert_run(
    tx: &Transaction<'_>,
    project_id: &str,
    pipeline_id: &str,
    trigger: &RunTrigger,
    now: &str,
) -> Result<Result<String, String>, PersistenceServiceError> {
    let (archived, version, state, parent, parent_version): (
        bool,
        u32,
        String,
        Option<String>,
        Option<u32>,
    ) = tx
        .query_row(
            "SELECT p.archived,a.adopted_version,a.state,p.parent_pipeline_id,
                    a.adopted_parent_version
               FROM custom_pipeline_activations a
               JOIN custom_pipelines p ON p.id=a.pipeline_id
              WHERE a.project_id=?1 AND a.pipeline_id=?2",
            params![project_id, pipeline_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| PersistenceServiceError::NotFound("pipeline".into()))?;
    if archived {
        return Ok(Err("Restore the pipeline before running it.".into()));
    }
    if state != "ready" {
        return Ok(Err("Edit the pipeline before running it.".into()));
    }
    let (definition, conflicts) = resolved_for(
        tx,
        pipeline_id,
        version,
        parent.as_deref().zip(parent_version),
    )?;
    if !conflicts.is_empty() {
        return Ok(Err("Edit the pipeline before running it.".into()));
    }
    if let Some(error) = definition_errors(tx, &definition)?.first() {
        return Ok(Err(error.message.clone()));
    }
    let json = definition_json(&definition)?;
    let sha = crate::exchange::render::sha256_hex(&json);
    let id = Uuid::new_v4().to_string();
    let (kind, payload, pulsation, outbox) = match trigger {
        RunTrigger::Manual { payload } => ("manual", payload.as_str(), None, None),
        RunTrigger::DefaultEvent {
            payload,
            pulsation_id,
            outbox_id,
        } => (
            "default_event",
            payload.as_str(),
            Some(pulsation_id.as_str()),
            Some(outbox_id.as_str()),
        ),
    };
    tx.execute(
        "INSERT INTO custom_pipeline_runs(id,pipeline_id,version,project_id,trigger_kind,
           trigger_payload_json,source_pulsation_id,resolved_definition_json,resolved_sha256,
           status,current_step,created_at,updated_at,trigger_outbox_id)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,'queued',0,?10,?10,?11)",
        params![
            id,
            pipeline_id,
            version,
            project_id,
            kind,
            payload,
            pulsation,
            json,
            sha,
            now,
            outbox
        ],
    )?;
    open_next_step(tx, &id, &definition, None, now)?;
    Ok(Ok(id))
}

impl PersistenceService {
    /// The project's boxes, archived pipelines, recent runs and the lane's
    /// ceiling, for the Agents canvas.
    pub fn custom_pipelines_snapshot(
        &self,
        project_id: &str,
    ) -> Result<CustomPipelinesSnapshot, PersistenceServiceError> {
        let project_id = required(project_id, "Project")?;
        let db = self.connection()?;
        let (archived, pipelines) = load_views(&db, project_id, None)?
            .into_iter()
            .partition::<Vec<_>, _>(|view| view.archived);
        let mut statement = db.prepare(&format!(
            "SELECT * FROM ({RUN_SQL}
                WHERE project_id=?1) r
              WHERE (SELECT count(*) FROM custom_pipeline_runs n
                      WHERE n.project_id=?1 AND n.pipeline_id=r.pipeline_id
                        AND (n.created_at>r.created_at OR (n.created_at=r.created_at AND n.id>r.id)))<?2
              ORDER BY r.created_at DESC,r.id DESC"
        ))?;
        let rows = statement
            .query_map(
                params![project_id, RECENT_RUNS_PER_PIPELINE as u32],
                map_run,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let runs = complete_runs(&db, rows)?;
        let available_globals = db
            .prepare(
                "SELECT p.id,p.name,p.description,p.current_version,
                        (SELECT name FROM custom_pipelines m WHERE m.id=p.parent_pipeline_id)
                   FROM custom_pipelines p
                  WHERE p.scope='global' AND p.archived=0
                    AND NOT EXISTS(SELECT 1 FROM custom_pipeline_activations a
                                    WHERE a.pipeline_id=p.id AND a.project_id=?1)
                  ORDER BY lower(p.name),p.id",
            )?
            .query_map([project_id], |row| {
                Ok(GlobalPipelineSummary {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get(2)?,
                    current_version: row.get(3)?,
                    parent_name: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(CustomPipelinesSnapshot {
            project_id: project_id.to_owned(),
            pipelines,
            archived,
            runs,
            max_concurrent: max_concurrent(&db)?,
            available_globals,
        })
    }

    pub fn custom_pipeline_run(
        &self,
        project_id: &str,
        run_id: &str,
    ) -> Result<PipelineRunView, PersistenceServiceError> {
        load_run(
            &self.connection()?,
            required(project_id, "Project")?,
            required(run_id, "Run")?,
        )
    }

    /// Creates a pipeline, or saves a new version of one. A run in progress
    /// keeps the version it started with (T4).
    pub fn save_custom_pipeline(
        &self,
        request: SaveCustomPipelineRequest,
    ) -> Result<SaveCustomPipelineOutcome, PersistenceServiceError> {
        let project_id = required(&request.project_id, "Project")?;
        let pipeline_id = request
            .pipeline_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty());
        let name = request.name.trim().to_owned();
        let description = request.description.trim().to_owned();
        let mut errors = Vec::new();
        errors.extend(validate_name(&name));
        if description.chars().count() > MAX_DESCRIPTION_CHARS {
            errors.push(PipelineValidationError::new(
                "description_too_long",
                None,
                format!("A description has at most {MAX_DESCRIPTION_CHARS} characters."),
            ));
        }
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_project(&tx, project_id)?;
        errors.extend(definition_errors(&tx, &request.definition)?);
        if !name.is_empty() && name_taken(&tx, project_id, &name, pipeline_id)? {
            errors.push(name_taken_error());
        } else if let Some(id) = pipeline_id
            && !name.is_empty()
            && name_taken_where_shown(&tx, id, &name)?
        {
            errors.push(PipelineValidationError::new(
                "name_taken",
                None,
                "A project that uses this pipeline has another pipeline with this name.",
            ));
        }
        let now = now();
        // A child stores its changes over its parent's current version (P1).
        let parent_id = match pipeline_id {
            Some(id) => own_pipeline(&tx, project_id, id)?.parent,
            None => request
                .parent_pipeline_id
                .as_deref()
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(str::to_owned),
        };
        let mut parent_version = None;
        let json = match &parent_id {
            None => definition_json(&request.definition)?,
            Some(parent) => {
                let (parent_scope, parent_project, parent_current, parent_parent): (
                    String,
                    Option<String>,
                    u32,
                    Option<String>,
                ) = tx
                    .query_row(
                        "SELECT scope,project_id,current_version,parent_pipeline_id
                           FROM custom_pipelines WHERE id=?1",
                        [parent],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                    )
                    .optional()?
                    .ok_or_else(|| PersistenceServiceError::NotFound("parent pipeline".into()))?;
                if pipeline_id.is_none() {
                    if parent_parent.is_some() {
                        errors.push(PipelineValidationError::new(
                            "child_of_child",
                            None,
                            "A pipeline based on another cannot have children of its own.",
                        ));
                    }
                    // P3: a project's pipeline has children only in its project.
                    let scope = request.scope.as_deref().unwrap_or("project");
                    if parent_scope == "project"
                        && (scope != "project" || parent_project.as_deref() != Some(project_id))
                    {
                        errors.push(PipelineValidationError::new(
                            "parent_in_project",
                            None,
                            "A child of a project's pipeline stays in that project.",
                        ));
                    }
                }
                parent_version = Some(parent_current);
                if parent_parent.is_some() {
                    // Its versions hold changes, not a definition to diff against.
                    return Ok(SaveCustomPipelineOutcome::Invalid { errors });
                }
                let parent_definition = resolved_for(&tx, parent, parent_current, None)?.0;
                match child_changes(&parent_definition, &request.definition) {
                    Ok(changes) => serde_json::to_string(&changes)
                        .map_err(|error| invalid(error.to_string()))?,
                    Err(error) => {
                        errors.push(error);
                        String::new()
                    }
                }
            }
        };
        let sha = crate::exchange::render::sha256_hex(&json);
        let id = match pipeline_id {
            None => {
                let scope = match request.scope.as_deref().unwrap_or("project") {
                    "global" => "global",
                    _ => "project",
                };
                errors.extend(limit_errors(&tx, project_id, scope)?);
                if !errors.is_empty() {
                    return Ok(SaveCustomPipelineOutcome::Invalid { errors });
                }
                let id = Uuid::new_v4().to_string();
                tx.execute(
                    "INSERT INTO custom_pipelines(id,scope,project_id,name,description,current_version,
                       archived,created_at,updated_at,parent_pipeline_id)
                     VALUES(?1,?6,CASE WHEN ?6='global' THEN NULL ELSE ?2 END,?3,?4,1,0,?5,?5,?7)",
                    params![id, project_id, name, description, now, scope, parent_id],
                )?;
                tx.execute(
                    "INSERT INTO custom_pipeline_versions(pipeline_id,version,definition_json,
                       definition_sha256,created_at) VALUES(?1,1,?2,?3,?4)",
                    params![id, json, sha, now],
                )?;
                tx.execute(
                    "INSERT INTO custom_pipeline_activations(project_id,pipeline_id,position,
                       adopted_version,adopted_parent_version,state,created_at,updated_at)
                     VALUES(?1,?2,(SELECT COALESCE(max(position)+1,0)
                                     FROM custom_pipeline_activations WHERE project_id=?1),
                            1,?4,'ready',?3,?3)",
                    params![project_id, id, now, parent_version],
                )?;
                id
            }
            Some(id) => {
                let shown = own_pipeline(&tx, project_id, id)?;
                let (archived, current) = (shown.archived, shown.current);
                if archived {
                    errors.push(PipelineValidationError::new(
                        "pipeline_archived",
                        None,
                        "Restore the pipeline before editing it.",
                    ));
                }
                if request.expected_version != Some(current) {
                    errors.push(PipelineValidationError::new(
                        "version_conflict",
                        None,
                        "This pipeline changed since you opened it. Reopen it and try again.",
                    ));
                }
                if !errors.is_empty() {
                    return Ok(SaveCustomPipelineOutcome::Invalid { errors });
                }
                let current_sha: String = tx.query_row(
                    "SELECT definition_sha256 FROM custom_pipeline_versions
                      WHERE pipeline_id=?1 AND version=?2",
                    params![id, current],
                    |row| row.get(0),
                )?;
                // A new version only when the steps or the trigger changed.
                let version = if current_sha == sha {
                    current
                } else {
                    let next = current + 1;
                    tx.execute(
                        "INSERT INTO custom_pipeline_versions(pipeline_id,version,definition_json,
                           definition_sha256,created_at) VALUES(?1,?2,?3,?4,?5)",
                        params![id, next, json, sha, now],
                    )?;
                    next
                };
                tx.execute(
                    "UPDATE custom_pipelines SET name=?2,description=?3,current_version=?4,updated_at=?5
                      WHERE id=?1",
                    params![id, name, description, version, now],
                )?;
                id.to_owned()
            }
        };
        // The project where the edit was made adopts it at once, with the
        // children it shows (P2); the others adopt it when they open.
        super::custom_pipeline_scope::adopt_with_children(&tx, project_id, &id, &now)?;
        tx.commit()?;
        Ok(SaveCustomPipelineOutcome::Saved {
            pipeline: Box::new(load_view(&self.connection()?, project_id, &id)?),
        })
    }

    /// Hides a pipeline's box and keeps its history, or shows it again within
    /// the limits (T9) and with a free name (T8).
    pub fn set_custom_pipeline_archived(
        &self,
        project_id: &str,
        pipeline_id: &str,
        archived: bool,
    ) -> Result<SaveCustomPipelineOutcome, PersistenceServiceError> {
        let project_id = required(project_id, "Project")?;
        let pipeline_id = required(pipeline_id, "Pipeline")?;
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let shown = own_pipeline(&tx, project_id, pipeline_id)?;
        let (was_archived, name) = (shown.archived, shown.name.clone());
        if was_archived != archived {
            // A global pipeline is archived for every project.
            if archived && has_active_run_in(&tx, pipeline_id, None)? {
                return Ok(SaveCustomPipelineOutcome::Invalid {
                    errors: vec![PipelineValidationError::new(
                        "run_in_progress",
                        None,
                        "This pipeline has a run in progress. Cancel it first.",
                    )],
                });
            }
            if !archived {
                let mut errors = limit_errors(&tx, project_id, &shown.scope)?;
                if name_taken(&tx, project_id, &name, Some(pipeline_id))? {
                    errors.push(name_taken_error());
                }
                if !errors.is_empty() {
                    return Ok(SaveCustomPipelineOutcome::Invalid { errors });
                }
            }
            tx.execute(
                "UPDATE custom_pipelines SET archived=?2,updated_at=?3 WHERE id=?1",
                params![pipeline_id, archived, now()],
            )?;
        }
        tx.commit()?;
        Ok(SaveCustomPipelineOutcome::Saved {
            pipeline: Box::new(load_view(&self.connection()?, project_id, pipeline_id)?),
        })
    }

    /// Deletes a pipeline that never ran; one with runs can only be archived.
    pub fn delete_custom_pipeline(
        &self,
        project_id: &str,
        pipeline_id: &str,
    ) -> Result<(), PersistenceServiceError> {
        let project_id = required(project_id, "Project")?;
        let pipeline_id = required(pipeline_id, "Pipeline")?;
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if own_pipeline(&tx, project_id, pipeline_id)?.scope == "global" {
            let elsewhere: u32 = tx.query_row(
                "SELECT count(*) FROM custom_pipeline_activations WHERE pipeline_id=?1 AND project_id<>?2",
                params![pipeline_id, project_id],
                |row| row.get(0),
            )?;
            if elsewhere > 0 {
                return Err(invalid(
                    "Other projects use this pipeline. Remove it from them first.",
                ));
            }
        }
        let children: u32 = tx.query_row(
            "SELECT count(*) FROM custom_pipelines WHERE parent_pipeline_id=?1",
            [pipeline_id],
            |row| row.get(0),
        )?;
        if children > 0 {
            return Err(invalid(
                "Other pipelines are based on this one. Detach or delete them first.",
            ));
        }
        let runs: u32 = tx.query_row(
            "SELECT count(*) FROM custom_pipeline_runs WHERE pipeline_id=?1",
            [pipeline_id],
            |row| row.get(0),
        )?;
        if runs > 0 {
            return Err(invalid(
                "A pipeline with runs can't be deleted. Archive it instead.",
            ));
        }
        tx.execute("DELETE FROM custom_pipelines WHERE id=?1", [pipeline_id])?;
        tx.commit()?;
        Ok(())
    }

    /// `Run` on a box: a run of the version the project adopted.
    pub fn start_custom_pipeline_run(
        &self,
        project_id: &str,
        pipeline_id: &str,
    ) -> Result<String, PersistenceServiceError> {
        let project_id = required(project_id, "Project")?;
        let pipeline_id = required(pipeline_id, "Pipeline")?;
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_project(&tx, project_id)?;
        let now = now();
        let payload = serde_json::json!({"kind": "manual", "requestedAt": now}).to_string();
        let id = insert_run(
            &tx,
            project_id,
            pipeline_id,
            &RunTrigger::Manual { payload },
            &now,
        )?
        .map_err(invalid)?;
        tx.commit()?;
        self.runtime_control.wakeup.notify_waiters();
        Ok(id)
    }

    /// `Cancel`: the run stops, and a running step's process is stopped.
    pub fn cancel_custom_pipeline_run(
        &self,
        project_id: &str,
        run_id: &str,
    ) -> Result<(), PersistenceServiceError> {
        let project_id = required(project_id, "Project")?;
        let run_id = required(run_id, "Run")?;
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let status: String = tx
            .query_row(
                "SELECT status FROM custom_pipeline_runs WHERE id=?1 AND project_id=?2",
                params![run_id, project_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| PersistenceServiceError::NotFound("pipeline run".into()))?;
        let status = PipelineRunStatus::parse(&status)
            .ok_or_else(|| PersistenceServiceError::Runtime(format!("run status `{status}`")))?;
        if !status.is_active() && status != PipelineRunStatus::Interrupted {
            return Err(invalid("This run already finished."));
        }
        let now = now();
        tx.execute(
            "UPDATE custom_pipeline_runs SET status='cancelled',finished_at=?2,updated_at=?2
              WHERE id=?1",
            params![run_id, now],
        )?;
        // A running step is closed by the lane when its process stops.
        tx.execute(
            "UPDATE custom_pipeline_step_runs SET status='cancelled',finished_at=?2
              WHERE run_id=?1 AND status IN ('queued','waiting_gate')",
            params![run_id, now],
        )?;
        tx.commit()?;
        if let Ok(tokens) = self.runtime_control.cancellations.lock()
            && let Some(token) = tokens.get(&automation_cancellation_key(run_id))
        {
            token.cancel();
        }
        self.runtime_control.wakeup.notify_waiters();
        Ok(())
    }

    /// `Resume` on a run the app's closing interrupted: its step runs again,
    /// as a new attempt.
    pub fn resume_custom_pipeline_run(
        &self,
        project_id: &str,
        run_id: &str,
    ) -> Result<(), PersistenceServiceError> {
        let project_id = required(project_id, "Project")?;
        let run_id = required(run_id, "Run")?;
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (status, current, json): (String, u32, String) = tx
            .query_row(
                "SELECT status,current_step,resolved_definition_json FROM custom_pipeline_runs
                  WHERE id=?1 AND project_id=?2",
                params![run_id, project_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?
            .ok_or_else(|| PersistenceServiceError::NotFound("pipeline run".into()))?;
        if status != PipelineRunStatus::Interrupted.as_str() {
            return Err(invalid("Only an interrupted run can be resumed."));
        }
        let definition = stored_definition(&json)?;
        let now = now();
        tx.execute(
            "UPDATE custom_pipeline_runs SET status='queued',error=NULL,updated_at=?2 WHERE id=?1",
            params![run_id, now],
        )?;
        open_next_step(&tx, run_id, &definition, current.checked_sub(1), &now)?;
        tx.commit()?;
        self.runtime_control.wakeup.notify_waiters();
        Ok(())
    }

    /// Approves or rejects the gate a run waits at. A rejection ends the run.
    pub fn decide_custom_pipeline_gate(
        &self,
        request: DecidePipelineGateRequest,
    ) -> Result<(), PersistenceServiceError> {
        let project_id = required(&request.project_id, "Project")?;
        let run_id = required(&request.run_id, "Run")?;
        let note = request
            .note
            .as_deref()
            .map(str::trim)
            .filter(|note| !note.is_empty());
        if note.is_some_and(|note| note.len() > MAX_GATE_NOTE_BYTES) {
            return Err(invalid("A gate note has at most 2 KiB."));
        }
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (status, current, json): (String, u32, String) = tx
            .query_row(
                "SELECT status,current_step,resolved_definition_json FROM custom_pipeline_runs
                  WHERE id=?1 AND project_id=?2",
                params![run_id, project_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?
            .ok_or_else(|| PersistenceServiceError::NotFound("pipeline run".into()))?;
        if status != PipelineRunStatus::WaitingGate.as_str() || current != request.step_index {
            return Err(invalid("This run is no longer waiting at this gate."));
        }
        let definition = stored_definition(&json)?;
        let Some(PipelineStep::Gate(gate)) = definition.steps.get(current as usize) else {
            return Err(invalid("This step is not a gate."));
        };
        let now = now();
        let changed = tx.execute(
            "UPDATE custom_pipeline_step_runs
                SET status=?3,gate_decision=?4,gate_feedback=?5,finished_at=?6
              WHERE run_id=?1 AND step_index=?2 AND status='waiting_gate'",
            params![
                run_id,
                current,
                if request.approve {
                    "passed"
                } else {
                    "rejected"
                },
                if request.approve {
                    "approved"
                } else {
                    "rejected"
                },
                note,
                now
            ],
        )?;
        if changed != 1 {
            return Err(invalid("This run is no longer waiting at this gate."));
        }
        if request.approve {
            open_next_step(&tx, run_id, &definition, Some(current), &now)?;
        } else {
            tx.execute(
                "UPDATE custom_pipeline_runs SET status='failed',error=?2,finished_at=?3,updated_at=?3
                  WHERE id=?1",
                params![run_id, format!("Rejected at {}.", gate.label.trim()), now],
            )?;
        }
        tx.commit()?;
        self.runtime_control.wakeup.notify_waiters();
        Ok(())
    }

    /// This project's own trigger for a global pipeline (R4b, M3), or back to
    /// the pipeline's own. It is not a new version, and a run in progress
    /// keeps what started it.
    pub fn set_custom_pipeline_trigger(
        &self,
        request: SetPipelineTriggerRequest,
    ) -> Result<SaveCustomPipelineOutcome, PersistenceServiceError> {
        let project_id = required(&request.project_id, "Project")?;
        let pipeline_id = required(&request.pipeline_id, "Pipeline")?;
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let shown = own_pipeline(&tx, project_id, pipeline_id)?;
        let refuse = |code: &str, message: &str| {
            Ok(SaveCustomPipelineOutcome::Invalid {
                errors: vec![PipelineValidationError::new(code, None, message)],
            })
        };
        if let Some(trigger) = &request.trigger {
            if shown.scope != "global" {
                return refuse(
                    "trigger_not_global",
                    "Only a global pipeline has a trigger of this project. Edit the pipeline instead.",
                );
            }
            let errors = validate_trigger(trigger);
            if !errors.is_empty() {
                return Ok(SaveCustomPipelineOutcome::Invalid { errors });
            }
        }
        if shown.archived {
            return refuse(
                "pipeline_archived",
                "Restore the pipeline before changing its trigger.",
            );
        }
        let view = load_view(&tx, project_id, pipeline_id)?;
        let effective_trigger = request
            .trigger
            .clone()
            .unwrap_or_else(|| view.definition.trigger.clone());
        let mut temp_def = view.definition;
        temp_def.trigger = effective_trigger;
        let def_errs = validate_definition(&temp_def, true);
        if !def_errs.is_empty() {
            return Ok(SaveCustomPipelineOutcome::Invalid { errors: def_errs });
        }
        let json = request
            .trigger
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|error| invalid(error.to_string()))?;
        tx.execute(
            "UPDATE custom_pipeline_activations SET trigger_override_json=?3,updated_at=?4
              WHERE project_id=?1 AND pipeline_id=?2",
            params![project_id, pipeline_id, json, now()],
        )?;
        tx.commit()?;
        Ok(SaveCustomPipelineOutcome::Saved {
            pipeline: Box::new(load_view(&self.connection()?, project_id, pipeline_id)?),
        })
    }

    /// Settings › Composition: how many steps and verifiers the automation
    /// lane runs at once in the whole app (1 to 3).
    pub fn set_automation_max_concurrent(&self, value: u32) -> Result<(), PersistenceServiceError> {
        if !AUTOMATION_MAX_CONCURRENT_RANGE.contains(&value) {
            return Err(invalid("Choose 1, 2 or 3 at a time."));
        }
        self.connection()?.execute(
            "INSERT INTO runtime_settings(key,value,updated_at) VALUES(?1,?2,?3)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at",
            params![AUTOMATION_MAX_CONCURRENT_KEY, value.to_string(), now()],
        )?;
        self.runtime_control.wakeup.notify_waiters();
        Ok(())
    }
}

/// The key of a run's token in the jobs' cancellation registry.
pub(super) fn automation_cancellation_key(run_id: &str) -> String {
    format!("automation:{run_id}")
}
