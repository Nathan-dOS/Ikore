//! Failure snapshots and their exact, human-authorized recovery-job binding.
use super::{Connection, OptionalExtension, PersistenceService, PersistenceServiceError, params};
use crate::orchestration::composition::recovery::ValidationFailureContextV1;

impl PersistenceService {
    pub(crate) fn store_validation_failure_context(
        db: &Connection,
        checkpoint: &str,
        context: &ValidationFailureContextV1,
    ) -> Result<(), PersistenceServiceError> {
        let json = serde_json::to_string(context)
            .map_err(|e| PersistenceServiceError::Runtime(e.to_string()))?;
        let stored = db.execute("INSERT INTO validation_failure_contexts(checkpoint_id,execution_id,context_json,created_at) SELECT c.id,f.stage_execution_id,?3,?4 FROM checkpoints c JOIN stage_failure_requests f ON f.checkpoint_id=c.id WHERE c.id=?1 AND f.stage_execution_id=?2 AND c.status='waiting'", params![checkpoint, context.stage_execution_id, json, Self::now_iso()])?;
        if stored != 1 {
            return Err(PersistenceServiceError::Validation(
                "Failure evidence no longer matches the waiting execution.".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn validation_failure_context(
        db: &Connection,
        checkpoint: &str,
    ) -> Result<Option<ValidationFailureContextV1>, PersistenceServiceError> {
        let json: Option<String> = db
            .query_row(
                "SELECT context_json FROM validation_failure_contexts WHERE checkpoint_id=?1",
                [checkpoint],
                |row| row.get(0),
            )
            .optional()?;
        json.map(|json| {
            serde_json::from_str(&json).map_err(|e| PersistenceServiceError::Runtime(e.to_string()))
        })
        .transpose()
    }

    pub(crate) fn bind_validation_failure_recovery(
        db: &Connection,
        checkpoint: &str,
        job: &str,
    ) -> Result<(), PersistenceServiceError> {
        db.execute("INSERT INTO validation_failure_recovery_jobs(job_id,checkpoint_id) SELECT j.id,v.checkpoint_id FROM validation_failure_contexts v JOIN checkpoints c ON c.id=v.checkpoint_id JOIN jobs j ON j.pulsation_id=c.pulsation_id AND j.input_revision=c.input_revision WHERE v.checkpoint_id=?1 AND j.id=?2", params![checkpoint,job])?;
        Ok(())
    }

    pub(crate) fn validation_failure_context_for_job(
        db: &Connection,
        job: &str,
    ) -> Result<Option<ValidationFailureContextV1>, PersistenceServiceError> {
        let checkpoint: Option<String> = db.query_row("SELECT r.checkpoint_id FROM validation_failure_recovery_jobs r JOIN jobs j ON j.id=r.job_id JOIN checkpoints c ON c.id=r.checkpoint_id WHERE j.id=?1 AND j.pulsation_id=c.pulsation_id AND j.input_revision=c.input_revision", [job], |row| row.get(0)).optional()?;
        match checkpoint {
            Some(checkpoint) => Self::validation_failure_context(db, &checkpoint),
            None => Ok(None),
        }
    }
}
