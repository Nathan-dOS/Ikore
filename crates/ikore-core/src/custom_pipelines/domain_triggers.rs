//! Feature 27 (Camada C): transactional domain trigger outbox and dispatcher.
//!
//! Domain subsystems (`feature_forge`, `companion_notes`, `doc_reconciliation`,
//! and checkpoint opening) write rows into `domain_trigger_outbox` inside
//! their own SQLite transactions only when a ready pipeline box in that
//! project listens to the event. The automation lane dispatches pending rows in
//! FIFO order and enforces idempotency (`custom_pipeline_runs_once_per_domain_event`).

use rusqlite::{Connection, TransactionBehavior, params};
use serde_json::Value;
use uuid::Uuid;

use super::custom_pipeline_store::{RunTrigger, insert_run, now};
use super::custom_pipeline_triggers::listeners;
use super::{PersistenceService, PersistenceServiceError};
use crate::custom_pipelines::{DomainEvent, PipelineTrigger};

const DISPATCH_BATCH: u32 = 32;
pub(crate) const DISCARD_NO_READY_LISTENERS: &str = "no_ready_listeners";

fn trigger_matches(trigger: &PipelineTrigger, event: DomainEvent, stage: Option<&str>) -> bool {
    match trigger {
        PipelineTrigger::DomainEvent {
            event: configured,
            stage: configured_stage,
        } if *configured == event => {
            configured_stage.is_none() || configured_stage.as_deref() == stage
        }
        _ => false,
    }
}

impl PersistenceService {
    /// Inserts a `pending` row into `domain_trigger_outbox` inside the caller's
    /// transaction when at least one ready box in `project_id` listens to the
    /// event. Returns `true` when an event row was queued.
    pub(crate) fn record_domain_event_in_tx(
        tx: &Connection,
        project_id: &str,
        event: DomainEvent,
        stage: Option<&str>,
        subject_id: Option<&str>,
        payload: Value,
        at: &str,
    ) -> Result<bool, PersistenceServiceError> {
        if !listeners(tx, project_id)?
            .iter()
            .any(|(_, trigger)| trigger_matches(trigger, event, stage))
        {
            return Ok(false);
        }
        let payload_json = serde_json::to_string(&payload)
            .map_err(|error| PersistenceServiceError::Runtime(error.to_string()))?;
        tx.execute(
            "INSERT INTO domain_trigger_outbox(
               id, project_id, event, stage, subject_id, payload_json, state, created_at
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7)",
            params![
                Uuid::new_v4().to_string(),
                project_id,
                event.as_str(),
                stage,
                subject_id,
                payload_json,
                at,
            ],
        )?;
        Ok(true)
    }

    /// Dispatches pending `domain_trigger_outbox` rows into queued pipeline runs.
    /// Returns the number of newly started runs.
    pub(crate) fn dispatch_domain_triggers(&self) -> Result<usize, PersistenceServiceError> {
        let mut db = self.connection()?;
        let pending = db
            .prepare(
                "SELECT id, project_id, event, stage, payload_json
                   FROM domain_trigger_outbox
                  WHERE state='pending'
                  ORDER BY created_at, id
                  LIMIT ?1",
            )?
            .query_map([DISPATCH_BATCH], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        if pending.is_empty() {
            return Ok(0);
        }

        let mut started = 0usize;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let at = now();
        for (outbox_id, project_id, event_raw, stage, payload) in pending {
            let state: Option<String> = tx
                .query_row(
                    "SELECT state FROM domain_trigger_outbox WHERE id=?1",
                    [&outbox_id],
                    |row| row.get(0),
                )
                .ok();
            if state.as_deref() != Some("pending") {
                continue;
            }
            let Some(event) = DomainEvent::parse(&event_raw) else {
                tx.execute(
                    "UPDATE domain_trigger_outbox
                        SET state='discarded', discard_reason='unknown_event', dispatched_at=?2
                      WHERE id=?1",
                    params![outbox_id, at],
                )?;
                continue;
            };
            let matching: Vec<String> = listeners(&tx, &project_id)?
                .into_iter()
                .filter(|(_, trigger)| trigger_matches(trigger, event, stage.as_deref()))
                .map(|(pipeline_id, _)| pipeline_id)
                .collect();
            if matching.is_empty() {
                tx.execute(
                    "UPDATE domain_trigger_outbox
                        SET state='discarded', discard_reason=?2, dispatched_at=?3
                      WHERE id=?1",
                    params![outbox_id, DISCARD_NO_READY_LISTENERS, at],
                )?;
                continue;
            }
            let trigger = RunTrigger::DomainEvent {
                payload,
                outbox_id: outbox_id.clone(),
            };
            let mut started_any = false;
            for pipeline_id in matching {
                let already: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM custom_pipeline_runs WHERE pipeline_id=?1 AND domain_event_id=?2)",
                    params![pipeline_id, outbox_id],
                    |row| row.get(0),
                )?;
                if already {
                    started_any = true;
                    continue;
                }
                if insert_run(&tx, &project_id, &pipeline_id, &trigger, &at)?.is_ok() {
                    started += 1;
                    started_any = true;
                }
            }
            let (final_state, reason) = if started_any {
                ("dispatched", None)
            } else {
                ("discarded", Some(DISCARD_NO_READY_LISTENERS))
            };
            tx.execute(
                "UPDATE domain_trigger_outbox
                    SET state=?2, discard_reason=?3, dispatched_at=?4
                  WHERE id=?1",
                params![outbox_id, final_state, reason, at],
            )?;
        }
        tx.commit()?;
        if started > 0 {
            self.runtime_control.wakeup.notify_waiters();
        }
        Ok(started)
    }
}
