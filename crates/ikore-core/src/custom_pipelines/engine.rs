//! Feature 25 (M2a): the automation lane, the supervisor's third (E3). It
//! admits pipeline steps through `automation_slots`, never through the main
//! lane's slot (T12): one step per project and `automation.max_concurrent`
//! in the whole app. Its steps run in a `JoinSet` the lane joins before it
//! returns, so no future is ever detached. A module of `persistence_service`.

use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use rusqlite::{OptionalExtension, TransactionBehavior, params};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::custom_pipeline_store::{
    automation_cancellation_key, load_step_runs, max_concurrent, open_next_step,
};
use super::{PersistenceService, PersistenceServiceError, sleep};
use crate::custom_pipelines::prompt::{self, GateNote, PreviousOutput};
use crate::custom_pipelines::{
    AgentStep, CustomPipelineDefinitionV1, HostTool, HostToolStep, PipelineStep, StageVerdictV1,
    StepOutput, StepTransport, Verdict, automation_slot_id, automation_slot_label,
    parse_definition, parse_verdict, stage_verdict_json_schema,
};
use crate::git_tool::{
    CommitPlanV1, GitOperationStatus, PulsationCommitQueueState, RuleViolation,
    execute_commit_for_pulsation, execute_pull_for_project, execute_push_for_project,
};
use crate::llm::{
    AgentExecutionTarget, DispatchPolicy, ExecutionId, GenerationId, LlmErrorKind, LlmGateway,
    LlmRequest, ResponseFormat,
};
use crate::terminals::{
    AUTOMATION_OUTPUT_TOKENS, AutomationStepRequest, CliBackend, ExecutionOrigin, TerminalError,
    ThinkingLevel,
};

const INTERRUPTED: &str = "The app closed while this step ran.";

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// A step the lane admitted: where it is, its place's token and what it runs.
#[derive(Debug, Clone)]
pub(super) struct AutomationClaim {
    token: String,
    pub(super) run_id: String,
    pub(super) step_index: u32,
    pub(super) attempt: u32,
    pub(super) project_id: String,
    pipeline_id: String,
    pub(super) pipeline_name: String,
    pub(super) definition: CustomPipelineDefinitionV1,
    step: PipelineStep,
}

impl AutomationClaim {
    pub(super) fn slot_owner_id(&self) -> String {
        format!("{}:{}:{}", self.run_id, self.step_index, self.attempt)
    }
}

/// What the step's agent reads, and the workspace its findings cite.
struct StepContext {
    system: String,
    user: String,
    workspace: PathBuf,
}

/// One answer of the step's agent.
struct StepCall {
    raw: String,
    terminal_run_id: Option<String>,
    llm_execution_id: Option<String>,
}

pub(super) enum StepOutcome {
    Text(String),
    Verdict(StageVerdictV1),
}

pub(super) enum StepFailure {
    Cancelled,
    Failed(String),
}

pub(super) struct StepResult {
    pub(super) outcome: Result<StepOutcome, StepFailure>,
    pub(super) terminal_run_id: Option<String>,
    pub(super) llm_execution_id: Option<String>,
    pub(super) git_operation_id: Option<String>,
}

impl StepResult {
    pub(super) fn failed(message: impl Into<String>) -> Self {
        Self {
            outcome: Err(StepFailure::Failed(message.into())),
            terminal_run_id: None,
            llm_execution_id: None,
            git_operation_id: None,
        }
    }

    pub(super) fn cancelled() -> Self {
        Self {
            outcome: Err(StepFailure::Cancelled),
            terminal_run_id: None,
            llm_execution_id: None,
            git_operation_id: None,
        }
    }

    pub(super) fn with_failure(failure: StepFailure) -> Self {
        Self {
            outcome: Err(failure),
            terminal_run_id: None,
            llm_execution_id: None,
            git_operation_id: None,
        }
    }

    fn record(&mut self, call: &StepCall) {
        if call.terminal_run_id.is_some() {
            self.terminal_run_id.clone_from(&call.terminal_run_id);
        }
        if call.llm_execution_id.is_some() {
            self.llm_execution_id.clone_from(&call.llm_execution_id);
        }
    }
}

/// Model text for the screen and the next step (ADR-030).
fn sanitized(text: &str) -> Result<String, String> {
    crate::llm::adapter::sanitize_model_text(text).map_err(|error| error.to_string())
}

/// A path a finding may cite: relative, inside the workspace, and there.
fn workspace_file(workspace: &Path, file: &str) -> Option<String> {
    let relative = file.trim().replace('\\', "/");
    let path = Path::new(&relative);
    let inside = !relative.is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir));
    (inside && workspace.join(path).exists()).then_some(relative)
}

/// The verdict as the host keeps it: its text sanitized, and a cited path
/// the workspace lacks dropped and marked (contract `stage-verdict-v1`).
pub(crate) fn checked_verdict(
    mut verdict: StageVerdictV1,
    workspace: &Path,
) -> Result<StageVerdictV1, String> {
    verdict.summary = sanitized(&verdict.summary)?;
    for finding in &mut verdict.findings {
        finding.message = sanitized(&finding.message)?;
        finding.suggestion = finding.suggestion.as_deref().map(sanitized).transpose()?;
        if let Some(file) = finding.file.take() {
            match workspace_file(workspace, &file) {
                Some(file) => finding.file = Some(file),
                None => {
                    finding.file_not_found = true;
                    finding.line = None;
                }
            }
        }
    }
    Ok(verdict)
}

impl PersistenceService {
    /// The lane: recovers what a closed app left, then admits steps while
    /// the ceilings allow and waits for one to finish or for a wakeup.
    pub(super) async fn run_automation_lane(&self, shutdown: &CancellationToken) {
        let service = self.clone();
        match tokio::task::spawn_blocking(move || service.recover_automation_lane()).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => eprintln!("automation lane recovery: {error}"),
            Err(error) => eprintln!("automation lane recovery: {error}"),
        }
        let mut steps = JoinSet::new();
        loop {
            // Register BEFORE inspection so a wakeup between the two is not lost.
            let notified = self.runtime_control.wakeup.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if shutdown.is_cancelled() {
                break;
            }
            // M3: events of the Ikore pipeline become runs before admission.
            let service = self.clone();
            match tokio::task::spawn_blocking(move || service.dispatch_pipeline_triggers()).await {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => eprintln!("pipeline triggers: {error}"),
                Err(error) => eprintln!("pipeline triggers: {error}"),
            }
            // Feature 27 (Camada C): domain events become runs before admission.
            let service = self.clone();
            match tokio::task::spawn_blocking(move || service.dispatch_domain_triggers()).await {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => eprintln!("domain triggers: {error}"),
                Err(error) => eprintln!("domain triggers: {error}"),
            }
            // M4: admit pending stage verifiers before custom pipeline steps.
            loop {
                let service = self.clone();
                match tokio::task::spawn_blocking(move || service.claim_next_verifier_run()).await {
                    Ok(Ok(Some(claim))) => {
                        let service = self.clone();
                        let stop = shutdown.clone();
                        steps.spawn(async move { service.run_verifier_run(claim, stop).await });
                    }
                    Ok(Ok(None)) => break,
                    Ok(Err(error)) => {
                        eprintln!("stage verifiers lane: {error}");
                        break;
                    }
                    Err(error) => {
                        eprintln!("stage verifiers lane: {error}");
                        break;
                    }
                }
            }
            loop {
                let service = self.clone();
                match tokio::task::spawn_blocking(move || service.claim_next_automation_step())
                    .await
                {
                    Ok(Ok(Some(claim))) => {
                        let service = self.clone();
                        let stop = shutdown.clone();
                        steps.spawn(async move { service.run_automation_step(claim, stop).await });
                    }
                    Ok(Ok(None)) => break,
                    Ok(Err(error)) => {
                        eprintln!("automation lane: {error}");
                        break;
                    }
                    Err(error) => {
                        eprintln!("automation lane: {error}");
                        break;
                    }
                }
            }
            tokio::select! {
                _ = shutdown.cancelled() => break,
                Some(_) = steps.join_next(), if !steps.is_empty() => {}
                _ = &mut notified => {}
                _ = sleep(Duration::from_secs(1)) => {}
            }
        }
        // Each step sees the shutdown, stops its process and records itself.
        while steps.join_next().await.is_some() {}
    }

    /// A closed app left no process: its places are free again, a running
    /// step is `interrupted` and its run offers `Resume`.
    pub(super) fn recover_automation_lane(&self) -> Result<(), PersistenceServiceError> {
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = now();
        tx.execute("DELETE FROM automation_slots", [])?;
        tx.execute(
            "UPDATE custom_pipeline_step_runs
                SET status=CASE WHEN (SELECT r.status FROM custom_pipeline_runs r WHERE r.id=run_id)='running'
                                THEN 'interrupted' ELSE 'cancelled' END,
                    error=CASE WHEN (SELECT r.status FROM custom_pipeline_runs r WHERE r.id=run_id)='running'
                               THEN ?2 ELSE error END,
                    finished_at=?1
              WHERE status='running'",
            params![now, INTERRUPTED],
        )?;
        tx.execute(
            "UPDATE custom_pipeline_runs SET status='interrupted',error=?2,updated_at=?1
              WHERE status='running' AND EXISTS(
                SELECT 1 FROM custom_pipeline_step_runs s
                 WHERE s.run_id=custom_pipeline_runs.id
                   AND s.step_index=custom_pipeline_runs.current_step
                   AND s.status='interrupted'
                   AND s.attempt=(SELECT max(t.attempt) FROM custom_pipeline_step_runs t
                                   WHERE t.run_id=s.run_id AND t.step_index=s.step_index))",
            params![now, INTERRUPTED],
        )?;
        Self::recover_stage_verifiers_in_tx(&tx, &now)?;
        tx.commit()?;
        Ok(())
    }

    /// The oldest queued agent step whose project has no step running, when
    /// the app-wide ceiling leaves a place (feature 25, "Faixa de automação").
    pub(super) fn claim_next_automation_step(
        &self,
    ) -> Result<Option<AutomationClaim>, PersistenceServiceError> {
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let running: u32 = tx.query_row("SELECT count(*) FROM automation_slots", [], |row| {
            row.get(0)
        })?;
        if running >= max_concurrent(&tx)? {
            return Ok(None);
        }
        let candidate: Option<(String, u32, u32, String, String, String, String)> = tx
            .query_row(
                "SELECT s.run_id,s.step_index,s.attempt,r.project_id,r.pipeline_id,p.name,
                        r.resolved_definition_json
                   FROM custom_pipeline_step_runs s
                   JOIN custom_pipeline_runs r ON r.id=s.run_id
                   JOIN custom_pipelines p ON p.id=r.pipeline_id
                  WHERE s.status='queued' AND s.kind IN ('agent','host_tool','canonical')
                    AND r.status IN ('queued','running') AND s.step_index=r.current_step
                    AND NOT EXISTS(SELECT 1 FROM automation_slots a
                                    WHERE a.project_id=r.project_id AND a.class='custom_step')
                  ORDER BY r.created_at,r.id,s.attempt
                  LIMIT 1",
                [],
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
        let Some((run_id, step_index, attempt, project_id, pipeline_id, pipeline_name, json)) =
            candidate
        else {
            return Ok(None);
        };
        let now = now();
        let definition = parse_definition(&json)
            .map_err(|error| PersistenceServiceError::Runtime(error.to_string()))?;
        let Some(step) = definition.steps.get(step_index as usize).cloned() else {
            // The stored run disagrees with its own definition: end it.
            tx.execute(
                "UPDATE custom_pipeline_step_runs SET status='failed',error=?4,finished_at=?5
                  WHERE run_id=?1 AND step_index=?2 AND attempt=?3",
                params![
                    run_id,
                    step_index,
                    attempt,
                    "The step is missing from the stored definition.",
                    now
                ],
            )?;
            tx.execute(
                "UPDATE custom_pipeline_runs SET status='failed',error=?2,finished_at=?3,updated_at=?3
                  WHERE id=?1",
                params![run_id, "The run's definition has no runnable step here.", now],
            )?;
            tx.commit()?;
            return Ok(None);
        };
        if matches!(step, PipelineStep::Gate(_)) {
            tx.execute(
                "UPDATE custom_pipeline_step_runs SET status='failed',error=?4,finished_at=?5
                  WHERE run_id=?1 AND step_index=?2 AND attempt=?3",
                params![
                    run_id,
                    step_index,
                    attempt,
                    "A gate step cannot be queued for worker execution.",
                    now
                ],
            )?;
            tx.execute(
                "UPDATE custom_pipeline_runs SET status='failed',error=?2,finished_at=?3,updated_at=?3
                  WHERE id=?1",
                params![run_id, "The run's definition has a gate at this step.", now],
            )?;
            tx.commit()?;
            return Ok(None);
        }
        let token = Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO automation_slots(token,class,project_id,owner_id,acquired_at)
             VALUES(?1,'custom_step',?2,?3,?4)",
            params![
                token,
                project_id,
                format!("{run_id}:{step_index}:{attempt}"),
                now
            ],
        )?;
        tx.execute(
            "UPDATE custom_pipeline_step_runs SET status='running',started_at=?4
              WHERE run_id=?1 AND step_index=?2 AND attempt=?3",
            params![run_id, step_index, attempt, now],
        )?;
        tx.execute(
            "UPDATE custom_pipeline_runs SET status='running',updated_at=?2 WHERE id=?1",
            params![run_id, now],
        )?;
        tx.commit()?;
        Ok(Some(AutomationClaim {
            token,
            run_id,
            step_index,
            attempt,
            project_id,
            pipeline_id,
            pipeline_name,
            definition,
            step,
        }))
    }

    pub(super) async fn run_automation_step(
        &self,
        claim: AutomationClaim,
        shutdown: CancellationToken,
    ) {
        let key = automation_cancellation_key(&claim.run_id);
        let token = CancellationToken::new();
        self.register_job_cancellation(&key, token.clone());
        // A `Cancel` that landed between the claim and the registration.
        let service = self.clone();
        let run_id = claim.run_id.clone();
        if let Ok(Ok(Some(status))) =
            tokio::task::spawn_blocking(move || service.automation_run_status(&run_id)).await
            && status != "running"
        {
            token.cancel();
        }
        let result = {
            let work = self.execute_claimed_step(&claim, &token);
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
            service.finish_automation_step(&claim, result, stopping)
        })
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => eprintln!("automation step not recorded: {error}"),
            Err(error) => eprintln!("automation step not recorded: {error}"),
        }
        self.runtime_control.wakeup.notify_waiters();
    }

    fn automation_run_status(
        &self,
        run_id: &str,
    ) -> Result<Option<String>, PersistenceServiceError> {
        Ok(self
            .connection()?
            .query_row(
                "SELECT status FROM custom_pipeline_runs WHERE id=?1",
                [run_id],
                |row| row.get(0),
            )
            .optional()?)
    }

    async fn execute_claimed_step(
        &self,
        claim: &AutomationClaim,
        token: &CancellationToken,
    ) -> StepResult {
        match &claim.step {
            PipelineStep::Agent(agent) => self.execute_agent_step(claim, agent, token).await,
            PipelineStep::HostTool(tool_step) => {
                self.execute_host_tool_step(claim, tool_step, token).await
            }
            PipelineStep::Canonical(canonical) => {
                self.execute_canonical_step(claim, canonical, token).await
            }
            PipelineStep::Gate(_) => StepResult::failed("Gate steps wait for human approval."),
        }
    }

    async fn execute_agent_step(
        &self,
        claim: &AutomationClaim,
        agent: &AgentStep,
        token: &CancellationToken,
    ) -> StepResult {
        let service = self.clone();
        let owned = claim.clone();
        let agent_owned = agent.clone();
        let context = match tokio::task::spawn_blocking(move || {
            service.automation_step_context(&owned, &agent_owned)
        })
        .await
        {
            Ok(Ok(context)) => context,
            Ok(Err(error)) => return StepResult::failed(error.to_string()),
            Err(error) => return StepResult::failed(error.to_string()),
        };
        let mut result = StepResult {
            outcome: Err(StepFailure::Cancelled),
            terminal_run_id: None,
            llm_execution_id: None,
            git_operation_id: None,
        };
        let first = match self
            .call_step_agent(claim, agent, &context.system, &context.user, 1, token)
            .await
        {
            Ok(call) => call,
            Err(failure) => {
                result.outcome = Err(failure);
                return result;
            }
        };
        result.record(&first);
        result.outcome = match agent.output {
            StepOutput::Text => sanitized(&first.raw)
                .map(StepOutcome::Text)
                .map_err(StepFailure::Failed),
            StepOutput::Verdict => {
                let verdict = match parse_verdict(&first.raw) {
                    Ok(verdict) => Ok(verdict),
                    // One correction, with the answer and what was wrong.
                    Err(problem) => {
                        let user = prompt::correction_prompt(&context.user, &first.raw, &problem);
                        match self
                            .call_step_agent(claim, agent, &context.system, &user, 2, token)
                            .await
                        {
                            Ok(second) => {
                                result.record(&second);
                                parse_verdict(&second.raw).map_err(|problem| {
                                    StepFailure::Failed(format!(
                                        "The verdict was invalid twice: {problem}"
                                    ))
                                })
                            }
                            Err(failure) => Err(failure),
                        }
                    }
                };
                verdict.and_then(|verdict| {
                    checked_verdict(verdict, &context.workspace)
                        .map(StepOutcome::Verdict)
                        .map_err(StepFailure::Failed)
                })
            }
        };
        result
    }

    async fn execute_host_tool_step(
        &self,
        claim: &AutomationClaim,
        tool_step: &HostToolStep,
        token: &CancellationToken,
    ) -> StepResult {
        if token.is_cancelled() {
            return StepResult {
                outcome: Err(StepFailure::Cancelled),
                terminal_run_id: None,
                llm_execution_id: None,
                git_operation_id: None,
            };
        }
        let mut result = StepResult {
            outcome: Err(StepFailure::Failed("Host tool did not run.".into())),
            terminal_run_id: None,
            llm_execution_id: None,
            git_operation_id: None,
        };
        let events = self.git_event_bus();
        match tool_step.tool {
            HostTool::GitCommit => {
                let target_pulsation_id = match self.source_pulsation_for_run(&claim.run_id) {
                    Ok(Some(pid)) => pid,
                    Ok(None) => {
                        result.outcome = Err(StepFailure::Failed(
                            "git.commit requires a source pulsation from a Pulsation done trigger."
                                .into(),
                        ));
                        return result;
                    }
                    Err(error) => return StepResult::failed(error.to_string()),
                };

                // Check if the immediately preceding step is an Agent step proposing a CommitPlanV1.
                let prev_agent_and_text = self.preceding_agent_plan_proposal(claim);
                let mut proposed_plan = prev_agent_and_text
                    .as_ref()
                    .and_then(|(_, _, raw)| parse_optional_commit_plan(raw));

                // EnsureDone queue backfilled & fetch pending entries up through target_pulsation_id (P8).
                let queue = match self.list_pulsation_commit_queue(&claim.project_id) {
                    Ok(q) => q,
                    Err(error) => return StepResult::failed(error.to_string()),
                };
                let target_pos = queue
                    .iter()
                    .position(|entry| entry.pulsation_id == target_pulsation_id);
                let to_commit: Vec<String> = match target_pos {
                    Some(idx) => queue[..=idx]
                        .iter()
                        .filter(|entry| entry.state == PulsationCommitQueueState::Pending)
                        .map(|entry| entry.pulsation_id.clone())
                        .collect(),
                    None => vec![target_pulsation_id.clone()],
                };
                if to_commit.is_empty() {
                    result.outcome = Ok(StepOutcome::Text(format!(
                        "No pending commits remained for pulsation {target_pulsation_id}."
                    )));
                    return result;
                }

                let mut summaries = Vec::with_capacity(to_commit.len());
                for pid in to_commit {
                    let is_target = pid == target_pulsation_id;
                    let ext_plan = if is_target {
                        proposed_plan.as_ref()
                    } else {
                        None
                    };
                    let mut outcome = match execute_commit_for_pulsation(
                        self,
                        self.git_locks(),
                        events.as_ref(),
                        &claim.project_id,
                        &pid,
                        None,
                        ext_plan,
                    )
                    .await
                    {
                        Ok(out) => out,
                        Err(error) => return StepResult::failed(error.to_string()),
                    };

                    // If an agent step proposed a CommitPlanV1 and it failed host validation,
                    // ask that agent for one correction and retry once (08 §Uso por agentes).
                    if is_target
                        && outcome.status == GitOperationStatus::Blocked
                        && ext_plan.is_some()
                        && let Some((prev_idx, prev_agent, prev_raw)) = prev_agent_and_text.as_ref()
                    {
                        let problem = format_violations(&outcome.violations);
                        let corr_user = format!(
                            "Your CommitPlanV1 was rejected by host rules:\n{problem}\n\nPrevious JSON:\n{prev_raw}\n\nReturn only corrected valid CommitPlanV1 JSON."
                        );
                        let mut corr_claim = claim.clone();
                        corr_claim.step_index = *prev_idx;
                        let mut corr_agent = prev_agent.clone();
                        corr_agent.output = StepOutput::Text;
                        let corr_sys = prompt::system_prompt(&claim.pipeline_name, &corr_agent);
                        if let Ok(call) = self
                            .call_step_agent(
                                &corr_claim,
                                &corr_agent,
                                &corr_sys,
                                &corr_user,
                                2,
                                token,
                            )
                            .await
                        {
                            result.record(&call);
                            if let Some(fixed_plan) = parse_optional_commit_plan(&call.raw) {
                                proposed_plan = Some(fixed_plan);
                                match execute_commit_for_pulsation(
                                    self,
                                    self.git_locks(),
                                    events.as_ref(),
                                    &claim.project_id,
                                    &pid,
                                    None,
                                    proposed_plan.as_ref(),
                                )
                                .await
                                {
                                    Ok(retried) => outcome = retried,
                                    Err(error) => return StepResult::failed(error.to_string()),
                                }
                            }
                        }
                    }

                    result.git_operation_id = Some(outcome.operation_id.clone());
                    if outcome.status != GitOperationStatus::Succeeded {
                        result.outcome = Err(StepFailure::Failed(format!(
                            "Commit for pulsation {pid} {}: {}",
                            outcome.status.as_str(),
                            format_violations(&outcome.violations)
                        )));
                        return result;
                    }
                    if outcome.skipped_already_in_head {
                        summaries.push(format!("{pid}: skipped (already in HEAD)"));
                    } else if let Some(receipt) = outcome.receipt {
                        summaries.push(format!(
                            "{pid}: committed {} commit(s) to {}",
                            receipt.commits.len(),
                            receipt.branch
                        ));
                    } else {
                        summaries.push(format!("{pid}: committed"));
                    }
                }
                result.outcome = Ok(StepOutcome::Text(summaries.join("\n")));
                result
            }
            HostTool::GitPush => {
                match execute_push_for_project(
                    self,
                    self.git_locks(),
                    events.as_ref(),
                    &claim.project_id,
                    None,
                )
                .await
                {
                    Ok(outcome) => {
                        result.git_operation_id = Some(outcome.operation_id);
                        if outcome.status == GitOperationStatus::Succeeded {
                            let summary = outcome
                                .receipt
                                .map(|r| r.summary)
                                .unwrap_or_else(|| "Push succeeded.".to_owned());
                            result.outcome = Ok(StepOutcome::Text(summary));
                        } else {
                            result.outcome =
                                Err(StepFailure::Failed(format_violations(&outcome.violations)));
                        }
                        result
                    }
                    Err(error) => StepResult::failed(error.to_string()),
                }
            }
            HostTool::GitPull => {
                let owner_id = claim.slot_owner_id();
                match execute_pull_for_project(
                    self,
                    self.git_locks(),
                    events.as_ref(),
                    &claim.project_id,
                    Some(&owner_id),
                    None,
                )
                .await
                {
                    Ok(outcome) => {
                        result.git_operation_id = Some(outcome.operation_id);
                        if outcome.status == GitOperationStatus::Succeeded {
                            let summary = outcome
                                .receipt
                                .map(|r| r.summary)
                                .unwrap_or_else(|| "Pull succeeded.".to_owned());
                            result.outcome = Ok(StepOutcome::Text(summary));
                        } else {
                            result.outcome =
                                Err(StepFailure::Failed(format_violations(&outcome.violations)));
                        }
                        result
                    }
                    Err(error) => StepResult::failed(error.to_string()),
                }
            }
        }
    }

    fn source_pulsation_for_run(
        &self,
        run_id: &str,
    ) -> Result<Option<String>, PersistenceServiceError> {
        let db = self.connection()?;
        Ok(db
            .query_row(
                "SELECT source_pulsation_id FROM custom_pipeline_runs WHERE id=?1",
                [run_id],
                |row| row.get(0),
            )
            .optional()?
            .flatten())
    }

    fn preceding_agent_plan_proposal(
        &self,
        claim: &AutomationClaim,
    ) -> Option<(u32, AgentStep, String)> {
        let prev_idx = claim.step_index.checked_sub(1)?;
        let PipelineStep::Agent(agent) = claim.definition.steps.get(prev_idx as usize)?.clone()
        else {
            return None;
        };
        let db = self.connection().ok()?;
        let runs = load_step_runs(&db, &claim.run_id).ok()?;
        let raw = runs
            .into_iter()
            .filter(|run| run.step_index == prev_idx)
            .max_by_key(|run| run.attempt)
            .and_then(|run| run.output_text)?;
        parse_optional_commit_plan(&raw)?;
        Some((prev_idx, agent, raw))
    }

    /// The step's prompt: its instructions, the trigger, the last agent
    /// step's output before it and the notes left at the gates since then.
    fn automation_step_context(
        &self,
        claim: &AutomationClaim,
        agent: &AgentStep,
    ) -> Result<StepContext, PersistenceServiceError> {
        let db = self.connection()?;
        let workspace: String = db.query_row(
            "SELECT workspace_path FROM projects WHERE id=?1",
            [&claim.project_id],
            |row| row.get(0),
        )?;
        let runs = load_step_runs(&db, &claim.run_id)?;
        let latest = |index: u32| {
            runs.iter()
                .filter(|run| run.step_index == index)
                .max_by_key(|run| run.attempt)
        };
        let mut previous = None;
        let mut notes = Vec::new();
        for index in (0..claim.step_index).rev() {
            match claim.definition.steps.get(index as usize) {
                Some(PipelineStep::Gate(gate)) => {
                    if let Some(note) = latest(index).and_then(|run| run.gate_feedback.clone()) {
                        notes.push(GateNote {
                            label: gate.label.clone(),
                            note,
                        });
                    }
                }
                Some(PipelineStep::Agent(agent)) => {
                    previous =
                        latest(index).and_then(|run| match (&run.verdict, &run.output_text) {
                            (Some(verdict), _) => {
                                Some(PreviousOutput::verdict(&agent.label, verdict))
                            }
                            (None, Some(text)) => Some(PreviousOutput {
                                label: agent.label.clone(),
                                text: text.clone(),
                            }),
                            (None, None) => None,
                        });
                    break;
                }
                Some(PipelineStep::Canonical(canonical)) => {
                    previous = latest(index).and_then(|run| {
                        run.output_text.as_ref().map(|text| PreviousOutput {
                            label: canonical.label.clone(),
                            text: text.clone(),
                        })
                    });
                    break;
                }
                Some(PipelineStep::HostTool(_)) | None => {}
            }
        }
        notes.reverse();
        let (trigger_kind, payload): (String, String) = db.query_row(
            "SELECT trigger_kind,trigger_payload_json FROM custom_pipeline_runs WHERE id=?1",
            [&claim.run_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let trigger = match trigger_kind.as_str() {
            "default_event" => prompt::event_trigger(&payload),
            "domain_event" => prompt::domain_event_trigger(&payload),
            _ => prompt::manual_trigger(),
        };
        Ok(StepContext {
            system: prompt::system_prompt(&claim.pipeline_name, agent),
            user: prompt::user_prompt(agent, &trigger, previous.as_ref(), &notes),
            workspace: PathBuf::from(workspace),
        })
    }

    /// One call of the step's agent, by its CLI or by its API route.
    async fn call_step_agent(
        &self,
        claim: &AutomationClaim,
        agent_step: &AgentStep,
        system: &str,
        user: &str,
        call: u32,
        token: &CancellationToken,
    ) -> Result<StepCall, StepFailure> {
        let agent = &agent_step.agent;
        let slot_id = automation_slot_id(&claim.project_id, &claim.pipeline_id, &agent_step.id);
        let verdict = agent_step.output == StepOutput::Verdict;
        let deadline = Instant::now()
            + ikore_contracts::timeout::AgentTimeoutPolicy::automation().max_timeout();
        match agent.transport {
            StepTransport::Cli => {
                let terminals = self
                    .terminal_service
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone()
                    .ok_or_else(|| StepFailure::Failed("The CLI runtime is not ready.".into()))?;
                let backend = agent
                    .backend
                    .as_deref()
                    .and_then(CliBackend::parse)
                    .ok_or_else(|| StepFailure::Failed("The step names no known CLI.".into()))?;
                let completion = terminals
                    .execute_automation_step(
                        AutomationStepRequest {
                            project_id: claim.project_id.clone(),
                            slot_id,
                            label: automation_slot_label(&claim.pipeline_name, &agent_step.label),
                            origin: ExecutionOrigin::Automation,
                            pulsation_id: None,
                            backend,
                            model: agent.model.clone(),
                            model_profile: agent.model_profile.clone(),
                            thinking_level: ThinkingLevel::parse(&agent.thinking_level)
                                .unwrap_or(ThinkingLevel::Default),
                            system_prompt: system.to_owned(),
                            user_prompt: user.to_owned(),
                            schema: verdict.then(stage_verdict_json_schema),
                            deadline,
                        },
                        token,
                    )
                    .await
                    .map_err(|error| match error {
                        TerminalError::Cancelled => StepFailure::Cancelled,
                        other => StepFailure::Failed(other.to_string()),
                    })?;
                Ok(StepCall {
                    raw: completion.raw_output,
                    terminal_run_id: Some(completion.terminal_run_id),
                    llm_execution_id: None,
                })
            }
            StepTransport::Api => {
                let route_id = agent
                    .api_route_id
                    .clone()
                    .ok_or_else(|| StepFailure::Failed("The step names no API route.".into()))?;
                let service = self.clone();
                let config = tokio::task::spawn_blocking(move || service.load_runtime_config())
                    .await
                    .map_err(|error| StepFailure::Failed(error.to_string()))?
                    .map_err(|error| StepFailure::Failed(error.to_string()))?;
                let execution_id = Uuid::new_v4().to_string();
                let request = LlmRequest {
                    execution_id: ExecutionId::new(execution_id.clone()),
                    generation_id: GenerationId::new(format!(
                        "{}:{}:{}:{call}",
                        claim.run_id, claim.step_index, claim.attempt
                    )),
                    project_id: claim.project_id.clone(),
                    pulsation_id: None,
                    job_id: None,
                    agent_run_id: None,
                    job_chain: format!("pipeline:{}", claim.run_id),
                    target: AgentExecutionTarget::automation(slot_id, route_id),
                    stage: "Automation".into(),
                    system_prompt: system.to_owned(),
                    user_prompt: user.to_owned(),
                    response_format: if verdict {
                        ResponseFormat::JsonSchema {
                            name: "stage_verdict_v1".into(),
                            schema: stage_verdict_json_schema(),
                        }
                    } else {
                        ResponseFormat::Text
                    },
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
                            StepFailure::Cancelled
                        } else {
                            StepFailure::Failed(error.to_string())
                        }
                    })?;
                Ok(StepCall {
                    raw: response.content,
                    terminal_run_id: None,
                    llm_execution_id: Some(execution_id),
                })
            }
        }
    }

    /// Records the step, frees its place and moves the run on.
    fn finish_automation_step(
        &self,
        claim: &AutomationClaim,
        result: StepResult,
        stopping: bool,
    ) -> Result<(), PersistenceServiceError> {
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = now();
        tx.execute(
            "DELETE FROM automation_slots WHERE token=?1",
            [&claim.token],
        )?;
        let run_status: Option<String> = tx
            .query_row(
                "SELECT status FROM custom_pipeline_runs WHERE id=?1",
                [&claim.run_id],
                |row| row.get(0),
            )
            .optional()?;
        tx.execute(
            "UPDATE custom_pipeline_step_runs
                SET terminal_run_id=COALESCE(?4,terminal_run_id),
                    llm_execution_id=COALESCE(?5,llm_execution_id),
                    git_operation_id=COALESCE(?6,git_operation_id),
                    finished_at=?7
              WHERE run_id=?1 AND step_index=?2 AND attempt=?3",
            params![
                claim.run_id,
                claim.step_index,
                claim.attempt,
                result.terminal_run_id,
                result.llm_execution_id,
                result.git_operation_id,
                now
            ],
        )?;
        let set_step = |status: &str,
                        output: Option<&str>,
                        verdict: Option<&str>,
                        error: Option<&str>|
         -> Result<(), PersistenceServiceError> {
            tx.execute(
                "UPDATE custom_pipeline_step_runs SET status=?4,output_text=?5,verdict_json=?6,error=?7
                  WHERE run_id=?1 AND step_index=?2 AND attempt=?3",
                params![claim.run_id, claim.step_index, claim.attempt, status, output, verdict, error],
            )?;
            Ok(())
        };
        let end_run = |status: &str, error: Option<&str>| -> Result<(), PersistenceServiceError> {
            tx.execute(
                "UPDATE custom_pipeline_runs
                    SET status=?2,error=?3,updated_at=?4,
                        finished_at=CASE WHEN ?2='interrupted' THEN NULL ELSE ?4 END
                  WHERE id=?1",
                params![claim.run_id, status, error, now],
            )?;
            Ok(())
        };
        let label = claim.step.label().trim();
        if run_status.as_deref() != Some("running") {
            // Cancelled meanwhile: the run already says so.
            set_step("cancelled", None, None, None)?;
        } else {
            match result.outcome {
                Err(StepFailure::Cancelled) if stopping => {
                    set_step("interrupted", None, None, Some(INTERRUPTED))?;
                    end_run("interrupted", Some(INTERRUPTED))?;
                }
                Err(StepFailure::Cancelled) => {
                    set_step("cancelled", None, None, None)?;
                    end_run("cancelled", None)?;
                }
                Err(StepFailure::Failed(message)) => {
                    set_step("failed", None, None, Some(&message))?;
                    end_run("failed", Some(&format!("{label} failed: {message}")))?;
                }
                Ok(StepOutcome::Text(text)) => {
                    set_step("done", Some(&text), None, None)?;
                    open_next_step(
                        &tx,
                        &claim.run_id,
                        &claim.definition,
                        Some(claim.step_index),
                        &now,
                    )?;
                }
                Ok(StepOutcome::Verdict(verdict)) => {
                    let json = serde_json::to_string(&verdict)
                        .map_err(|error| PersistenceServiceError::Runtime(error.to_string()))?;
                    if verdict.verdict == Verdict::Pass {
                        set_step("passed", None, Some(&json), None)?;
                        open_next_step(
                            &tx,
                            &claim.run_id,
                            &claim.definition,
                            Some(claim.step_index),
                            &now,
                        )?;
                    } else {
                        set_step("rejected", None, Some(&json), None)?;
                        end_run(
                            "failed",
                            Some(&format!("Rejected by {label}: {}", verdict.summary.trim())),
                        )?;
                    }
                }
            }
        }
        tx.commit()?;
        Ok(())
    }
}

fn format_violations(violations: &[RuleViolation]) -> String {
    if violations.is_empty() {
        return "Blocked without specific violation details.".to_owned();
    }
    violations
        .iter()
        .map(|v| match &v.path {
            Some(path) => format!("[{}] ({}): {}", v.code, path, v.message),
            None => format!("[{}] {}", v.code, v.message),
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn parse_optional_commit_plan(raw: &str) -> Option<CommitPlanV1> {
    let trimmed = raw.trim();
    if let Ok(plan) = serde_json::from_str::<CommitPlanV1>(trimmed) {
        return Some(plan);
    }
    let stripped = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .map(str::trim)
        .and_then(|s| s.strip_suffix("```"))
        .map(str::trim)?;
    serde_json::from_str::<CommitPlanV1>(stripped).ok()
}

#[cfg(test)]
mod tests {
    use ikore_contracts::custom_pipeline::{FindingSeverity, VerdictFinding};

    use super::*;

    #[test]
    fn findings_cite_only_files_inside_the_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(workspace.path().join("src")).unwrap();
        std::fs::write(workspace.path().join("src/lib.rs"), "").unwrap();
        let finding = |file: &str| VerdictFinding {
            id: file.into(),
            severity: FindingSeverity::Warning,
            message: "m".into(),
            file: Some(file.into()),
            line: Some(3),
            suggestion: None,
            file_not_found: false,
        };
        let verdict = StageVerdictV1 {
            verdict: Verdict::Pass,
            summary: "ok".into(),
            findings: vec![
                finding("src\\lib.rs"),
                finding("../outside.rs"),
                finding("src/missing.rs"),
                finding("C:/Windows/win.ini"),
            ],
        };
        let checked = checked_verdict(verdict, workspace.path()).unwrap();
        assert_eq!(checked.findings[0].file.as_deref(), Some("src/lib.rs"));
        assert_eq!(checked.findings[0].line, Some(3));
        for finding in &checked.findings[1..] {
            assert!(finding.file.is_none() && finding.file_not_found && finding.line.is_none());
        }
    }
}
