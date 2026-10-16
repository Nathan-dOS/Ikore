//! The Visualizer lane (ADR-027 part 4): optional notes on an artifact
//! checkpoint's Review Brief, written by an agent through a closed contract
//! the host validates.
//!
//! It never blocks the checkpoint. The deterministic brief is complete
//! without it; an answer that fails validation gets one corrective call and is
//! then omitted with an activity line; a decision on the checkpoint ends the
//! series and keeps the note that was available with the decision.
use super::*;
use crate::llm::{
    AgentExecutionTarget, AgentRole, LlmError, LlmErrorKind, LlmGateway, LlmRequest, ResponseFormat,
};
use crate::orchestration::review_brief::{
    self, CheckpointVisualization, ReviewBriefV1, VisualizationState, VisualizerInputV1,
};
use coordination::invalid;
use indexing::BackgroundClaim;
use rusqlite::TransactionBehavior;

/// Calls one checkpoint may spend: the answer and one correction.
const MAX_CALLS: u32 = 2;
/// The note is short; the cap keeps a runaway answer cheap.
const MAX_OUTPUT_TOKENS: u32 = 600;
/// The stage name routing bindings use for the Visualizer.
const ROUTING_STAGE: &str = "Review";

/// One attempt's delivered input, and the reason the previous answer was
/// rejected when this is the corrective call.
struct VisualizationAttempt {
    input: VisualizerInputV1,
    previous_rejection: Option<String>,
    pulsation_id: String,
    project_id: String,
}

/// A `checkpoint_visualizations` row as Review reads it.
struct StoredVisualization {
    state: String,
    input: Option<String>,
    brief: Option<String>,
    diagnostic: Option<String>,
}

impl PersistenceService {
    /// Requests a note on the artifact checkpoint the pulsation just opened,
    /// when its project enabled the Visualizer and the pipeline has typed
    /// payloads (v4). Runs inside the transaction that opened the checkpoint.
    pub(super) fn enqueue_visualization(
        db: &Connection,
        pulse: &str,
    ) -> Result<(), PersistenceServiceError> {
        let opened: Option<(String, String, u64, u64)> = db
            .query_row(
                "SELECT c.id,p.project_id,p.input_revision,p.state_version FROM checkpoints c
                 JOIN pulsations p ON p.id=c.pulsation_id
                 WHERE c.pulsation_id=?1 AND c.status='waiting' AND c.purpose='artifact_review'",
                [pulse],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let Some((checkpoint, project, input_revision, state_version)) = opened else {
            return Ok(());
        };
        if !Self::composition_settings_for(db, &project)?.visualizer_enabled
            || !Self::composed_pulsation(db, pulse)?
        {
            return Ok(());
        }
        let job = Uuid::new_v4().to_string();
        let now = Self::now_iso();
        let inserted = db.execute(
            "INSERT OR IGNORE INTO jobs(id,pulsation_id,kind,status,attempts,created_at,updated_at,project_id,input_revision,state_version,job_class,idempotency_key)
             VALUES(?1,?2,'visualize_checkpoint','pending',0,?3,?3,?4,?5,?6,'background',?7)",
            params![
                job,
                pulse,
                now,
                project,
                input_revision,
                state_version,
                format!("{checkpoint}:visualize")
            ],
        )?;
        if inserted == 1 {
            db.execute(
                "INSERT INTO checkpoint_visualizations(checkpoint_id,pulsation_id,job_id,state,created_at,updated_at)
                 VALUES(?1,?2,?3,'pending',?4,?4)",
                params![checkpoint, pulse, job, now],
            )?;
        }
        Ok(())
    }

    /// Ends the note series of a decided checkpoint. The validated note, if
    /// any, is kept with the decision so the text the developer saw stays
    /// auditable; a pending call is cancelled. Returns the job to cancel.
    pub(super) fn close_visualization(
        db: &Connection,
        checkpoint_id: &str,
    ) -> Result<Option<String>, PersistenceServiceError> {
        db.execute(
            "UPDATE checkpoints SET visualizer_brief_json=(
               SELECT brief_json FROM checkpoint_visualizations WHERE checkpoint_id=?1 AND state='validated'
             ) WHERE id=?1",
            [checkpoint_id],
        )?;
        let open: Option<String> = db
            .query_row(
                "SELECT job_id FROM checkpoint_visualizations WHERE checkpoint_id=?1 AND state IN ('pending','running')",
                [checkpoint_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(job) = open else {
            return Ok(None);
        };
        let now = Self::now_iso();
        db.execute(
            "UPDATE checkpoint_visualizations SET state='skipped',diagnostic='The checkpoint was decided first.',updated_at=?2 WHERE checkpoint_id=?1",
            params![checkpoint_id, now],
        )?;
        db.execute(
            "UPDATE jobs SET status='failed',cancellation_reason='superseded',finished_at=?2,updated_at=?2
             WHERE id=?1 AND status IN ('pending','running','interrupted')",
            params![job, now],
        )?;
        Ok(Some(job))
    }

    /// The Visualizer's note on a still-current review checkpoint, validated
    /// in the same snapshot as the admission check. `None` when no note was
    /// requested, or the series ended without one before a decision.
    pub fn read_checkpoint_visualization(
        &self,
        checkpoint: &HitlCheckpoint,
    ) -> Result<Option<CheckpointVisualization>, PersistenceServiceError> {
        let mut db = self.connection()?;
        let tx = db.transaction()?;
        serial::require_current_review(&tx, checkpoint)?;
        let row = tx
            .query_row(
                "SELECT state,input_json,brief_json,diagnostic FROM checkpoint_visualizations WHERE checkpoint_id=?1",
                [&checkpoint.checkpoint_id],
                |row| {
                    Ok(StoredVisualization {
                        state: row.get(0)?,
                        input: row.get(1)?,
                        brief: row.get(2)?,
                        diagnostic: row.get(3)?,
                    })
                },
            )
            .optional()?;
        let Some(StoredVisualization {
            state,
            input,
            brief,
            diagnostic,
        }) = row
        else {
            return Ok(None);
        };
        Ok(match state.as_str() {
            "pending" | "running" => Some(CheckpointVisualization {
                state: VisualizationState::Pending,
                note: None,
                diagnostic: None,
            }),
            "validated" => {
                let input: Option<VisualizerInputV1> =
                    input.and_then(|json| serde_json::from_str(&json).ok());
                let brief: Option<ReviewBriefV1> =
                    brief.and_then(|json| serde_json::from_str(&json).ok());
                match (input, brief) {
                    (Some(input), Some(brief)) => Some(CheckpointVisualization {
                        state: VisualizationState::Validated,
                        note: Some(review_brief::resolve_visualization(&brief, &input)),
                        diagnostic: None,
                    }),
                    _ => Some(CheckpointVisualization {
                        state: VisualizationState::Omitted,
                        note: None,
                        diagnostic: Some("The stored note is unreadable.".into()),
                    }),
                }
            }
            "omitted" => Some(CheckpointVisualization {
                state: VisualizationState::Omitted,
                note: None,
                diagnostic,
            }),
            _ => None,
        })
    }

    /// The background lane: answers the oldest pending note request whose
    /// connections are free of foreground demand.
    pub(super) async fn process_visualization(
        &self,
        shutdown: &CancellationToken,
    ) -> Result<bool, PersistenceServiceError> {
        let service = self.clone();
        let now = self.clock.now().to_rfc3339();
        let candidate = tokio::task::spawn_blocking(
            move || -> Result<Option<(AgentRuntimeConfig, String, String, u64)>, PersistenceServiceError> {
                let db = service.connection()?;
                let candidate: Option<(String, String, u64)> = db
                    .query_row(
                        "SELECT j.id,j.project_id,j.generation FROM jobs j
                         JOIN checkpoint_visualizations v ON v.job_id=j.id
                         WHERE j.kind='visualize_checkpoint' AND j.status='pending'
                           AND j.cancellation_reason IS NULL
                           AND v.state IN ('pending','running')
                           AND (j.available_at IS NULL OR j.available_at<=?1)
                         ORDER BY v.created_at,j.id LIMIT 1",
                        [now],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .optional()?;
                let Some((id, project, generation)) = candidate else {
                    return Ok(None);
                };
                Ok(Some((service.load_runtime_config()?, id, project, generation)))
            },
        )
        .await
        .map_err(|error| invalid(error.to_string()))??;
        let Some((config, id, project, _)) = candidate else {
            return Ok(false);
        };
        match self.llm_router.background_ready(
            &project,
            AgentRole::Visualizer,
            ROUTING_STAGE,
            config.provider.as_str(),
        ) {
            Ok(false) => return Ok(false),
            Ok(true) => {}
            Err(error) => {
                let service = self.clone();
                let reason = format!("No route for the Visualizer: {error}");
                tokio::task::spawn_blocking(move || service.omit_pending(&id, &reason))
                    .await
                    .map_err(|error| invalid(error.to_string()))??;
                return Ok(true);
            }
        }
        let service = self.clone();
        let claim = tokio::task::spawn_blocking(move || service.claim_background_id(&id))
            .await
            .map_err(|error| invalid(error.to_string()))??;
        let Some(claim) = claim else {
            return Ok(false);
        };
        let _slot = self.slot_guard(Self::job_slot_token(&claim.job));
        let result = self.execute_visualization(&claim, &config, shutdown).await;
        self.unregister_job_cancellation(&claim.job.id);
        if let Err(error) = result {
            let service = self.clone();
            let id = claim.job.id.clone();
            let generation = claim.generation;
            let reason = error.to_string();
            tokio::task::spawn_blocking(move || service.finish_omitted(&id, generation, &reason))
                .await
                .map_err(|error| invalid(error.to_string()))??;
        }
        Ok(true)
    }

    /// Charges one call and builds the input from the checkpoint's brief.
    /// `None` when the checkpoint is no longer waiting or has no brief: the
    /// series ends without a note.
    fn begin_visualization(
        &self,
        id: &str,
        generation: u64,
    ) -> Result<Option<VisualizationAttempt>, PersistenceServiceError> {
        let db = self.connection()?;
        let (checkpoint_id, pulsation_id, project_id, state_version, waiting): (
            String,
            String,
            String,
            u64,
            bool,
        ) = db.query_row(
            "SELECT v.checkpoint_id,v.pulsation_id,p.project_id,p.state_version,
                    c.status='waiting' AND p.status='checkpoint'
             FROM checkpoint_visualizations v
             JOIN checkpoints c ON c.id=v.checkpoint_id
             JOIN pulsations p ON p.id=v.pulsation_id
             WHERE v.job_id=?1",
            [id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )?;
        let brief = if waiting {
            self.read_checkpoint_brief(&HitlCheckpoint {
                pulsation_id: pulsation_id.clone(),
                checkpoint_id: checkpoint_id.clone(),
                state_version,
            })
            .ok()
            .flatten()
        } else {
            None
        };
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(brief) = brief else {
            tx.execute(
                "UPDATE checkpoint_visualizations SET state='skipped',diagnostic='The checkpoint has no current brief.',updated_at=?2 WHERE job_id=?1 AND state IN ('pending','running')",
                params![id, Self::now_iso()],
            )?;
            tx.execute(
                "UPDATE jobs SET status='done',finished_at=?3,updated_at=?3 WHERE id=?1 AND generation=?2 AND status='running'",
                params![id, generation, Self::now_iso()],
            )?;
            tx.commit()?;
            return Ok(None);
        };
        let input = review_brief::visualizer_input(&brief);
        let json = serde_json::to_string(&input).map_err(|error| invalid(error.to_string()))?;
        let previous_rejection: Option<String> = tx
            .query_row(
                "SELECT diagnostic FROM checkpoint_visualizations WHERE job_id=?1 AND calls>0",
                [id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        let charged = tx.execute(
            "UPDATE checkpoint_visualizations SET state='running',calls=calls+1,input_json=?3,updated_at=?4
             WHERE job_id=?1 AND calls<?5 AND state IN ('pending','running')
               AND EXISTS(SELECT 1 FROM jobs WHERE id=?1 AND generation=?2 AND status='running' AND cancellation_reason IS NULL)",
            params![id, generation, json, Self::now_iso(), MAX_CALLS],
        )?;
        if charged != 1 {
            return Err(invalid(
                "The Visualizer call budget or generation is stale.",
            ));
        }
        tx.commit()?;
        Ok(Some(VisualizationAttempt {
            input,
            previous_rejection,
            pulsation_id,
            project_id,
        }))
    }

    async fn execute_visualization(
        &self,
        claim: &BackgroundClaim,
        config: &AgentRuntimeConfig,
        shutdown: &CancellationToken,
    ) -> Result<(), PersistenceServiceError> {
        let service = self.clone();
        let id = claim.job.id.clone();
        let generation = claim.generation;
        let attempt =
            tokio::task::spawn_blocking(move || service.begin_visualization(&id, generation))
                .await
                .map_err(|error| invalid(error.to_string()))??;
        let Some(attempt) = attempt else {
            return Ok(());
        };
        let request = LlmRequest {
            execution_id: Uuid::new_v4().to_string().into(),
            generation_id: format!("{}:{}", claim.job.id, claim.generation).into(),
            project_id: attempt.project_id.clone(),
            pulsation_id: Some(attempt.pulsation_id.clone()),
            job_id: Some(claim.job.id.clone()),
            agent_run_id: None,
            job_chain: format!("visualize:{}", claim.job.id),
            target: AgentExecutionTarget::canonical(AgentRole::Visualizer),
            stage: ROUTING_STAGE.into(),
            system_prompt: review_brief::visualizer_system_prompt(
                attempt.previous_rejection.as_deref(),
            ),
            user_prompt: serde_json::to_string(&attempt.input)
                .map_err(|error| invalid(error.to_string()))?,
            response_format: ResponseFormat::JsonSchema {
                name: "review_brief_v1".into(),
                schema: review_brief::visualizer_schema(),
            },
            max_output_tokens: config.max_output_tokens.min(MAX_OUTPUT_TOKENS),
            temperature: config.temperature,
            deadline: std::time::Instant::now()
                + crate::agent_timeout::AgentTimeoutPolicy::for_role(AgentRole::Visualizer)
                    .base_timeout(),
            stream: true,
            legacy_provider: config.provider.as_str().into(),
            legacy_model: config.model.clone(),
            dispatch: crate::llm::DispatchPolicy::default(),
        };
        if shutdown.is_cancelled() {
            claim.cancellation.cancel();
        }
        let terminal_service = self
            .terminal_service
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let call = async {
            if let Some(terminals) = terminal_service {
                match terminals
                    .execute_llm_request(&request, &claim.cancellation)
                    .await
                {
                    Ok(Some(completion)) => Ok(completion.raw_output),
                    Ok(None) => self
                        .llm_router
                        .execute(&request, &claim.cancellation)
                        .await
                        .map(|output| output.content),
                    Err(error) => Err(LlmError::new(
                        LlmErrorKind::ProviderUnavailable,
                        error.to_string(),
                    )),
                }
            } else {
                self.llm_router
                    .execute(&request, &claim.cancellation)
                    .await
                    .map(|output| output.content)
            }
        };
        tokio::pin!(call);
        let result = tokio::select! {
            result = &mut call => result,
            _ = shutdown.cancelled() => { claim.cancellation.cancel(); call.await }
        };
        let service = self.clone();
        let id = claim.job.id.clone();
        let generation = claim.generation;
        let stopping = shutdown.is_cancelled();
        tokio::task::spawn_blocking(move || match result {
            Ok(output) => match review_brief::validate_visualization(&output, &attempt.input) {
                Ok(brief) => service.finish_validated(&id, generation, &brief),
                Err(reason) => service.finish_rejected(&id, generation, &reason),
            },
            Err(error) => service.finish_call_error(&id, generation, &error, stopping),
        })
        .await
        .map_err(|error| invalid(error.to_string()))?
    }

    fn finish_validated(
        &self,
        id: &str,
        generation: u64,
        brief: &ReviewBriefV1,
    ) -> Result<(), PersistenceServiceError> {
        let json = serde_json::to_string(brief).map_err(|error| invalid(error.to_string()))?;
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = Self::now_iso();
        if tx.execute(
            "UPDATE jobs SET status='done',finished_at=?3,updated_at=?3 WHERE id=?1 AND generation=?2 AND status='running'",
            params![id, generation, now],
        )? == 1
        {
            tx.execute(
                "UPDATE checkpoint_visualizations SET state='validated',brief_json=?2,diagnostic=NULL,updated_at=?3 WHERE job_id=?1 AND state='running'",
                params![id, json, now],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// One corrective call when the budget allows it, else the note is
    /// omitted.
    fn finish_rejected(
        &self,
        id: &str,
        generation: u64,
        reason: &str,
    ) -> Result<(), PersistenceServiceError> {
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let calls: u32 = tx.query_row(
            "SELECT calls FROM checkpoint_visualizations WHERE job_id=?1",
            [id],
            |row| row.get(0),
        )?;
        if calls >= MAX_CALLS {
            drop(tx);
            return self.finish_omitted(
                id,
                generation,
                &format!("The Visualizer's answer was rejected twice: {reason}"),
            );
        }
        let now = Self::now_iso();
        if tx.execute(
            "UPDATE jobs SET status='pending',last_error=?3,updated_at=?4 WHERE id=?1 AND generation=?2 AND status='running'",
            params![id, generation, reason, now],
        )? == 1
        {
            tx.execute(
                "UPDATE checkpoint_visualizations SET state='pending',diagnostic=?2,updated_at=?3 WHERE job_id=?1",
                params![id, reason, now],
            )?;
        }
        tx.commit()?;
        self.wake_scheduler();
        Ok(())
    }

    /// A shutdown or a foreground preemption refunds the call and retries;
    /// any other failure omits the note.
    fn finish_call_error(
        &self,
        id: &str,
        generation: u64,
        error: &LlmError,
        stopping: bool,
    ) -> Result<(), PersistenceServiceError> {
        let deferred = stopping
            || matches!(
                error.kind,
                LlmErrorKind::BackgroundDeferred | LlmErrorKind::Cancelled
            );
        if !deferred {
            return self.finish_omitted(
                id,
                generation,
                &format!("The Visualizer call failed: {error}"),
            );
        }
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = Self::now_iso();
        if tx.execute(
            "UPDATE jobs SET status='pending',available_at=?3,updated_at=?4 WHERE id=?1 AND generation=?2 AND status='running'",
            params![
                id,
                generation,
                (self.clock.now() + chrono::Duration::seconds(5)).to_rfc3339(),
                now
            ],
        )? == 1
        {
            tx.execute(
                "UPDATE checkpoint_visualizations SET state='pending',calls=MAX(calls-1,0),updated_at=?2 WHERE job_id=?1",
                params![id, now],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Ends the series without a note and says so in the pulsation's
    /// activity.
    fn finish_omitted(
        &self,
        id: &str,
        generation: u64,
        reason: &str,
    ) -> Result<(), PersistenceServiceError> {
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = Self::now_iso();
        if tx.execute(
            "UPDATE jobs SET status='failed',last_error=?3,finished_at=?4,updated_at=?4 WHERE id=?1 AND generation=?2 AND status='running'",
            params![id, generation, reason, now],
        )? == 1
        {
            Self::record_omission(&tx, id, reason)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Omits a request that never started (no route to call).
    fn omit_pending(&self, id: &str, reason: &str) -> Result<(), PersistenceServiceError> {
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = Self::now_iso();
        if tx.execute(
            "UPDATE jobs SET status='failed',last_error=?2,finished_at=?3,updated_at=?3 WHERE id=?1 AND status='pending'",
            params![id, reason, now],
        )? == 1
        {
            Self::record_omission(&tx, id, reason)?;
        }
        tx.commit()?;
        Ok(())
    }

    fn record_omission(
        db: &Connection,
        id: &str,
        reason: &str,
    ) -> Result<(), PersistenceServiceError> {
        let now = Self::now_iso();
        db.execute(
            "UPDATE checkpoint_visualizations SET state='omitted',diagnostic=?2,updated_at=?3 WHERE job_id=?1",
            params![id, reason, now],
        )?;
        db.execute(
            "INSERT INTO pulse_events(id,pulsation_id,event_type,message,created_at)
             SELECT ?1,pulsation_id,'visualizer_omitted',?2,?3 FROM checkpoint_visualizations WHERE job_id=?4",
            params![
                Uuid::new_v4().to_string(),
                format!("Visualizer note omitted. {reason}"),
                now,
                id
            ],
        )?;
        Ok(())
    }
}
