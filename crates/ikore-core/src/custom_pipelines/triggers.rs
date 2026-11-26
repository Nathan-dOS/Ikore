//! Feature 25 (M3): events of the Ikore pipeline start the developer's own
//! pipelines. A module of `persistence_service`.
//!
//! `transition_pulsation` records an event in `pipeline_trigger_outbox`, in
//! its own transaction, when a box of the project listens to it; that is the
//! only change in the Ikore pipeline's flow (P1). The automation lane
//! dispatches the event once the publications of the stage that passed are
//! applied, and discards it with the reason when the pulsation failed or the
//! publications were cancelled first (plan R17, T6). A dispatched event
//! starts every ready box that listens to it, once (`custom_pipeline_runs_once_per_event`).

use std::collections::HashMap;

use ikore_contracts::stages::PulsationStage;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::{Value, json};
use uuid::Uuid;

use super::custom_pipeline_store::{RunTrigger, insert_run, now, resolved_for};
use super::{PersistenceService, PersistenceServiceError, Pulsation, PulsationStatus};
use crate::custom_pipelines::{DefaultEvent, PipelineTrigger, STAGE_VALUES};

/// Files a trigger lists; the rest is counted in `filesOmitted`.
pub const MAX_TRIGGER_FILES: usize = 200;
/// Pending events the lane looks at per pass, oldest first.
const DISPATCH_BATCH: u32 = 32;

/// Why an event started nothing (`pipeline_trigger_outbox.discard_reason`).
pub const DISCARD_SOURCE_FAILED: &str = "source_pulsation_failed";
pub const DISCARD_PUBLICATION_CONFLICT: &str = "publication_conflict";
pub const DISCARD_PUBLICATIONS_CANCELLED: &str = "publications_cancelled";
pub const DISCARD_PAYLOAD_UNAVAILABLE: &str = "payload_unavailable";

/// An event of the Ikore pipeline a pipeline can listen to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TriggerEvent {
    pub(crate) event: DefaultEvent,
    /// The stage that passed; `None` on `Pulsation done`.
    pub(crate) stage: Option<PulsationStage>,
}

impl TriggerEvent {
    fn event_value(self) -> &'static str {
        match self.event {
            DefaultEvent::StagePassed => "stage_passed",
            DefaultEvent::PulsationDone => "pulsation_done",
        }
    }

    fn parse(event: &str, stage: Option<&str>) -> Option<Self> {
        Some(match event {
            "stage_passed" => Self {
                event: DefaultEvent::StagePassed,
                stage: Some(PulsationStage::from_db_value(stage?)?),
            },
            "pulsation_done" => Self {
                event: DefaultEvent::PulsationDone,
                stage: None,
            },
            _ => return None,
        })
    }

    /// Whether a pipeline with this trigger listens to the event.
    pub(crate) fn starts(self, trigger: &PipelineTrigger) -> bool {
        match trigger {
            PipelineTrigger::Manual | PipelineTrigger::DomainEvent { .. } => false,
            PipelineTrigger::DefaultEvent { event, stage } => {
                *event == self.event
                    && stage.as_deref() == self.stage.map(PulsationStage::as_db_value)
            }
        }
    }
}

/// The event a transition is, if any (feature 25, section 5): the pulsation
/// left a stage for the next one (`stage_auto_advanced`, a checkpoint's
/// approval, the path confirmation that lets Specification run), or it is
/// done. An approved Integrator is `Pulsation done`, never a passed stage.
pub(crate) fn event_of(
    current_stage: PulsationStage,
    next_status: PulsationStatus,
    next_stage: PulsationStage,
) -> Option<TriggerEvent> {
    match next_status {
        PulsationStatus::Done => Some(TriggerEvent {
            event: DefaultEvent::PulsationDone,
            stage: None,
        }),
        PulsationStatus::Failed => None,
        _ if current_stage.next() == Some(next_stage) => Some(TriggerEvent {
            event: DefaultEvent::StagePassed,
            stage: Some(current_stage),
        }),
        _ => None,
    }
}

/// The trigger a project's box follows: the project's own for a global
/// pipeline (R4b), else the one its adopted version resolves to.
pub(super) fn effective_trigger(
    db: &Connection,
    pipeline_id: &str,
    version: u32,
    parent: Option<(&str, u32)>,
    trigger_override: Option<&str>,
) -> Option<PipelineTrigger> {
    if let Some(json) = trigger_override {
        return serde_json::from_str(json).ok();
    }
    resolved_for(db, pipeline_id, version, parent)
        .ok()
        .map(|(definition, _)| definition.trigger)
}

/// The ready, visible boxes of a project and the trigger each follows. A box
/// whose definition cannot be read listens to nothing: a trigger never
/// breaks the Ikore pipeline's transition.
pub(super) fn listeners(
    db: &Connection,
    project_id: &str,
) -> Result<Vec<(String, PipelineTrigger)>, PersistenceServiceError> {
    let mut statement = db.prepare_cached(
        "SELECT a.pipeline_id,a.adopted_version,p.parent_pipeline_id,a.adopted_parent_version,
                a.trigger_override_json
           FROM custom_pipeline_activations a
           JOIN custom_pipelines p ON p.id=a.pipeline_id
          WHERE a.project_id=?1 AND p.archived=0 AND a.state='ready'
          ORDER BY a.position,a.pipeline_id",
    )?;
    let rows = statement
        .query_map([project_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<u32>>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        .filter_map(|(id, version, parent, parent_version, trigger_override)| {
            let trigger = effective_trigger(
                db,
                &id,
                version,
                parent.as_deref().zip(parent_version),
                trigger_override.as_deref(),
            )?;
            (!matches!(trigger, PipelineTrigger::Manual)).then_some((id, trigger))
        })
        .collect())
}

/// Called by `transition_pulsation` after its `pulse_events` row: records the
/// event when a box of the project listens to it, in the same transaction.
pub(super) fn record_pipeline_trigger(
    tx: &Connection,
    current: &Pulsation,
    next_status: PulsationStatus,
    next_stage: PulsationStage,
    state_version: u64,
    now: &str,
) -> Result<(), PersistenceServiceError> {
    let Some(event) = event_of(current.stage, next_status, next_stage) else {
        return Ok(());
    };
    if !listeners(tx, &current.project_id)?
        .iter()
        .any(|(_, trigger)| event.starts(trigger))
    {
        return Ok(());
    }
    tx.execute(
        "INSERT INTO pipeline_trigger_outbox(id,project_id,pulsation_id,event,stage,state_version,
           state,created_at)
         VALUES(?1,?2,?3,?4,?5,?6,'pending',?7)",
        params![
            Uuid::new_v4().to_string(),
            current.project_id,
            current.id,
            event.event_value(),
            event.stage.map(PulsationStage::as_db_value),
            state_version,
            now
        ],
    )?;
    Ok(())
}

/// A pending row of the outbox.
struct PendingEvent {
    id: String,
    project_id: String,
    pulsation_id: String,
    event: TriggerEvent,
    state_version: u64,
}

enum Readiness {
    Wait,
    Ready,
    Discard(&'static str),
}

/// Whether the stage's publications are applied (R17): those of the stage
/// that passed, from its last acceptance before the event.
fn readiness(
    db: &Connection,
    pending: &PendingEvent,
) -> Result<Readiness, PersistenceServiceError> {
    let status: Option<String> = db
        .query_row(
            "SELECT status FROM pulsations WHERE id=?1",
            [&pending.pulsation_id],
            |row| row.get(0),
        )
        .optional()?;
    let failed = status.as_deref() == Some(PulsationStatus::Failed.as_db_value());
    let Some(stage) = pending.event.stage else {
        // `complete_integrations` releases the base only with every
        // publication applied.
        return Ok(Readiness::Ready);
    };
    let states = db
        .prepare_cached(
            "SELECT o.state FROM artifact_publications o
               JOIN artifacts a ON a.id=o.artifact_id
              WHERE a.pulsation_id=?1 AND a.producer_stage=?2
                AND a.producer_state_version=(
                  SELECT max(producer_state_version) FROM artifacts
                   WHERE pulsation_id=?1 AND producer_stage=?2 AND producer_state_version<?3)",
        )?
        .query_map(
            params![
                pending.pulsation_id,
                stage.as_db_value(),
                pending.state_version
            ],
            |row| row.get::<_, String>(0),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    let any = |wanted: &[&str]| states.iter().any(|state| wanted.contains(&state.as_str()));
    Ok(if any(&["pending", "applying"]) {
        if failed {
            Readiness::Discard(DISCARD_SOURCE_FAILED)
        } else {
            Readiness::Wait
        }
    } else if any(&["conflict"]) {
        Readiness::Discard(DISCARD_PUBLICATION_CONFLICT)
    } else if any(&["cancelled"]) {
        Readiness::Discard(if failed {
            DISCARD_SOURCE_FAILED
        } else {
            DISCARD_PUBLICATIONS_CANCELLED
        })
    } else {
        Readiness::Ready
    })
}

/// What a run started by the event receives, immutable (feature 25, section
/// 5): the pulsation, the event, the batch's tasks and the files the
/// pulsation published until then (path, sha256, stage and tasks), never
/// their content. Sanitized like model text (ADR-030).
pub(crate) fn trigger_payload(
    db: &Connection,
    pulsation_id: &str,
    event: TriggerEvent,
    state_version: u64,
    now: &str,
) -> Result<String, PersistenceServiceError> {
    let title: String = db.query_row(
        "SELECT feature_name FROM pulsations WHERE id=?1",
        [pulsation_id],
        |row| row.get(0),
    )?;
    let tasks = db
        .prepare_cached(
            "SELECT json_extract(t.value,'$.id'),json_extract(t.value,'$.draft.title')
               FROM pulsation_batches b, json_each(b.manifest_json,'$.tasks') t
              WHERE b.pulsation_id=?1 ORDER BY t.key",
        )?
        .query_map([pulsation_id], |row| {
            Ok(json!({
                "id": row.get::<_, Option<String>>(0)?,
                "title": row.get::<_, Option<String>>(1)?,
            }))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let stages = STAGE_VALUES
        .iter()
        .map(|stage| format!("'{stage}'"))
        .collect::<Vec<_>>()
        .join(",");
    let rows = db
        .prepare(&format!(
            "SELECT a.relative_path,o.desired_sha256,a.producer_stage,
                    (SELECT json_group_array(e.task_id) FROM task_evidence e
                      WHERE e.job_id=a.producer_job_id AND e.generation=a.generation
                        AND e.stage=a.producer_stage
                        AND EXISTS(SELECT 1 FROM json_each(e.evidence_json,'$.artifactPositions') x
                                    WHERE x.value=a.output_position))
               FROM artifacts a JOIN artifact_publications o ON o.artifact_id=a.id
              WHERE a.pulsation_id=?1 AND a.acceptance='accepted' AND o.state='applied'
                AND o.removed_at IS NULL AND a.relative_path IS NOT NULL
                AND a.producer_state_version<?2 AND a.producer_stage IN ({stages})
              ORDER BY a.producer_state_version,a.output_position,a.id"
        ))?
        .query_map(params![pulsation_id, state_version], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    // A later stage that writes a path again replaces its entry.
    let mut files: Vec<Value> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for (path, sha256, stage, task_ids) in rows {
        let entry = json!({
            "path": path,
            "sha256": sha256,
            "stage": stage,
            "tasks": task_ids
                .and_then(|json| serde_json::from_str::<Value>(&json).ok())
                .unwrap_or_else(|| json!([])),
        });
        match index.get(&path.to_lowercase()) {
            Some(&at) => files[at] = entry,
            None => {
                index.insert(path.to_lowercase(), files.len());
                files.push(entry);
            }
        }
    }
    let omitted = files.len().saturating_sub(MAX_TRIGGER_FILES);
    files.truncate(MAX_TRIGGER_FILES);
    let payload = json!({
        "kind": "default_event",
        "event": event.event_value(),
        "stage": event.stage.map(PulsationStage::as_db_value),
        "pulsation": { "id": pulsation_id, "title": title },
        "stateVersion": state_version,
        "tasks": tasks,
        "files": files,
        "filesOmitted": omitted,
        "dispatchedAt": now,
    });
    crate::llm::adapter::sanitize_model_text(&payload.to_string())
        .map_err(|error| PersistenceServiceError::Runtime(error.to_string()))
}

impl PersistenceService {
    /// The lane's dispatcher: each pending event whose publications are
    /// applied starts the ready boxes that listen to it, and an event the
    /// pulsation left behind is discarded with the reason. `true` when a run
    /// was created.
    pub(super) fn dispatch_pipeline_triggers(&self) -> Result<bool, PersistenceServiceError> {
        let mut db = self.connection()?;
        let any: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM pipeline_trigger_outbox WHERE state='pending')",
            [],
            |row| row.get(0),
        )?;
        if !any {
            return Ok(false);
        }
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let pending = tx
            .prepare(
                "SELECT id,project_id,pulsation_id,event,stage,state_version
                   FROM pipeline_trigger_outbox WHERE state='pending'
                  ORDER BY created_at,id LIMIT ?1",
            )?
            .query_map([DISPATCH_BATCH], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, u64>(5)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let now = now();
        let discard = |id: &str, reason: &str| {
            tx.execute(
                "UPDATE pipeline_trigger_outbox
                    SET state='discarded',discard_reason=?2,discarded_at=?3 WHERE id=?1",
                params![id, reason, now],
            )
        };
        let mut started = false;
        for (id, project_id, pulsation_id, event, stage, state_version) in pending {
            let Some(event) = TriggerEvent::parse(&event, stage.as_deref()) else {
                discard(&id, DISCARD_PAYLOAD_UNAVAILABLE)?;
                continue;
            };
            let pending = PendingEvent {
                id,
                project_id,
                pulsation_id,
                event,
                state_version,
            };
            match readiness(&tx, &pending)? {
                Readiness::Wait => continue,
                Readiness::Discard(reason) => {
                    discard(&pending.id, reason)?;
                    continue;
                }
                Readiness::Ready => {}
            }
            let Ok(payload) = trigger_payload(
                &tx,
                &pending.pulsation_id,
                pending.event,
                pending.state_version,
                &now,
            ) else {
                discard(&pending.id, DISCARD_PAYLOAD_UNAVAILABLE)?;
                continue;
            };
            for (pipeline_id, trigger) in listeners(&tx, &pending.project_id)? {
                if !pending.event.starts(&trigger) {
                    continue;
                }
                // A box that cannot run now starts nothing (Nathan, M3).
                let run = insert_run(
                    &tx,
                    &pending.project_id,
                    &pipeline_id,
                    &RunTrigger::DefaultEvent {
                        payload: payload.clone(),
                        pulsation_id: pending.pulsation_id.clone(),
                        outbox_id: pending.id.clone(),
                    },
                    &now,
                )?;
                started |= run.is_ok();
            }
            tx.execute(
                "UPDATE pipeline_trigger_outbox SET state='dispatched',dispatched_at=?2 WHERE id=?1",
                params![pending.id, now],
            )?;
        }
        tx.commit()?;
        if started {
            self.runtime_control.wakeup.notify_waiters();
        }
        Ok(started)
    }
}
