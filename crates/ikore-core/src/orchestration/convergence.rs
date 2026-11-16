//! Convergence of a pulsation's revision rounds (ADR-032, M4).
//!
//! The host owns the rule: human Request revision decisions (at artifact
//! checkpoints and on the failure path) are counted per input revision,
//! separately from the technical repair, retry and fallback attempts the
//! stage budget already bounds. Once the rounds since the last replan reach
//! the project's threshold, every further revision needs an explicit
//! decision: continue with a reason, or replan from the Specification.
//! Counts come from durable rows only, so a restart never resets them.

use super::{
    Connection, ConvergenceAck, ConvergenceDecisionRecord, ConvergenceStatus, OptionalExtension,
    PersistenceService, PersistenceServiceError, PulsationStage, Uuid, params,
};
use crate::orchestration::PipelineVersion;

/// The longest reason kept with a decision, in Unicode scalar values.
const MAX_REASON_CHARS: usize = 2_000;

impl PersistenceService {
    /// The convergence state of a pulsation's current input revision.
    pub fn convergence_status(
        &self,
        pulsation_id: &str,
    ) -> Result<ConvergenceStatus, PersistenceServiceError> {
        Self::convergence_status_on(&self.connection()?, pulsation_id)
    }

    pub(super) fn convergence_status_on(
        db: &Connection,
        pulsation_id: &str,
    ) -> Result<ConvergenceStatus, PersistenceServiceError> {
        let Some((project_id, input_revision, version)) = db
            .query_row(
                "SELECT project_id,input_revision,pipeline_version FROM pulsations WHERE id=?1",
                [pulsation_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, u64>(1)?,
                        row.get::<_, u8>(2)?,
                    ))
                },
            )
            .optional()?
        else {
            return Err(PersistenceServiceError::NotFound(
                "Pulsation not found.".into(),
            ));
        };
        let applies =
            PipelineVersion::from_db(version).is_some_and(|version| version.supports_composition());
        let threshold = Self::composition_settings_for(db, &project_id)?.revision_round_threshold;
        let human_rounds = db
            .prepare(
                "SELECT stage,COUNT(*) FROM checkpoints WHERE pulsation_id=?1 AND input_revision=?2
                   AND purpose='artifact_review' AND status='revision_requested'
                 GROUP BY stage ORDER BY stage",
            )?
            .query_map(params![pulsation_id, input_revision], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let failure_decisions = |decision: &str| -> Result<u32, PersistenceServiceError> {
            Ok(db.query_row(
                "SELECT COUNT(*) FROM stage_failure_requests f JOIN checkpoints c ON c.id=f.checkpoint_id
                 WHERE f.pulsation_id=?1 AND c.input_revision=?2 AND f.decision=?3",
                params![pulsation_id, input_revision, decision],
                |row| row.get(0),
            )?)
        };
        let failure_revisions = failure_decisions("request_revision")?;
        let retries = failure_decisions("retry_stage")?;
        let technical_attempts: u32 = db.query_row(
            "SELECT COUNT(*) FROM stage_work_unit_attempts a JOIN stage_executions e ON e.id=a.execution_id
             WHERE e.pulsation_id=?1 AND e.parent_input_revision=?2",
            params![pulsation_id, input_revision],
            |row| row.get(0),
        )?;
        let extra_invocations: u32 = db.query_row(
            "SELECT COUNT(*) FROM stage_invocation_reservations r JOIN stage_executions e ON e.id=r.execution_id
             WHERE e.pulsation_id=?1 AND e.parent_input_revision=?2 AND r.pool='extra' AND r.state<>'released'",
            params![pulsation_id, input_revision],
            |row| row.get(0),
        )?;
        let candidate_generations: u32 = db.query_row(
            "SELECT COUNT(*) FROM artifacts WHERE pulsation_id=?1 AND input_revision=?2
               AND upper(producer_stage)='CODE' AND kind=?3",
            params![
                pulsation_id,
                input_revision,
                crate::orchestration::composition::recipes::STAGE_PAYLOAD_KIND
            ],
            |row| row.get(0),
        )?;
        let decisions = db
            .prepare(
                "SELECT stage,decision,rounds_at_decision,threshold,reason,created_at
                 FROM pulsation_convergence_decisions WHERE pulsation_id=?1 AND input_revision=?2
                 ORDER BY created_at,rowid",
            )?
            .query_map(params![pulsation_id, input_revision], |row| {
                Ok(ConvergenceDecisionRecord {
                    stage: row.get(0)?,
                    decision: row.get(1)?,
                    rounds_at_decision: row.get(2)?,
                    threshold: row.get(3)?,
                    reason: row.get(4)?,
                    created_at: row.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut status = ConvergenceStatus {
            applies,
            input_revision,
            threshold,
            rounds: 0,
            human_rounds,
            failure_revisions,
            retries,
            technical_attempts,
            extra_invocations,
            candidate_generations,
            decisions,
        };
        let total = total_rounds(&status);
        // A replan's own revision opens the new window: rounds restart at 0.
        status.rounds = status
            .decisions
            .iter()
            .rev()
            .find(|decision| decision.decision == "replan")
            .map_or(total, |replan| {
                total.saturating_sub(replan.rounds_at_decision + 1)
            });
        Ok(status)
    }

    /// Admits one more revision of `stage` at `checkpoint_id`, inside the
    /// caller's transaction and before the checkpoint is resolved. Below the
    /// threshold, or before pipeline v4, nothing is recorded. At or beyond
    /// it the decision is required, recorded with its reason and returned:
    /// `Some(Specification)` when the developer chose to replan.
    pub(super) fn admit_revision_round(
        tx: &Connection,
        pulsation_id: &str,
        checkpoint_id: &str,
        stage: PulsationStage,
        ack: Option<&ConvergenceAck>,
    ) -> Result<Option<PulsationStage>, PersistenceServiceError> {
        let status = Self::convergence_status_on(tx, pulsation_id)?;
        if !status.threshold_reached() {
            return Ok(None);
        }
        let Some(ack) = ack else {
            return Err(PersistenceServiceError::Validation(format!(
                "convergence_threshold_reached: this pulsation already had {} revision round(s) (threshold {}). Choose to continue with a reason, replan from the Specification, or reject.",
                status.rounds, status.threshold
            )));
        };
        let reason = ack.reason().trim();
        if reason.is_empty() {
            return Err(PersistenceServiceError::Validation(
                "A decision beyond the revision threshold needs a reason.".into(),
            ));
        }
        let reason = crate::llm::adapter::sanitize_model_text(reason)
            .unwrap_or_else(|_| crate::secret_redactor::OMITTED_CONTENT.to_owned())
            .chars()
            .take(MAX_REASON_CHARS)
            .collect::<String>();
        let replan = matches!(ack, ConvergenceAck::Replan { .. });
        if replan && stage == PulsationStage::Intake {
            return Err(PersistenceServiceError::Validation(
                "Intake has no Specification to replan from.".into(),
            ));
        }
        let now = Self::now_iso();
        tx.execute(
            "INSERT INTO pulsation_convergence_decisions(id,pulsation_id,checkpoint_id,input_revision,stage,decision,rounds_at_decision,threshold,reason,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                Uuid::new_v4().to_string(),
                pulsation_id,
                checkpoint_id,
                status.input_revision,
                stage.as_db_value(),
                ack.as_db_value(),
                total_rounds(&status),
                status.threshold,
                reason,
                now,
            ],
        )?;
        tx.execute(
            "INSERT INTO pulse_events(id,pulsation_id,event_type,from_stage,to_stage,message,created_at)
             VALUES(?1,?2,'convergence_decision',?3,?4,?5,?6)",
            params![
                Uuid::new_v4().to_string(),
                pulsation_id,
                stage.as_db_value(),
                if replan {
                    PulsationStage::Specification.as_db_value()
                } else {
                    stage.as_db_value()
                },
                format!(
                    "{} after {} revision round(s) (threshold {}).",
                    if replan {
                        "Replan from the Specification"
                    } else {
                        "Revision beyond the threshold"
                    },
                    status.rounds,
                    status.threshold
                ),
                now,
            ],
        )?;
        Ok(replan.then_some(PulsationStage::Specification))
    }
}

/// Every human revision of the input revision, before the one being
/// decided. A decision records it as `rounds_at_decision`, so the window
/// after a replan is measured from it; `status.rounds` is only the window.
fn total_rounds(status: &ConvergenceStatus) -> u32 {
    status
        .human_rounds
        .iter()
        .map(|(_, count)| count)
        .sum::<u32>()
        + status.failure_revisions
}
