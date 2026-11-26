//! Feature 25 (M4): runtime engine for stage verifiers — hook admission,
//! automation-lane execution, crash-once recovery (P16), outcome settlement
//! (P18–P21), `VerificationRejected` failure checkpoints, and
//! `verification_error` checkpoints (P15, P17, P25). A module of
//! `persistence_service`.

use std::path::PathBuf;
use std::time::Instant;

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::custom_pipeline_engine::checked_verdict;
use super::custom_pipeline_store::max_concurrent;
use super::mandate_persistence::ReceiptInput;
use super::{
    PersistenceService, PersistenceServiceError, Pulsation, PulsationStage, PulsationStatus,
    sanitized_observation_text,
};
use crate::custom_pipelines::{StageVerdictV1, StepTransport, Verdict, stage_verdict_json_schema};
use crate::llm::{
    AgentExecutionTarget, DispatchPolicy, ExecutionId, GenerationId, LlmErrorKind, LlmGateway,
    LlmRequest, ResponseFormat,
};
use crate::orchestration::FailureClass;
use crate::stage_verifiers::prompt::{
    load_previous_verdict_for_verifier, load_stage_result_artifacts, load_task_scope_text,
    verifier_correction_prompt, verifier_system_prompt, verifier_user_prompt,
};
use crate::stage_verifiers::{
    FindingSeverity, MAX_JUSTIFICATION_CHARS, ResolveVerificationErrorRequest,
    ResolveVerificationErrorResponse, StageVerifierConfig, VerificationErrorCheckpoint,
    VerificationErrorDecision, parse_verifier_config, parse_verifier_draft,
    verifier_cancellation_key, verifier_slot_id, verifier_tab_label,
};
use crate::terminals::{
    AUTOMATION_OUTPUT_TOKENS, AutomationStepRequest, CliBackend, ExecutionOrigin, TerminalError,
    ThinkingLevel,
};

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn invalid(message: impl Into<String>) -> PersistenceServiceError {
    PersistenceServiceError::Validation(message.into())
}

fn runtime(message: impl Into<String>) -> PersistenceServiceError {
    PersistenceServiceError::Runtime(message.into())
}

fn stage_order(stage: PulsationStage) -> usize {
    match stage {
        PulsationStage::Intake => 0,
        PulsationStage::Specification => 1,
        PulsationStage::Contract => 2,
        PulsationStage::Code => 3,
        PulsationStage::Test => 4,
        PulsationStage::Integrator => 5,
    }
}

/// A pending verifier run claimed by the automation lane.
#[derive(Debug, Clone)]
pub(crate) struct VerifierClaim {
    token: String,
    pub(crate) run_id: String,
    pub(crate) verifier_id: String,
    project_id: String,
    pulsation_id: String,
    stage: PulsationStage,
    stage_job_id: String,
    stage_generation: u64,
    attempt: u64,
    config: StageVerifierConfig,
}

struct VerifierContext {
    system: String,
    user: String,
    workspace: PathBuf,
}

struct VerifierCall {
    raw: String,
    terminal_run_id: Option<String>,
    llm_execution_id: Option<String>,
}

enum VerifierOutcome {
    Verdict(StageVerdictV1),
}

enum VerifierFailure {
    Cancelled,
    TechnicalError(String),
}

struct VerifierRunResult {
    outcome: Result<VerifierOutcome, VerifierFailure>,
    terminal_run_id: Option<String>,
    llm_execution_id: Option<String>,
}

impl VerifierRunResult {
    fn technical_error(message: impl Into<String>) -> Self {
        Self {
            outcome: Err(VerifierFailure::TechnicalError(message.into())),
            terminal_run_id: None,
            llm_execution_id: None,
        }
    }

    fn record(&mut self, call: &VerifierCall) {
        if call.terminal_run_id.is_some() {
            self.terminal_run_id.clone_from(&call.terminal_run_id);
        }
        if call.llm_execution_id.is_some() {
            self.llm_execution_id.clone_from(&call.llm_execution_id);
        }
    }
}

struct DeferredDispositionRow {
    stage: PulsationStage,
    stage_generation: u64,
    stage_job_id: String,
    input_revision: u64,
    deferred_disposition: String,
}

fn insert_pulse_event(
    tx: &Connection,
    pulsation_id: &str,
    event_type: &str,
    from_status: PulsationStatus,
    to_status: PulsationStatus,
    from_stage: PulsationStage,
    to_stage: PulsationStage,
    message: Option<&str>,
    now: &str,
) -> Result<(), PersistenceServiceError> {
    tx.execute(
        "INSERT INTO pulse_events(
           id,pulsation_id,event_type,from_status,to_status,from_stage,to_stage,message,created_at
         ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![
            Uuid::new_v4().to_string(),
            pulsation_id,
            event_type,
            from_status.as_db_value(),
            to_status.as_db_value(),
            from_stage.as_db_value(),
            to_stage.as_db_value(),
            message,
            now
        ],
    )?;
    Ok(())
}

/// Counts rejections for `(pulsation_id, stage, verifier_id)` since the most
/// recent human revision (`hitl_revision_requested` or
/// `stage_failure_revision_requested`) that reopened `stage` (`target <= stage`).
/// Returns the cutoff `state_version` so callers can also reuse it.
fn rejection_cutoff_state_version(
    tx: &Connection,
    pulsation_id: &str,
    stage: PulsationStage,
) -> Result<u64, PersistenceServiceError> {
    let target_order = stage_order(stage);
    let mut stmt = tx.prepare(
        "SELECT c.state_version,
                (SELECT j.kind FROM jobs j
                  WHERE j.pulsation_id=c.pulsation_id AND j.state_version=c.state_version+1
                  ORDER BY j.rowid ASC LIMIT 1)
           FROM checkpoints c
          WHERE c.pulsation_id=?1 AND c.status='revision_requested'
          ORDER BY c.state_version DESC",
    )?;
    let rows = stmt
        .query_map([pulsation_id], |row| {
            Ok((row.get::<_, u64>(0)?, row.get::<_, Option<String>>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (before_version, job_kind) in rows {
        let reopened_stage = job_kind
            .as_deref()
            .and_then(|kind| kind.strip_prefix("run_stage::"))
            .and_then(PulsationStage::from_db_value);
        if let Some(reopened) = reopened_stage
            && stage_order(reopened) <= target_order
        {
            return Ok(before_version + 1);
        }
    }
    Ok(0)
}

fn count_verifier_rejections_since_revision(
    tx: &Connection,
    pulsation_id: &str,
    stage: PulsationStage,
    verifier_id: &str,
) -> Result<u64, PersistenceServiceError> {
    let cutoff = rejection_cutoff_state_version(tx, pulsation_id, stage)?;
    let count: i64 = tx.query_row(
        "SELECT COUNT(*)
           FROM stage_verifier_runs r
           JOIN jobs j ON j.id=r.stage_job_id
          WHERE r.pulsation_id=?1 AND r.stage=?2 AND r.verifier_id=?3
            AND r.status='rejected' AND j.state_version>=?4",
        params![
            pulsation_id,
            stage.as_db_value(),
            verifier_id,
            cutoff as i64
        ],
        |row| row.get(0),
    )?;
    Ok(count.max(0) as u64)
}

fn load_deferred_disposition(
    tx: &Connection,
    pulsation_id: &str,
    stage: PulsationStage,
    stage_generation: u64,
) -> Result<Option<DeferredDispositionRow>, PersistenceServiceError> {
    tx.query_row(
        "SELECT stage,stage_generation,stage_job_id,input_revision,deferred_disposition
           FROM pulsation_stage_verification
          WHERE pulsation_id=?1 AND stage=?2 AND stage_generation=?3",
        params![pulsation_id, stage.as_db_value(), stage_generation as i64],
        |row| {
            let stage_raw: String = row.get(0)?;
            let stage = PulsationStage::from_db_value(&stage_raw).ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    "invalid stage".into(),
                )
            })?;
            Ok(DeferredDispositionRow {
                stage,
                stage_generation: row.get::<_, i64>(1)?.max(1) as u64,
                stage_job_id: row.get(2)?,
                input_revision: row.get::<_, i64>(3)?.max(1) as u64,
                deferred_disposition: row.get(4)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

fn format_rejection_diagnostic(rejected_runs: &[(StageVerifierConfig, StageVerdictV1)]) -> String {
    let mut parts = Vec::new();
    for (config, verdict) in rejected_runs {
        let blocking: Vec<String> = verdict
            .findings
            .iter()
            .filter(|finding| finding.severity == FindingSeverity::Blocking)
            .map(|finding| sanitized_observation_text(&finding.message))
            .collect();
        let summary = sanitized_observation_text(&verdict.summary);
        if blocking.is_empty() {
            parts.push(format!("{}: {}", config.label, summary));
        } else {
            parts.push(format!(
                "{}: {} ({})",
                config.label,
                summary,
                blocking.join("; ")
            ));
        }
    }
    let combined = parts.join(" | ");
    sanitized_observation_text(&combined)
        .chars()
        .take(1_200)
        .collect()
}

fn cancel_active_verifiers_and_collect(
    tx: &Transaction<'_>,
    pulsation_id: &str,
    now: &str,
) -> Result<Vec<String>, PersistenceServiceError> {
    let run_ids = tx
        .prepare(
            "SELECT id FROM stage_verifier_runs
              WHERE pulsation_id=?1 AND status IN ('pending','running')",
        )?
        .query_map([pulsation_id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    tx.execute(
        "UPDATE stage_verifier_runs
            SET status='cancelled',finished_at=COALESCE(finished_at,?2),
                error=COALESCE(error,'Cancelled because the pulsation closed.')
          WHERE pulsation_id=?1 AND status IN ('pending','running')",
        params![pulsation_id, now],
    )?;
    tx.execute(
        "UPDATE pulsation_stage_verification
            SET state='cancelled',updated_at=?2
          WHERE pulsation_id=?1 AND state IN ('running','error')",
        params![pulsation_id, now],
    )?;
    tx.execute(
        "UPDATE verification_error_requests
            SET decision='abort',resolved_at=?2,updated_at=?2
          WHERE pulsation_id=?1 AND decision IS NULL",
        params![pulsation_id, now],
    )?;
    Ok(run_ids)
}

impl PersistenceService {
    /// Hooks a default stage's accepted result into stage verification when Labs
    /// `stage_verifiers_enabled` is ON and enabled verifiers exist for `current.stage`.
    /// Returns `Ok(false)` immediately when Labs is OFF or no enabled verifiers
    /// exist so default pipeline behavior is completely untouched (E8, P25).
    pub(crate) fn maybe_start_stage_verification_in_tx(
        tx: &Transaction<'_>,
        current: &Pulsation,
        stage_job_id: &str,
        stage_generation: u64,
        input_revision: u64,
        deferred_disposition: &str,
    ) -> Result<bool, PersistenceServiceError> {
        let settings = Self::composition_settings_for(tx, &current.project_id)?;
        if !settings.stage_verifiers_enabled {
            return Ok(false);
        }
        let stage_db = current.stage.as_db_value();
        let rows = tx
            .prepare(
                "SELECT id,label,position,reject_limit,agent_json,instructions,version
                   FROM stage_verifiers
                  WHERE project_id=?1 AND stage=?2 AND enabled=1
                  ORDER BY position,created_at,id",
            )?
            .query_map(params![current.project_id, stage_db], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        if rows.is_empty() {
            return Ok(false);
        }

        let now = now();
        tx.execute(
            "INSERT INTO pulsation_stage_verification(
               pulsation_id,stage,stage_generation,stage_job_id,input_revision,state,
               deferred_disposition,updated_at
             ) VALUES(?1,?2,?3,?4,?5,'running',?6,?7)",
            params![
                current.id,
                stage_db,
                stage_generation as i64,
                stage_job_id,
                input_revision as i64,
                deferred_disposition,
                now
            ],
        )?;

        for (id, label, position, reject_limit, agent_json, instructions, version) in rows {
            let draft_json = serde_json::json!({
                "stage": stage_db,
                "label": label,
                "position": position,
                "enabled": true,
                "rejectLimit": reject_limit,
                "agent": serde_json::from_str::<serde_json::Value>(&agent_json)
                    .map_err(|error| runtime(error.to_string()))?,
                "instructions": instructions,
            })
            .to_string();
            let draft =
                parse_verifier_draft(&draft_json).map_err(|error| runtime(error.message))?;
            let config = StageVerifierConfig::from_draft(&id, version, &draft);
            let config_json =
                serde_json::to_string(&config).map_err(|error| runtime(error.to_string()))?;
            tx.execute(
                "INSERT INTO stage_verifier_runs(
                   id,verifier_id,config_json,pulsation_id,stage,stage_job_id,stage_generation,
                   attempt,recovery_count,status,created_at
                 ) VALUES(?1,?2,?3,?4,?5,?6,?7,1,0,'pending',?8)",
                params![
                    Uuid::new_v4().to_string(),
                    id,
                    config_json,
                    current.id,
                    stage_db,
                    stage_job_id,
                    stage_generation as i64,
                    now
                ],
            )?;
        }

        if current.status == PulsationStatus::Checkpoint {
            Self::transition_pulsation(
                tx,
                current,
                PulsationStatus::Running,
                current.stage,
                "verification_started",
                Some("Stage verifiers queued."),
            )?;
        } else {
            insert_pulse_event(
                tx,
                &current.id,
                "verification_started",
                current.status,
                PulsationStatus::Running,
                current.stage,
                current.stage,
                Some("Stage verifiers queued."),
                &now,
            )?;
        }
        Ok(true)
    }

    /// Crash recovery for stage verifiers when `recover_automation_lane` runs on
    /// startup (P16):
    /// - 1st crash (`recovery_count == 0`) resets to `'pending'` and bumps `recovery_count`.
    /// - 2nd crash (`recovery_count >= 1`) fails to `'error'` and opens `verification_error`.
    pub(crate) fn recover_stage_verifiers_in_tx(
        tx: &Transaction<'_>,
        now: &str,
    ) -> Result<(), PersistenceServiceError> {
        let running = tx
            .prepare(
                "SELECT r.id,r.pulsation_id,r.stage,r.stage_generation,r.recovery_count,
                        p.status,v.state
                   FROM stage_verifier_runs r
                   JOIN pulsations p ON p.id=r.pulsation_id
                   LEFT JOIN pulsation_stage_verification v
                     ON v.pulsation_id=r.pulsation_id AND v.stage=r.stage
                    AND v.stage_generation=r.stage_generation
                  WHERE r.status='running'",
            )?
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        for (run_id, pulsation_id, stage_raw, _generation, recovery_count, pulse_status, v_state) in
            running
        {
            if pulse_status != "running" || v_state.as_deref() != Some("running") {
                tx.execute(
                    "UPDATE stage_verifier_runs
                        SET status='cancelled',finished_at=?2,
                            error=COALESCE(error,'Cancelled during startup recovery.')
                      WHERE id=?1",
                    params![run_id, now],
                )?;
                continue;
            }
            if recovery_count == 0 {
                tx.execute(
                    "UPDATE stage_verifier_runs
                        SET status='pending',recovery_count=recovery_count+1,started_at=NULL
                      WHERE id=?1",
                    [&run_id],
                )?;
            } else {
                let msg = "The app closed twice while this verifier was running.";
                tx.execute(
                    "UPDATE stage_verifier_runs
                        SET status='error',finished_at=?2,error=?3
                      WHERE id=?1",
                    params![run_id, now, msg],
                )?;
                if let Some(stage) = PulsationStage::from_db_value(&stage_raw) {
                    Self::open_verification_error_checkpoint_in_tx(
                        tx,
                        &pulsation_id,
                        stage,
                        &run_id,
                        msg,
                        now,
                    )?;
                }
            }
        }
        Ok(())
    }

    /// Admits the oldest pending verifier run whose stage publications are all
    /// `applied`, respecting the global `automation.max_concurrent` ceiling and
    /// the per-project cap of 1 running verifier (P18, P21).
    pub(crate) fn claim_next_verifier_run(
        &self,
    ) -> Result<Option<VerifierClaim>, PersistenceServiceError> {
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let running: u32 = tx.query_row("SELECT count(*) FROM automation_slots", [], |row| {
            row.get(0)
        })?;
        if running >= max_concurrent(&tx)? {
            return Ok(None);
        }

        struct ClaimedVerifierCandidate {
            run_id: String,
            verifier_id: String,
            config_json: String,
            project_id: String,
            pulsation_id: String,
            stage_raw: String,
            stage_generation: i64,
            attempt: i64,
            stage_job_id: String,
        }

        let candidate: Option<ClaimedVerifierCandidate> = tx
            .query_row(
                "SELECT r.id,r.verifier_id,r.config_json,p.project_id,r.pulsation_id,
                        r.stage,r.stage_generation,r.attempt,r.stage_job_id
                   FROM stage_verifier_runs r
                   JOIN pulsations p ON p.id=r.pulsation_id
                   JOIN pulsation_stage_verification v
                     ON v.pulsation_id=r.pulsation_id AND v.stage=r.stage
                    AND v.stage_generation=r.stage_generation
                  WHERE r.status='pending'
                    AND p.status='running'
                    AND v.state='running'
                    AND NOT EXISTS(
                      SELECT 1 FROM checkpoints c
                       WHERE c.pulsation_id=p.id AND c.status='waiting'
                    )
                    AND NOT EXISTS(
                      SELECT 1 FROM automation_slots a
                       WHERE a.project_id=p.project_id AND a.class='verification'
                    )
                    AND NOT EXISTS(
                      SELECT 1 FROM artifact_publications ap
                        JOIN artifacts a ON a.id=ap.artifact_id
                       WHERE ap.pulsation_id=r.pulsation_id
                         AND a.producer_job_id=r.stage_job_id AND a.acceptance='accepted'
                         AND ap.state IN ('pending','applying','conflict')
                    )
                  ORDER BY r.created_at,
                           COALESCE(json_extract(r.config_json,'$.position'),0),
                           r.id
                  LIMIT 1",
                [],
                |row| {
                    Ok(ClaimedVerifierCandidate {
                        run_id: row.get(0)?,
                        verifier_id: row.get(1)?,
                        config_json: row.get(2)?,
                        project_id: row.get(3)?,
                        pulsation_id: row.get(4)?,
                        stage_raw: row.get(5)?,
                        stage_generation: row.get(6)?,
                        attempt: row.get(7)?,
                        stage_job_id: row.get(8)?,
                    })
                },
            )
            .optional()?;

        let Some(ClaimedVerifierCandidate {
            run_id,
            verifier_id,
            config_json,
            project_id,
            pulsation_id,
            stage_raw,
            stage_generation,
            attempt,
            stage_job_id,
        }) = candidate
        else {
            return Ok(None);
        };

        let now = now();
        let stage = PulsationStage::from_db_value(&stage_raw)
            .ok_or_else(|| runtime("Invalid stage in verifier run."))?;
        let config = parse_verifier_config(&config_json).map_err(|error| runtime(error.message))?;
        let token = Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO automation_slots(token,class,project_id,owner_id,acquired_at)
             VALUES(?1,'verification',?2,?3,?4)",
            params![token, project_id, run_id, now],
        )?;
        tx.execute(
            "UPDATE stage_verifier_runs SET status='running',started_at=?2 WHERE id=?1",
            params![run_id, now],
        )?;
        tx.commit()?;
        Ok(Some(VerifierClaim {
            token,
            run_id,
            verifier_id,
            project_id,
            pulsation_id,
            stage,
            stage_job_id,
            stage_generation: stage_generation.max(1) as u64,
            attempt: attempt.max(1) as u64,
            config,
        }))
    }

    pub(crate) async fn run_verifier_run(&self, claim: VerifierClaim, shutdown: CancellationToken) {
        let key = verifier_cancellation_key(&claim.run_id);
        let token = CancellationToken::new();
        self.register_job_cancellation(&key, token.clone());
        let service = self.clone();
        let run_id = claim.run_id.clone();
        if let Ok(Ok(Some(status))) =
            tokio::task::spawn_blocking(move || service.verifier_run_status(&run_id)).await
            && status != "running"
        {
            token.cancel();
        }
        let result = {
            let work = self.execute_claimed_verifier(&claim, &token);
            tokio::pin!(work);
            tokio::select! {
                result = &mut work => result,
                _ = shutdown.cancelled() => {
                    token.cancel();
                    work.await
                }
            }
        };
        self.unregister_job_cancellation(&key);
        let stopping = shutdown.is_cancelled();
        let service = self.clone();
        match tokio::task::spawn_blocking(move || {
            service.finish_verifier_run(&claim, result, stopping)
        })
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => eprintln!("verifier run not recorded: {error}"),
            Err(error) => eprintln!("verifier run not recorded: {error}"),
        }
        self.wake_scheduler();
    }

    fn verifier_run_status(&self, run_id: &str) -> Result<Option<String>, PersistenceServiceError> {
        Ok(self
            .connection()?
            .query_row(
                "SELECT status FROM stage_verifier_runs WHERE id=?1",
                [run_id],
                |row| row.get(0),
            )
            .optional()?)
    }

    async fn execute_claimed_verifier(
        &self,
        claim: &VerifierClaim,
        token: &CancellationToken,
    ) -> VerifierRunResult {
        let service = self.clone();
        let owned = claim.clone();
        let context =
            match tokio::task::spawn_blocking(move || service.verifier_context(&owned)).await {
                Ok(Ok(context)) => context,
                Ok(Err(error)) => return VerifierRunResult::technical_error(error.to_string()),
                Err(error) => return VerifierRunResult::technical_error(error.to_string()),
            };

        let mut result = VerifierRunResult {
            outcome: Err(VerifierFailure::Cancelled),
            terminal_run_id: None,
            llm_execution_id: None,
        };
        let first = match self
            .call_verifier_agent(claim, &context.system, &context.user, 1, token)
            .await
        {
            Ok(call) => call,
            Err(failure) => {
                result.outcome = Err(failure);
                return result;
            }
        };
        result.record(&first);
        let verdict = match ikore_contracts::stage_verifier::parse_verdict(&first.raw) {
            Ok(verdict) => Ok(verdict),
            Err(problem) => {
                let user = verifier_correction_prompt(&context.user, &first.raw, &problem);
                match self
                    .call_verifier_agent(claim, &context.system, &user, 2, token)
                    .await
                {
                    Ok(second) => {
                        result.record(&second);
                        ikore_contracts::stage_verifier::parse_verdict(&second.raw).map_err(
                            |problem| {
                                VerifierFailure::TechnicalError(format!(
                                    "The verdict was invalid twice: {problem}"
                                ))
                            },
                        )
                    }
                    Err(failure) => Err(failure),
                }
            }
        };
        result.outcome = verdict.and_then(|verdict| {
            checked_verdict(verdict, &context.workspace)
                .map(VerifierOutcome::Verdict)
                .map_err(VerifierFailure::TechnicalError)
        });
        result
    }

    fn verifier_context(
        &self,
        claim: &VerifierClaim,
    ) -> Result<VerifierContext, PersistenceServiceError> {
        let db = self.connection()?;
        let workspace: String = db.query_row(
            "SELECT workspace_path FROM projects WHERE id=?1",
            [&claim.project_id],
            |row| row.get(0),
        )?;
        let artifacts = load_stage_result_artifacts(&db, &claim.stage_job_id)?;
        let task_scope = load_task_scope_text(&db, &claim.pulsation_id)?;
        let previous_verdict = load_previous_verdict_for_verifier(
            &db,
            &claim.pulsation_id,
            claim.stage,
            &claim.verifier_id,
            claim.stage_generation,
        )?;
        Ok(VerifierContext {
            system: verifier_system_prompt(&claim.config),
            user: verifier_user_prompt(
                &claim.config,
                claim.stage,
                &artifacts,
                task_scope.as_deref(),
                previous_verdict.as_ref(),
            ),
            workspace: PathBuf::from(workspace),
        })
    }

    async fn call_verifier_agent(
        &self,
        claim: &VerifierClaim,
        system: &str,
        user: &str,
        call: u32,
        token: &CancellationToken,
    ) -> Result<VerifierCall, VerifierFailure> {
        let agent = &claim.config.agent;
        let slot_id = verifier_slot_id(&claim.project_id, &claim.verifier_id);
        let deadline = Instant::now()
            + ikore_contracts::timeout::AgentTimeoutPolicy::automation().max_timeout();
        match agent.transport {
            StepTransport::Cli => {
                let terminals = self
                    .terminal_service
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone()
                    .ok_or_else(|| {
                        VerifierFailure::TechnicalError("The CLI runtime is not ready.".into())
                    })?;
                let backend = agent
                    .backend
                    .as_deref()
                    .and_then(CliBackend::parse)
                    .ok_or_else(|| {
                        VerifierFailure::TechnicalError(
                            "The verifier names no known official CLI.".into(),
                        )
                    })?;
                let completion = terminals
                    .execute_automation_step(
                        AutomationStepRequest {
                            project_id: claim.project_id.clone(),
                            slot_id,
                            label: verifier_tab_label(
                                claim.stage.as_db_value(),
                                &claim.config.label,
                            ),
                            origin: ExecutionOrigin::Verification,
                            pulsation_id: Some(claim.pulsation_id.clone()),
                            backend,
                            model: agent.model.clone(),
                            model_profile: agent.model_profile.clone(),
                            thinking_level: ThinkingLevel::parse(&agent.thinking_level)
                                .unwrap_or(ThinkingLevel::Default),
                            system_prompt: system.to_owned(),
                            user_prompt: user.to_owned(),
                            schema: Some(stage_verdict_json_schema()),
                            deadline,
                        },
                        token,
                    )
                    .await
                    .map_err(|error| match error {
                        TerminalError::Cancelled => VerifierFailure::Cancelled,
                        other => VerifierFailure::TechnicalError(other.to_string()),
                    })?;
                Ok(VerifierCall {
                    raw: completion.raw_output,
                    terminal_run_id: Some(completion.terminal_run_id),
                    llm_execution_id: None,
                })
            }
            StepTransport::Api => {
                let route_id = agent.api_route_id.clone().ok_or_else(|| {
                    VerifierFailure::TechnicalError("The verifier names no API route.".into())
                })?;
                let service = self.clone();
                let route_lookup = route_id.clone();
                let (config, supports_schema, supports_json_object) =
                    tokio::task::spawn_blocking(move || {
                        let config = service.load_runtime_config()?;
                        let db = service.connection()?;
                        let mut stmt = db.prepare(
                            "SELECT c.provider_id
                               FROM provider_route_targets t
                               JOIN provider_connections c ON c.id=t.connection_id
                              WHERE t.route_id=?1 AND t.enabled=1 AND c.enabled=1",
                        )?;
                        let providers = stmt
                            .query_map([&route_lookup], |row| row.get::<_, String>(0))?
                            .collect::<Result<Vec<_>, _>>()?;
                        let mut has_schema = false;
                        let mut has_json_object = false;
                        for provider_id in providers {
                            if let Some(profile) = crate::llm::profile_by_id(&provider_id) {
                                has_schema |= profile.capabilities.json_schema;
                                has_json_object |= profile.capabilities.json_object;
                            }
                        }
                        Ok::<_, PersistenceServiceError>((config, has_schema, has_json_object))
                    })
                    .await
                    .map_err(|error| VerifierFailure::TechnicalError(error.to_string()))?
                    .map_err(|error| VerifierFailure::TechnicalError(error.to_string()))?;
                let schema = stage_verdict_json_schema();
                let (effective_user_prompt, response_format) = if !supports_schema
                    && supports_json_object
                {
                    let schema_text = serde_json::to_string(&schema)
                        .map_err(|error| VerifierFailure::TechnicalError(error.to_string()))?;
                    (
                        format!(
                            "{user}\n\nReturn only one JSON object matching this schema:\n{schema_text}"
                        ),
                        ResponseFormat::JsonObject,
                    )
                } else {
                    (
                        user.to_owned(),
                        ResponseFormat::JsonSchema {
                            name: "stage_verdict_v1".into(),
                            schema,
                        },
                    )
                };
                let execution_id = Uuid::new_v4().to_string();
                let request = LlmRequest {
                    execution_id: ExecutionId::new(execution_id.clone()),
                    generation_id: GenerationId::new(format!(
                        "verifier:{}:{}:{call}",
                        claim.run_id, claim.attempt
                    )),
                    project_id: claim.project_id.clone(),
                    pulsation_id: Some(claim.pulsation_id.clone()),
                    job_id: Some(claim.stage_job_id.clone()),
                    agent_run_id: None,
                    job_chain: format!("verifier:{}", claim.run_id),
                    target: AgentExecutionTarget::automation(slot_id, route_id),
                    stage: claim.stage.as_db_value().into(),
                    system_prompt: system.to_owned(),
                    user_prompt: effective_user_prompt,
                    response_format,
                    max_output_tokens: config.max_output_tokens.min(AUTOMATION_OUTPUT_TOKENS),
                    temperature: config.temperature,
                    deadline,
                    stream: true,
                    legacy_provider: config.provider.as_str().into(),
                    legacy_model: config.model.clone(),
                    dispatch: DispatchPolicy::default(),
                };
                let response = self
                    .llm_router
                    .execute(&request, token)
                    .await
                    .map_err(|error| {
                        if error.kind == LlmErrorKind::Cancelled || token.is_cancelled() {
                            VerifierFailure::Cancelled
                        } else {
                            VerifierFailure::TechnicalError(error.to_string())
                        }
                    })?;
                Ok(VerifierCall {
                    raw: response.content,
                    terminal_run_id: None,
                    llm_execution_id: Some(execution_id),
                })
            }
        }
    }

    fn finish_verifier_run(
        &self,
        claim: &VerifierClaim,
        result: VerifierRunResult,
        stopping: bool,
    ) -> Result<(), PersistenceServiceError> {
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = now();
        tx.execute(
            "DELETE FROM automation_slots WHERE token=?1",
            [&claim.token],
        )?;
        tx.execute(
            "UPDATE stage_verifier_runs
                SET terminal_run_id=COALESCE(?2,terminal_run_id),
                    llm_execution_id=COALESCE(?3,llm_execution_id),
                    finished_at=?4
              WHERE id=?1",
            params![
                claim.run_id,
                result.terminal_run_id,
                result.llm_execution_id,
                now
            ],
        )?;

        let current_state: Option<(String, String, Option<String>)> = tx
            .query_row(
                "SELECT r.status,p.status,v.state
                   FROM stage_verifier_runs r
                   JOIN pulsations p ON p.id=r.pulsation_id
                   LEFT JOIN pulsation_stage_verification v
                     ON v.pulsation_id=r.pulsation_id AND v.stage=r.stage
                    AND v.stage_generation=r.stage_generation
                  WHERE r.id=?1",
                [&claim.run_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((run_status, pulse_status, verification_state)) = current_state else {
            tx.commit()?;
            return Ok(());
        };
        if run_status != "running"
            || pulse_status != "running"
            || verification_state.as_deref() != Some("running")
        {
            tx.execute(
                "UPDATE stage_verifier_runs SET status='cancelled' WHERE id=?1 AND status='running'",
                [&claim.run_id],
            )?;
            tx.commit()?;
            return Ok(());
        }

        match result.outcome {
            Err(VerifierFailure::Cancelled) if stopping => {
                let recovery_count: i64 = tx.query_row(
                    "SELECT recovery_count FROM stage_verifier_runs WHERE id=?1",
                    [&claim.run_id],
                    |row| row.get(0),
                )?;
                if recovery_count == 0 {
                    tx.execute(
                        "UPDATE stage_verifier_runs
                            SET status='pending',recovery_count=recovery_count+1,
                                started_at=NULL,finished_at=NULL
                          WHERE id=?1",
                        [&claim.run_id],
                    )?;
                } else {
                    let msg = "The app closed twice while this verifier was running.";
                    tx.execute(
                        "UPDATE stage_verifier_runs SET status='error',error=?2 WHERE id=?1",
                        params![claim.run_id, msg],
                    )?;
                    Self::open_verification_error_checkpoint_in_tx(
                        &tx,
                        &claim.pulsation_id,
                        claim.stage,
                        &claim.run_id,
                        msg,
                        &now,
                    )?;
                }
            }
            Err(VerifierFailure::Cancelled) => {
                tx.execute(
                    "UPDATE stage_verifier_runs SET status='cancelled' WHERE id=?1",
                    [&claim.run_id],
                )?;
            }
            Err(VerifierFailure::TechnicalError(message)) => {
                let clean = sanitized_observation_text(&message);
                tx.execute(
                    "UPDATE stage_verifier_runs SET status='error',error=?2 WHERE id=?1",
                    params![claim.run_id, clean],
                )?;
                Self::open_verification_error_checkpoint_in_tx(
                    &tx,
                    &claim.pulsation_id,
                    claim.stage,
                    &claim.run_id,
                    &clean,
                    &now,
                )?;
            }
            Ok(VerifierOutcome::Verdict(verdict)) => {
                let verdict_json =
                    serde_json::to_string(&verdict).map_err(|error| runtime(error.to_string()))?;
                let status = if verdict.verdict == Verdict::Pass {
                    "passed"
                } else {
                    "rejected"
                };
                tx.execute(
                    "UPDATE stage_verifier_runs SET status=?2,verdict_json=?3,error=NULL WHERE id=?1",
                    params![claim.run_id, status, verdict_json],
                )?;
                Self::settle_stage_verification_in_tx(
                    &tx,
                    &claim.pulsation_id,
                    claim.stage,
                    claim.stage_generation,
                    &now,
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn open_verification_error_checkpoint_in_tx(
        tx: &Transaction<'_>,
        pulsation_id: &str,
        stage: PulsationStage,
        verifier_run_id: &str,
        error_message: &str,
        now: &str,
    ) -> Result<(), PersistenceServiceError> {
        let Some(current) = Self::find_pulsation_by_id(tx, pulsation_id)? else {
            return Ok(());
        };
        if current.status != PulsationStatus::Running {
            return Ok(());
        }
        let (stage_job_id, stage_generation, input_revision): (String, u64, u64) = tx.query_row(
            "SELECT r.stage_job_id,r.stage_generation,v.input_revision FROM stage_verifier_runs r
               JOIN pulsation_stage_verification v
                 ON v.pulsation_id=r.pulsation_id AND v.stage=r.stage
                AND v.stage_generation=r.stage_generation
              WHERE r.id=?1",
            [verifier_run_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        tx.execute(
            "UPDATE pulsation_stage_verification SET state='error',updated_at=?4
              WHERE pulsation_id=?1 AND stage=?2 AND stage_generation=?3",
            params![
                pulsation_id,
                stage.as_db_value(),
                stage_generation as i64,
                now
            ],
        )?;
        let updated = Self::transition_pulsation(
            tx,
            &current,
            PulsationStatus::Checkpoint,
            stage,
            "verification_error",
            Some("A stage verifier encountered a technical error and waits for a decision."),
        )?;
        let state_version: u64 = tx.query_row(
            "SELECT state_version FROM pulsations WHERE id=?1",
            [&updated.id],
            |row| row.get(0),
        )?;
        let checkpoint_id = Uuid::new_v4().to_string();
        let clean_error = sanitized_observation_text(error_message);
        tx.execute(
            "INSERT INTO checkpoints(
               id,pulsation_id,stage,status,created_at,updated_at,kind,input_revision,
               state_version,producer_job_id,generation,purpose
             ) VALUES(?1,?2,?3,'waiting',?4,?4,'verification_error',?5,?6,?7,?8,'verification_error')",
            params![
                checkpoint_id,
                pulsation_id,
                stage.as_db_value(),
                now,
                input_revision as i64,
                state_version as i64,
                stage_job_id,
                stage_generation as i64
            ],
        )?;
        tx.execute(
            "INSERT INTO verification_error_requests(
               checkpoint_id,pulsation_id,verifier_run_id,stage,error_message,created_at,updated_at
             ) VALUES(?1,?2,?3,?4,?5,?6,?6)",
            params![
                checkpoint_id,
                pulsation_id,
                verifier_run_id,
                stage.as_db_value(),
                clean_error,
                now
            ],
        )?;
        Ok(())
    }

    /// Evaluates whether all verifiers for `(pulsation_id, stage, stage_generation)`
    /// have finished and applies the outcome: advance/review, auto re-run, or
    /// open `stage_failure (VerificationRejected)` (P18, P19, P20).
    pub(crate) fn settle_stage_verification_in_tx(
        tx: &Transaction<'_>,
        pulsation_id: &str,
        stage: PulsationStage,
        stage_generation: u64,
        now: &str,
    ) -> Result<(), PersistenceServiceError> {
        let Some(current) = Self::find_pulsation_by_id(tx, pulsation_id)? else {
            return Ok(());
        };
        if current.status != PulsationStatus::Running {
            return Ok(());
        }
        let Some(disposition) =
            load_deferred_disposition(tx, pulsation_id, stage, stage_generation)?
        else {
            return Ok(());
        };

        // Load all runs for this generation and keep only the latest attempt per verifier.
        let rows = tx
            .prepare(
                "SELECT verifier_id,attempt,status,config_json,verdict_json
                   FROM stage_verifier_runs
                  WHERE pulsation_id=?1 AND stage=?2 AND stage_generation=?3
                  ORDER BY COALESCE(json_extract(config_json,'$.position'),0),created_at,attempt DESC",
            )?
            .query_map(
                params![pulsation_id, stage.as_db_value(), stage_generation as i64],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;

        let mut latest: Vec<(
            String,
            i64,
            String,
            StageVerifierConfig,
            Option<StageVerdictV1>,
        )> = Vec::new();
        for (verifier_id, attempt, status, config_json, verdict_json) in rows {
            if let Some(existing) = latest
                .iter_mut()
                .find(|(existing_id, _, _, _, _)| existing_id == &verifier_id)
            {
                if attempt > existing.1 {
                    let config = parse_verifier_config(&config_json)
                        .map_err(|error| runtime(error.message))?;
                    let verdict = verdict_json
                        .as_deref()
                        .and_then(|json| serde_json::from_str::<StageVerdictV1>(json).ok());
                    *existing = (verifier_id, attempt, status, config, verdict);
                }
            } else {
                let config =
                    parse_verifier_config(&config_json).map_err(|error| runtime(error.message))?;
                let verdict = verdict_json
                    .as_deref()
                    .and_then(|json| serde_json::from_str::<StageVerdictV1>(json).ok());
                latest.push((verifier_id, attempt, status, config, verdict));
            }
        }

        if latest
            .iter()
            .any(|(_, _, status, _, _)| matches!(status.as_str(), "pending" | "running" | "error"))
        {
            return Ok(());
        }

        let mut non_advisory_rejections: Vec<(StageVerifierConfig, StageVerdictV1)> = Vec::new();
        for (_, _, status, config, verdict) in &latest {
            if status == "rejected" && !config.is_advisory() {
                let verdict = verdict.clone().unwrap_or(StageVerdictV1 {
                    verdict: Verdict::Reject,
                    summary: "Rejected by verifier.".into(),
                    findings: Vec::new(),
                });
                non_advisory_rejections.push((config.clone(), verdict));
            }
        }

        if non_advisory_rejections.is_empty() {
            tx.execute(
                "UPDATE pulsation_stage_verification SET state='passed',updated_at=?4
                  WHERE pulsation_id=?1 AND stage=?2 AND stage_generation=?3",
                params![
                    pulsation_id,
                    stage.as_db_value(),
                    stage_generation as i64,
                    now
                ],
            )?;
            Self::apply_deferred_disposition_in_tx(
                tx,
                &current,
                &disposition,
                "verification_passed",
                now,
            )?;
            return Ok(());
        }

        tx.execute(
            "UPDATE pulsation_stage_verification SET state='rejected',updated_at=?4
              WHERE pulsation_id=?1 AND stage=?2 AND stage_generation=?3",
            params![
                pulsation_id,
                stage.as_db_value(),
                stage_generation as i64,
                now
            ],
        )?;

        let mut limit_exhausted = false;
        for (config, _) in &non_advisory_rejections {
            let rejections = count_verifier_rejections_since_revision(
                tx,
                pulsation_id,
                stage,
                &config.verifier_id,
            )?;
            if rejections > u64::from(config.reject_limit) {
                limit_exhausted = true;
                break;
            }
        }

        if !limit_exhausted {
            Self::requeue_stage_for_verification_rejection_in_tx(tx, &current, stage, now)?;
            return Ok(());
        }

        // Reject limit exhausted: open stage_failure (`VerificationRejected`).
        let diagnostic = format_rejection_diagnostic(&non_advisory_rejections);
        let stage_execution_id: Option<String> = tx
            .query_row(
                "SELECT stage_execution_id FROM stage_acceptances WHERE job_id=?1 AND generation=?2",
                params![disposition.stage_job_id, stage_generation as i64],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        let is_v4 = Self::composed_pulsation(tx, pulsation_id)?;
        let offered = if is_v4 {
            crate::orchestration::offered_decisions_v4(
                FailureClass::VerificationRejected,
                stage,
                0,
                false,
            )
        } else {
            None
        };
        let targets: Vec<&str> = match &offered {
            Some(v4) => v4
                .revision_targets
                .iter()
                .map(|target| target.as_db_value())
                .collect(),
            None => vec![stage.as_db_value()],
        };

        Self::transition_pulsation(
            tx,
            &current,
            PulsationStatus::Checkpoint,
            stage,
            "verification_rejected",
            Some(&diagnostic),
        )?;
        let (state_version, input_revision): (u64, u64) = tx.query_row(
            "SELECT state_version,input_revision FROM pulsations WHERE id=?1",
            [pulsation_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let checkpoint_id = Uuid::new_v4().to_string();
        let targets_json =
            serde_json::to_string(&targets).map_err(|error| runtime(error.to_string()))?;
        tx.execute(
            "INSERT INTO checkpoints(
               id,pulsation_id,stage,status,created_at,updated_at,kind,input_revision,
               state_version,producer_job_id,generation,purpose
             ) VALUES(?1,?2,?3,'waiting',?4,?4,'stage_failure',?5,?6,?7,?8,'stage_failure')",
            params![
                checkpoint_id,
                pulsation_id,
                stage.as_db_value(),
                now,
                input_revision as i64,
                state_version as i64,
                disposition.stage_job_id,
                stage_generation as i64
            ],
        )?;
        tx.execute(
            "INSERT INTO stage_failure_requests(
               checkpoint_id,pulsation_id,job_id,stage,failure_class,allow_retry,allow_revision,
               allow_continue_anyway,revision_stage,error_code,diagnostic,agent_run_id,
               created_at,updated_at,stage_execution_id,allowed_revision_targets_json,
               remaining_budget_json
             ) VALUES(?1,?2,?3,?4,?5,1,1,1,?6,'verification_rejected',?7,NULL,?8,?8,?9,?10,NULL)",
            params![
                checkpoint_id,
                pulsation_id,
                disposition.stage_job_id,
                stage.as_db_value(),
                FailureClass::VerificationRejected.as_db_value(),
                targets.first().copied().unwrap_or(stage.as_db_value()),
                diagnostic,
                now,
                stage_execution_id,
                targets_json
            ],
        )?;
        Ok(())
    }

    fn requeue_stage_for_verification_rejection_in_tx(
        tx: &Transaction<'_>,
        current: &Pulsation,
        stage: PulsationStage,
        now: &str,
    ) -> Result<(), PersistenceServiceError> {
        let composed = Self::composed_pulsation(tx, &current.id)?;
        if composed {
            Self::supersede_stage_executions(tx, &current.id, stage)?;
        }
        let target_index = crate::orchestration::catalog::STAGES
            .iter()
            .position(|(candidate, _, _)| *candidate == stage)
            .ok_or_else(|| runtime("Unknown pulsation stage."))?;
        for (_, stage_value, _) in &crate::orchestration::catalog::STAGES[target_index..] {
            tx.execute(
                "UPDATE artifacts SET acceptance='superseded'
                  WHERE pulsation_id=?1 AND producer_stage=?2",
                params![current.id, stage_value],
            )?;
        }
        tx.execute(
            "UPDATE artifact_publications SET state='cancelled'
              WHERE pulsation_id=?1 AND state IN ('pending','applying')
                AND artifact_id IN (SELECT id FROM artifacts WHERE acceptance='superseded')",
            [&current.id],
        )?;
        let _ = now;
        let updated = Self::transition_pulsation(
            tx,
            current,
            PulsationStatus::Planning,
            stage,
            "verification_rejected",
            Some("A stage verifier rejected the candidate; running the stage again with findings."),
        )?;
        Self::enqueue_stage_job(tx, &updated.id, stage)?;
        Ok(())
    }

    fn apply_deferred_disposition_in_tx(
        tx: &Transaction<'_>,
        current: &Pulsation,
        disposition: &DeferredDispositionRow,
        event_before: &str,
        now: &str,
    ) -> Result<Pulsation, PersistenceServiceError> {
        if event_before == "verification_passed" {
            insert_pulse_event(
                tx,
                &current.id,
                "verification_passed",
                current.status,
                current.status,
                disposition.stage,
                disposition.stage,
                Some("All stage verifiers passed."),
                now,
            )?;
        }
        if disposition.deferred_disposition == "await_review" {
            let updated = Self::transition_pulsation(
                tx,
                current,
                PulsationStatus::Checkpoint,
                disposition.stage,
                "checkpoint_ready",
                Some("Checkpoint ready for HITL review."),
            )?;
            Self::open_checkpoint(tx, &updated.id, updated.stage)?;
            tx.execute(
                "UPDATE checkpoints
                    SET state_version=(SELECT state_version FROM pulsations WHERE id=?1),
                        input_revision=?2,producer_job_id=?3,generation=?4
                  WHERE pulsation_id=?1 AND status='waiting' AND purpose='artifact_review'",
                params![
                    current.id,
                    disposition.input_revision as i64,
                    disposition.stage_job_id,
                    disposition.stage_generation as i64
                ],
            )?;
            Self::enqueue_visualization(tx, &current.id)?;
            return Ok(updated);
        }
        let next_raw = disposition
            .deferred_disposition
            .strip_prefix("advance:")
            .ok_or_else(|| runtime("Invalid deferred stage verification disposition."))?;
        let next = PulsationStage::from_db_value(next_raw)
            .ok_or_else(|| runtime("Invalid target stage in deferred verification disposition."))?;
        let updated = Self::transition_pulsation(
            tx,
            current,
            PulsationStatus::Planning,
            next,
            "stage_auto_advanced",
            Some("Result accepted; waiting for required publications."),
        )?;
        Self::enqueue_stage_job(tx, &updated.id, next)?;
        Ok(updated)
    }

    /// Validates, sanitizes, and applies `ContinueAnyway` on a
    /// `VerificationRejected` failure checkpoint (R6, P17, P20). Returns the
    /// sanitized optional justification so the caller can pass it to
    /// `ApprovalReceiptV1` (which hashes it into `feedback_sha256` without
    /// storing raw text in the receipt/attestation).
    pub(crate) fn sanitize_continue_anyway_justification(
        raw_justification: Option<&str>,
    ) -> Result<Option<String>, PersistenceServiceError> {
        let trimmed = raw_justification.map(str::trim).unwrap_or_default();
        if trimmed.is_empty() {
            return Ok(None);
        }
        if trimmed.chars().count() > MAX_JUSTIFICATION_CHARS {
            return Err(invalid(format!(
                "Justification cannot exceed {MAX_JUSTIFICATION_CHARS} characters."
            )));
        }
        Ok(Some(sanitized_observation_text(trimmed)))
    }

    pub(crate) fn apply_verification_continue_anyway_in_tx(
        tx: &Transaction<'_>,
        checkpoint_id: &str,
        current: &Pulsation,
        stage: PulsationStage,
        justification: Option<&str>,
        now: &str,
    ) -> Result<Pulsation, PersistenceServiceError> {
        let stage_generation: u64 = tx.query_row(
            "SELECT generation FROM checkpoints WHERE id=?1",
            [checkpoint_id],
            |row| row.get::<_, i64>(0).map(|v| v.max(1) as u64),
        )?;
        let disposition = load_deferred_disposition(tx, &current.id, stage, stage_generation)?
            .ok_or_else(|| {
                runtime("Deferred verification disposition missing for Continue anyway.")
            })?;
        tx.execute(
            "UPDATE stage_failure_requests SET justification=?2 WHERE checkpoint_id=?1",
            params![checkpoint_id, justification],
        )?;
        tx.execute(
            "UPDATE stage_verifier_runs
                SET status='overridden'
              WHERE pulsation_id=?1 AND stage=?2 AND stage_generation=?3 AND status='rejected'",
            params![current.id, stage.as_db_value(), stage_generation as i64],
        )?;
        tx.execute(
            "UPDATE pulsation_stage_verification
                SET state='overridden',updated_at=?4
              WHERE pulsation_id=?1 AND stage=?2 AND stage_generation=?3",
            params![
                current.id,
                stage.as_db_value(),
                stage_generation as i64,
                now
            ],
        )?;
        let event_message = match justification {
            Some(text) => format!("Verification overridden by developer: {text}"),
            None => "Verification overridden by developer.".to_owned(),
        };
        let running = Self::transition_pulsation(
            tx,
            current,
            PulsationStatus::Running,
            stage,
            "verification_overridden",
            Some(&event_message),
        )?;
        Self::apply_deferred_disposition_in_tx(
            tx,
            &running,
            &disposition,
            "verification_overridden",
            now,
        )
    }

    /// Resolves `Retry` on a `VerificationRejected` failure checkpoint (P20):
    /// supersedes the stage, enqueues a new generation with `## Verification
    /// findings` and gives +1 round without resetting the rejection counter.
    pub(crate) fn apply_verification_retry_in_tx(
        tx: &Transaction<'_>,
        current: &Pulsation,
        stage: PulsationStage,
    ) -> Result<Pulsation, PersistenceServiceError> {
        let composed = Self::composed_pulsation(tx, &current.id)?;
        if composed {
            Self::supersede_stage_executions(tx, &current.id, stage)?;
        }
        let target_index = crate::orchestration::catalog::STAGES
            .iter()
            .position(|(candidate, _, _)| *candidate == stage)
            .ok_or_else(|| runtime("Unknown pulsation stage."))?;
        for (_, stage_value, _) in &crate::orchestration::catalog::STAGES[target_index..] {
            tx.execute(
                "UPDATE artifacts SET acceptance='superseded'
                  WHERE pulsation_id=?1 AND producer_stage=?2",
                params![current.id, stage_value],
            )?;
        }
        tx.execute(
            "UPDATE artifact_publications SET state='cancelled'
              WHERE pulsation_id=?1 AND state IN ('pending','applying')
                AND artifact_id IN (SELECT id FROM artifacts WHERE acceptance='superseded')",
            [&current.id],
        )?;
        let updated = Self::transition_pulsation(
            tx,
            current,
            PulsationStatus::Planning,
            stage,
            "stage_failure_retry",
            Some("Stage retry requested after verifier rejection (+1 round)."),
        )?;
        Self::enqueue_stage_job(tx, &updated.id, stage)?;
        Ok(updated)
    }

    /// Cancels any pending/running verifier runs and marks active verification
    /// rows cancelled inside an existing transaction. Returns run IDs whose
    /// in-flight cancellation tokens should be triggered after commit.
    pub(crate) fn cancel_stage_verifiers_in_tx(
        tx: &Transaction<'_>,
        pulsation_id: &str,
        now: &str,
    ) -> Result<Vec<String>, PersistenceServiceError> {
        cancel_active_verifiers_and_collect(tx, pulsation_id, now)
    }

    /// Signals cancellation tokens for verifier runs after commit.
    pub(crate) fn cancel_verifier_tokens(&self, run_ids: &[String]) {
        for run_id in run_ids {
            self.request_job_cancellation(&verifier_cancellation_key(run_id));
        }
    }

    pub(crate) fn open_verification_error_checkpoints_for_project(
        db: &Connection,
        project_id: &str,
    ) -> Result<Vec<VerificationErrorCheckpoint>, PersistenceServiceError> {
        let mut stmt = db.prepare(
            "SELECT c.id,c.pulsation_id,p.project_id,p.feature_name,e.verifier_run_id,
                    r.verifier_id,r.config_json,COALESCE(v.label,'Verifier'),
                    e.stage,e.error_message,r.attempt,p.state_version,c.created_at
               FROM verification_error_requests e
               JOIN checkpoints c ON c.id=e.checkpoint_id
               JOIN pulsations p ON p.id=e.pulsation_id
               JOIN stage_verifier_runs r ON r.id=e.verifier_run_id
               LEFT JOIN stage_verifiers v ON v.id=r.verifier_id
              WHERE p.project_id=?1 AND p.status='checkpoint' AND c.status='waiting'
                AND c.purpose='verification_error' AND e.decision IS NULL
              ORDER BY c.created_at ASC,c.id ASC",
        )?;
        let rows = stmt
            .query_map([project_id], |row| {
                let stage_raw: String = row.get(8)?;
                let config_json: String = row.get(6)?;
                let label_fallback: String = row.get(7)?;
                let verifier_label = parse_verifier_config(&config_json)
                    .map(|c| c.label)
                    .unwrap_or(label_fallback);
                let stage = PulsationStage::from_db_value(&stage_raw)
                    .ok_or_else(|| Self::invalid_enum_error("stage", &stage_raw))?;
                Ok(VerificationErrorCheckpoint {
                    checkpoint_id: row.get(0)?,
                    pulsation_id: row.get(1)?,
                    project_id: row.get(2)?,
                    feature_name: row.get(3)?,
                    verifier_run_id: row.get(4)?,
                    verifier_id: row.get(5)?,
                    verifier_label,
                    stage,
                    error_message: row.get(9)?,
                    attempt: row.get::<_, i64>(10)?.max(1) as u64,
                    state_version: row.get(11)?,
                    created_at: row.get(12)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Returns the open `verification_error` checkpoint for `pulsation_id`, if any.
    pub fn get_open_verification_error_checkpoint(
        &self,
        pulsation_id: &str,
    ) -> Result<Option<VerificationErrorCheckpoint>, PersistenceServiceError> {
        let pulsation_id = Self::normalize_pulsation_id(pulsation_id)?;
        let db = self.connection()?;
        let project_id: Option<String> = db
            .query_row(
                "SELECT project_id FROM pulsations WHERE id=?1",
                [&pulsation_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(project_id) = project_id else {
            return Err(PersistenceServiceError::NotFound(
                "Pulsation not found.".into(),
            ));
        };
        Ok(
            Self::open_verification_error_checkpoints_for_project(&db, &project_id)?
                .into_iter()
                .find(|cp| cp.pulsation_id == pulsation_id),
        )
    }

    /// Resolves a waiting `verification_error` checkpoint (`RetryVerifier`,
    /// `SkipVerifier`, or `Abort`), recording `ApprovalReceiptV1` on every
    /// decision (P15, P17, P25).
    pub fn resolve_verification_error_checkpoint(
        &self,
        request: ResolveVerificationErrorRequest,
    ) -> Result<ResolveVerificationErrorResponse, PersistenceServiceError> {
        let checkpoint_id = request.checkpoint_id.trim();
        if checkpoint_id.is_empty() {
            return Err(invalid("checkpointId cannot be empty."));
        }
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let found: Option<(String, String, String, String, String, i64, i64)> = tx
            .query_row(
                "SELECT c.pulsation_id,e.verifier_run_id,e.stage,r.verifier_id,r.config_json,
                        r.stage_generation,r.attempt
                   FROM verification_error_requests e
                   JOIN checkpoints c ON c.id=e.checkpoint_id
                   JOIN pulsations p ON p.id=e.pulsation_id
                   JOIN stage_verifier_runs r ON r.id=e.verifier_run_id
                  WHERE c.id=?1 AND c.purpose='verification_error' AND c.status='waiting'
                    AND e.decision IS NULL AND p.status='checkpoint'
                    AND c.state_version=?2 AND p.state_version=?2
                    AND c.input_revision=p.input_revision",
                params![checkpoint_id, request.expected_state_version as i64],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            pulsation_id,
            verifier_run_id,
            stage_raw,
            verifier_id,
            config_json,
            stage_generation_i64,
            attempt,
        )) = found
        else {
            return Err(invalid(
                "The verification error checkpoint is missing or stale. Refresh before deciding.",
            ));
        };
        let waiting: i64 = tx.query_row(
            "SELECT COUNT(*) FROM checkpoints WHERE pulsation_id=?1 AND status='waiting'",
            [&pulsation_id],
            |row| row.get(0),
        )?;
        if waiting != 1 {
            return Err(invalid(
                "The verification error checkpoint is ambiguous. Refresh before deciding.",
            ));
        }
        let stage = PulsationStage::from_db_value(&stage_raw)
            .ok_or_else(|| runtime("Invalid stage in verification error checkpoint."))?;
        let current = Self::find_pulsation_by_id(&tx, &pulsation_id)?
            .ok_or_else(|| PersistenceServiceError::NotFound("Pulsation not found.".into()))?;
        if current.stage != stage {
            return Err(invalid(
                "The pulsation stage changed; refresh the checkpoint.",
            ));
        }

        let now = now();
        let checkpoint_status = match request.decision {
            VerificationErrorDecision::RetryVerifier | VerificationErrorDecision::SkipVerifier => {
                "approved"
            }
            VerificationErrorDecision::Abort => "rejected",
        };
        tx.execute(
            "UPDATE checkpoints SET status=?2,updated_at=?3 WHERE id=?1",
            params![checkpoint_id, checkpoint_status, now],
        )?;
        tx.execute(
            "UPDATE verification_error_requests
                SET decision=?2,resolved_at=?3,updated_at=?3
              WHERE checkpoint_id=?1",
            params![checkpoint_id, request.decision.as_db_value(), now],
        )?;
        Self::record_approval_receipt(
            &tx,
            ReceiptInput {
                checkpoint_id,
                pulsation_id: &pulsation_id,
                purpose: "verification_error",
                stage: stage.as_db_value(),
                decision: request.decision.as_db_value(),
                state_version: request.expected_state_version,
                reviewed: Vec::new(),
                confirmed_paths: Vec::new(),
                feedback: None,
            },
        )?;

        let stage_generation = stage_generation_i64.max(1) as u64;
        let mut cancellation_ids = Vec::new();
        let updated = match request.decision {
            VerificationErrorDecision::RetryVerifier => {
                let stage_job_id: String = tx.query_row(
                    "SELECT stage_job_id FROM stage_verifier_runs WHERE id=?1",
                    [&verifier_run_id],
                    |row| row.get(0),
                )?;
                tx.execute(
                    "INSERT INTO stage_verifier_runs(
                       id,verifier_id,config_json,pulsation_id,stage,stage_job_id,
                       stage_generation,attempt,recovery_count,status,created_at
                     ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,0,'pending',?9)",
                    params![
                        Uuid::new_v4().to_string(),
                        verifier_id,
                        config_json,
                        pulsation_id,
                        stage.as_db_value(),
                        stage_job_id,
                        stage_generation_i64,
                        attempt + 1,
                        now
                    ],
                )?;
                tx.execute(
                    "UPDATE pulsation_stage_verification
                        SET state='running',updated_at=?4
                      WHERE pulsation_id=?1 AND stage=?2 AND stage_generation=?3",
                    params![pulsation_id, stage.as_db_value(), stage_generation_i64, now],
                )?;
                Self::transition_pulsation(
                    &tx,
                    &current,
                    PulsationStatus::Running,
                    stage,
                    "verification_error_retried",
                    Some("Verifier retry requested by the user."),
                )?
            }
            VerificationErrorDecision::SkipVerifier => {
                tx.execute(
                    "UPDATE stage_verifier_runs SET status='skipped' WHERE id=?1",
                    [&verifier_run_id],
                )?;
                tx.execute(
                    "UPDATE pulsation_stage_verification
                        SET state='running',updated_at=?4
                      WHERE pulsation_id=?1 AND stage=?2 AND stage_generation=?3",
                    params![pulsation_id, stage.as_db_value(), stage_generation_i64, now],
                )?;
                let running = Self::transition_pulsation(
                    &tx,
                    &current,
                    PulsationStatus::Running,
                    stage,
                    "verification_error_skipped",
                    Some("Verifier skipped for this stage generation by the user."),
                )?;
                Self::settle_stage_verification_in_tx(
                    &tx,
                    &pulsation_id,
                    stage,
                    stage_generation,
                    &now,
                )?;
                Self::find_pulsation_by_id(&tx, &running.id)?.unwrap_or(running)
            }
            VerificationErrorDecision::Abort => {
                cancellation_ids = cancel_active_verifiers_and_collect(&tx, &pulsation_id, &now)?;
                tx.execute(
                    "UPDATE jobs
                        SET status='failed',cancellation_reason='user_cancel',
                            cancel_requested_at=?2,finished_at=?2,updated_at=?2
                      WHERE pulsation_id=?1 AND status IN ('pending','running','interrupted')",
                    params![pulsation_id, now],
                )?;
                let mut failed = Self::transition_pulsation(
                    &tx,
                    &current,
                    PulsationStatus::Failed,
                    stage,
                    "verification_error_aborted",
                    Some("Pulsation aborted at the verification error checkpoint."),
                )?;
                tx.execute(
                    "UPDATE pulsations SET terminal_reason='verification_error' WHERE id=?1",
                    [&pulsation_id],
                )?;
                failed.terminal_reason = Some("verification_error".to_owned());
                Self::mark_effects_for_review(
                    &tx,
                    &pulsation_id,
                    "verification_error_aborted_with_effects",
                )?;
                tx.execute(
                    "UPDATE artifact_publications SET state='cancelled'
                      WHERE pulsation_id=?1 AND state IN ('pending','applying')",
                    [&pulsation_id],
                )?;
                Self::enqueue_learning(&tx, &pulsation_id)?;
                Self::cancel_stage_executions(&tx, &pulsation_id)?;
                failed
            }
        };

        tx.commit()?;
        self.cancel_verifier_tokens(&cancellation_ids);
        self.wake_scheduler();
        Ok(ResolveVerificationErrorResponse { pulsation: updated })
    }
}
