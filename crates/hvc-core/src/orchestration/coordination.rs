//! SQLite admission and executor slots. Ownership survives HITL and process shutdown.
use super::*;
use crate::orchestration::JobClass;

#[cfg(test)]
#[path = "coordination_tests.rs"]
mod tests;

pub(super) fn invalid(message: impl Into<String>) -> PersistenceServiceError {
    PersistenceServiceError::Validation(message.into())
}

pub(super) fn class_name(class: JobClass) -> &'static str {
    match class {
        JobClass::Main => "main",
        JobClass::Background => "background",
    }
}

/// A live executor retains its slot even after its result is committed or user-cancelled.
/// A crash leaves the durable reservation for recovery under the data-directory lock.
pub(super) struct SlotGuard {
    service: PersistenceService,
    token: String,
    notify: bool,
}

pub(super) struct UserCancellationIntent {
    service: PersistenceService,
    pulse: String,
}

impl Drop for UserCancellationIntent {
    fn drop(&mut self) {
        let mut pending = self
            .service
            .runtime_control
            .pending_user_cancels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(count) = pending.get_mut(&self.pulse) {
            *count -= 1;
            if *count == 0 {
                pending.remove(&self.pulse);
            }
        }
        self.service.wake_scheduler();
    }
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        if let Err(error) = self.service.release_runtime_slot(&self.token) {
            eprintln!("executor slot release failed (recovery required): {error}");
        }
        if self.notify {
            self.service.wake_scheduler();
        }
    }
}

impl SlotGuard {
    pub(super) fn quiet(mut self) -> Self {
        self.notify = false;
        self
    }
}

impl PersistenceService {
    pub(super) fn begin_user_cancellation(&self, pulse: &str) -> UserCancellationIntent {
        *self
            .runtime_control
            .pending_user_cancels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(pulse.into())
            .or_default() += 1;
        UserCancellationIntent {
            service: self.clone(),
            pulse: pulse.into(),
        }
    }

    pub(super) fn user_cancellation_pending(&self, pulse: &str) -> bool {
        self.runtime_control
            .pending_user_cancels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(pulse)
    }

    pub(super) fn wake_scheduler(&self) {
        self.runtime_control.wakeup.notify_waiters();
    }

    pub(super) fn reserve_slot(
        db: &Connection,
        class: JobClass,
        token: &str,
        project: Option<&str>,
        job: Option<&str>,
    ) -> Result<bool, PersistenceServiceError> {
        Ok(db.execute(
            "INSERT OR IGNORE INTO runtime_slots(job_class,token,project_id,job_id,acquired_at) VALUES(?1,?2,?3,?4,?5)",
            params![class_name(class), token, project, job, Self::now_iso()],
        )? == 1)
    }

    pub(super) fn slot_guard(&self, token: String) -> SlotGuard {
        SlotGuard {
            service: self.clone(),
            token,
            notify: true,
        }
    }

    pub(super) fn release_runtime_slot(&self, token: &str) -> Result<(), PersistenceServiceError> {
        self.connection()?
            .execute("DELETE FROM runtime_slots WHERE token=?1", [token])?;
        Ok(())
    }

    pub(super) fn job_slot_token(job: &Job) -> String {
        format!("{}:{}", job.id, job.attempts)
    }

    pub(super) fn claim_next_pending_job(
        &self,
    ) -> Result<Option<(Job, CancellationToken)>, PersistenceServiceError> {
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        // Publication has principal priority. Unavailable/fenced jobs never consume attempts.
        let job = tx.query_row(
            "SELECT j.id,j.pulsation_id,j.kind,j.status,j.attempts,j.last_error,j.created_at,j.updated_at,j.started_at,j.finished_at,j.cancel_requested_at,j.available_at
             FROM jobs j JOIN pulsations p ON p.id=j.pulsation_id
             JOIN project_pipeline_state s ON s.project_id=p.project_id
             WHERE j.status='pending' AND j.cancellation_reason IS NULL
               AND j.project_id=p.project_id AND j.input_revision=p.input_revision AND j.state_version=p.state_version
               AND p.status IN ('planning','running') AND p.id IN (SELECT id FROM coordinated_pulsations)
               AND s.base_state='ready'
               AND NOT EXISTS(SELECT 1 FROM pulsation_preparation r WHERE r.pulsation_id=p.id AND r.review_state='pending')
               AND NOT EXISTS(SELECT 1 FROM runtime_slots WHERE job_id=j.id)
               AND (j.kind='run_stage::'||p.stage OR (p.stage='SPECIFICATION' AND j.kind='run_stage::Specification'))
               AND (j.available_at IS NULL OR j.available_at<=?1)
               AND NOT EXISTS(SELECT 1 FROM runtime_slots WHERE job_class='main')
               AND NOT EXISTS(SELECT 1 FROM artifact_publications o JOIN artifacts a ON a.id=o.artifact_id
                   WHERE o.job_class='main' AND o.pulsation_id=p.id AND a.acceptance='accepted' AND o.state NOT IN ('applied','cancelled'))
               AND NOT EXISTS(SELECT 1 FROM artifact_publications o JOIN artifacts a ON a.id=o.artifact_id
                   WHERE o.job_class='main' AND o.pulsation_id IN (SELECT id FROM coordinated_pulsations) AND a.acceptance='accepted' AND o.state IN ('pending','applying'))
             ORDER BY CASE WHEN s.active_pulsation_id=p.id THEN 0 ELSE 1 END,p.created_at,p.project_id,p.queue_sequence,j.id LIMIT 1",
            [Self::now_iso()], Self::map_job,
        ).optional()?;
        let Some(mut job) = job else { return Ok(None) };
        let pulse = job.pulsation_id.as_deref().expect("stage pulse");
        if self.user_cancellation_pending(pulse) {
            return Ok(None);
        }
        let (project, stage): (String, String) = tx.query_row(
            "SELECT project_id,stage FROM pulsations WHERE id=?1",
            [pulse],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if stage != "Intake" {
            tx.execute("UPDATE project_pipeline_state SET active_pulsation_id=?2,state_version=state_version+1 WHERE project_id=?1 AND active_pulsation_id IS NULL",params![project,pulse])?;
        }
        let mut cancelled = Vec::new();
        if stage == "SPECIFICATION" {
            tx.execute(
                "UPDATE pulsations SET specification_started=1 WHERE id=?1",
                [pulse],
            )?;
            cancelled = Self::invalidate_preparation(&tx, pulse, "base_released")?;
            tx.execute("UPDATE pulsation_preparation SET prespec_state='skipped',skip_reason='skipped_no_predecessor' WHERE pulsation_id=?1 AND prespec_state='not_scheduled'",[pulse])?;
        }
        job.attempts += 1;
        let token = Self::job_slot_token(&job);
        if !Self::reserve_slot(&tx, JobClass::Main, &token, Some(&project), Some(&job.id))? {
            return Ok(None);
        }
        let now = Self::now_iso();
        tx.execute("UPDATE jobs SET status='running',job_class='main',attempts=attempts+1,generation=generation+1,started_at=?2,cancel_requested_at=NULL,available_at=NULL,updated_at=?2 WHERE id=?1",params![job.id,now])?;
        let cancellation = CancellationToken::new();
        self.register_job_cancellation(&job.id, cancellation.clone());
        if let Err(error) = tx.commit() {
            self.unregister_job_cancellation(&job.id);
            return Err(error.into());
        }
        self.signal_cancelled_jobs(&cancelled);
        job.status = JobStatus::Running;
        job.started_at = Some(now.clone());
        job.updated_at = now;
        job.available_at = None;
        job.cancel_requested_at = None;
        Ok(Some((job, cancellation)))
    }

    pub(super) fn check_project_admission(
        db: &Connection,
        pulse: &str,
    ) -> Result<(), PersistenceServiceError> {
        let admitted: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM coordinated_pulsations WHERE id=?1)",
            [pulse],
            |r| r.get(0),
        )?;
        if !admitted {
            return Err(invalid(
                "Projeto aguardando base/proprietário ou reconciliação explícita.",
            ));
        }
        Ok(())
    }

    /// Covers setup failures before the agent-run/error handler exists. A released executor
    /// must not leave an unexplained running job holding the project until the next restart.
    pub(super) fn close_abandoned_claim(
        &self,
        job: &Job,
        message: &str,
    ) -> Result<(), PersistenceServiceError> {
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current: bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM jobs j JOIN pulsations p ON p.id=j.pulsation_id WHERE j.id=?1 AND j.attempts=?2 AND j.status='running' AND j.state_version=p.state_version AND j.input_revision=p.input_revision AND p.status IN ('planning','running') AND NOT EXISTS(SELECT 1 FROM stage_acceptances a WHERE a.job_id=j.id))",params![job.id,job.attempts],|r|r.get(0))?;
        if current {
            let pulse = job
                .pulsation_id
                .as_deref()
                .ok_or_else(|| invalid("Job sem pulsação"))?;
            let p = Self::find_pulsation_by_id(&tx, pulse)?
                .ok_or_else(|| invalid("Pulsação ausente"))?;
            // Setup can die while the pulsation is still `planning`, before
            // `execute_claimed_job` performs Planning -> Running.
            let parked = Self::record_stage_failure_checkpoint(
                &tx,
                &p,
                p.stage,
                &job.id,
                None,
                crate::orchestration::FailureClass::ExecutorSetup,
                "executor_setup_failed",
                message,
            )?;
            if parked {
                tx.execute(
                    "UPDATE agent_runs SET status='awaiting_decision',updated_at=?2
                     WHERE job_id=?1 AND status='running'",
                    params![job.id, Self::now_iso()],
                )?;
            } else {
                Self::transition_pulsation(
                    &tx,
                    &p,
                    PulsationStatus::Failed,
                    p.stage,
                    "executor_setup_failed",
                    Some(message),
                )?;
                tx.execute(
                    "UPDATE pulsations SET terminal_reason='executor_setup_failed' WHERE id=?1",
                    [pulse],
                )?;
                tx.execute("UPDATE jobs SET status='failed',last_error=?2,finished_at=?3,updated_at=?3 WHERE id=?1",params![job.id,message,Self::now_iso()])?;
                tx.execute("UPDATE agent_runs SET status='failed',updated_at=?2 WHERE job_id=?1 AND status='running'",params![job.id,Self::now_iso()])?;
                Self::mark_effects_for_review(&tx, pulse, "executor_failed_with_effects")?;
                Self::enqueue_learning(&tx, pulse)?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub(super) fn release_safe_owner(
        db: &Connection,
        pulse: &str,
    ) -> Result<(), PersistenceServiceError> {
        db.execute("UPDATE project_pipeline_state SET active_pulsation_id=NULL,state_version=state_version+1 WHERE active_pulsation_id=?1 AND base_state<>'needs_review' AND EXISTS(SELECT 1 FROM pulsations p WHERE p.id=?1 AND p.status IN ('done','failed'))",[pulse])?;
        Self::block_held_successors(db)?;
        let project: String = db.query_row(
            "SELECT project_id FROM pulsations WHERE id=?1",
            [pulse],
            |r| r.get(0),
        )?;
        crate::feature_forge::service::release_failed(db, &project)?;
        Ok(())
    }

    pub(super) fn block_held_successors(db: &Connection) -> Result<(), PersistenceServiceError> {
        db.execute("UPDATE project_pipeline_state SET base_state='needs_review',blocking_reason='remaining_legacy_owners',state_version=state_version+1 WHERE active_pulsation_id IS NULL AND base_state<>'needs_review' AND EXISTS(SELECT 1 FROM pulsations p WHERE p.project_id=project_pipeline_state.project_id AND p.coordination_hold=1 AND p.status NOT IN ('done','failed'))",[])?;
        Ok(())
    }

    /// Called only at startup, before any executor exists and under the AppCore lock.
    pub(super) fn recover_coordination(db: &Connection) -> Result<(), PersistenceServiceError> {
        db.execute("DELETE FROM runtime_slots", [])?;
        // A durable terminal owner is released only if its effects have already been reviewed.
        db.execute("UPDATE project_pipeline_state SET active_pulsation_id=NULL,state_version=state_version+1 WHERE base_state<>'needs_review' AND active_pulsation_id IN (SELECT id FROM pulsations WHERE status IN ('done','failed'))",[])?;
        Self::block_held_successors(db)?;
        Ok(())
    }
}
