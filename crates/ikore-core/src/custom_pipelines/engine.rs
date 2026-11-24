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
    AgentStep, CustomPipelineDefinitionV1, PipelineStep, StageVerdictV1, StepOutput, StepTransport,
    Verdict, automation_slot_id, automation_slot_label, parse_definition, parse_verdict,
    stage_verdict_json_schema,
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
    step_index: u32,
    attempt: u32,
    pub(super) project_id: String,
    pipeline_id: String,
    pipeline_name: String,
    definition: CustomPipelineDefinitionV1,
    step: AgentStep,
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

enum StepOutcome {
    Text(String),
    Verdict(StageVerdictV1),
}

enum StepFailure {
    Cancelled,
    Failed(String),
}

struct StepResult {
    outcome: Result<StepOutcome, StepFailure>,
    terminal_run_id: Option<String>,
    llm_execution_id: Option<String>,
}

impl StepResult {
    fn failed(message: impl Into<String>) -> Self {
        Self {
            outcome: Err(StepFailure::Failed(message.into())),
            terminal_run_id: None,
            llm_execution_id: None,
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
fn sanitized(text: &str) -> Result<String, StepFailure> {
    crate::llm::adapter::sanitize_model_text(text)
        .map_err(|error| StepFailure::Failed(error.to_string()))
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
fn checked_verdict(
    mut verdict: StageVerdictV1,
    workspace: &Path,
) -> Result<StageVerdictV1, StepFailure> {
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
                  WHERE s.status='queued' AND s.kind='agent'
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
        let Some(PipelineStep::Agent(step)) = definition.steps.get(step_index as usize).cloned()
        else {
            // The stored run disagrees with its own definition: end it.
            tx.execute(
                "UPDATE custom_pipeline_step_runs SET status='failed',error=?4,finished_at=?5
                  WHERE run_id=?1 AND step_index=?2 AND attempt=?3",
                params![
                    run_id,
                    step_index,
                    attempt,
                    "The step is not an agent step.",
                    now
                ],
            )?;
            tx.execute(
                "UPDATE custom_pipeline_runs SET status='failed',error=?2,finished_at=?3,updated_at=?3
                  WHERE id=?1",
                params![run_id, "The run's definition has no agent at this step.", now],
            )?;
            tx.commit()?;
            return Ok(None);
        };
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

    async fn run_automation_step(&self, claim: AutomationClaim, shutdown: CancellationToken) {
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
        let service = self.clone();
        let owned = claim.clone();
        let context = match tokio::task::spawn_blocking(move || {
            service.automation_step_context(&owned)
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
        };
        let first = match self
            .call_step_agent(claim, &context.system, &context.user, 1, token)
            .await
        {
            Ok(call) => call,
            Err(failure) => {
                result.outcome = Err(failure);
                return result;
            }
        };
        result.record(&first);
        result.outcome = match claim.step.output {
            StepOutput::Text => sanitized(&first.raw).map(StepOutcome::Text),
            StepOutput::Verdict => {
                let verdict = match parse_verdict(&first.raw) {
                    Ok(verdict) => Ok(verdict),
                    // One correction, with the answer and what was wrong.
                    Err(problem) => {
                        let user = prompt::correction_prompt(&context.user, &first.raw, &problem);
                        match self
                            .call_step_agent(claim, &context.system, &user, 2, token)
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
                verdict
                    .and_then(|verdict| checked_verdict(verdict, &context.workspace))
                    .map(StepOutcome::Verdict)
            }
        };
        result
    }

    /// The step's prompt: its instructions, the trigger, the last agent
    /// step's output before it and the notes left at the gates since then.
    fn automation_step_context(
        &self,
        claim: &AutomationClaim,
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
                Some(PipelineStep::HostTool(_)) | None => {}
            }
        }
        notes.reverse();
        let (trigger_kind, payload): (String, String) = db.query_row(
            "SELECT trigger_kind,trigger_payload_json FROM custom_pipeline_runs WHERE id=?1",
            [&claim.run_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let trigger = if trigger_kind == "default_event" {
            prompt::event_trigger(&payload)
        } else {
            prompt::manual_trigger()
        };
        Ok(StepContext {
            system: prompt::system_prompt(&claim.pipeline_name, &claim.step),
            user: prompt::user_prompt(&claim.step, &trigger, previous.as_ref(), &notes),
            workspace: PathBuf::from(workspace),
        })
    }

    /// One call of the step's agent, by its CLI or by its API route.
    async fn call_step_agent(
        &self,
        claim: &AutomationClaim,
        system: &str,
        user: &str,
        call: u32,
        token: &CancellationToken,
    ) -> Result<StepCall, StepFailure> {
        let agent = &claim.step.agent;
        let slot_id = automation_slot_id(&claim.project_id, &claim.pipeline_id, &claim.step.id);
        let verdict = claim.step.output == StepOutput::Verdict;
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
                            label: automation_slot_label(&claim.pipeline_name, &claim.step.label),
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
                    llm_execution_id=COALESCE(?5,llm_execution_id),finished_at=?6
              WHERE run_id=?1 AND step_index=?2 AND attempt=?3",
            params![
                claim.run_id,
                claim.step_index,
                claim.attempt,
                result.terminal_run_id,
                result.llm_execution_id,
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
        let label = claim.step.label.trim();
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
        let checked = checked_verdict(verdict, workspace.path()).ok().unwrap();
        assert_eq!(checked.findings[0].file.as_deref(), Some("src/lib.rs"));
        assert_eq!(checked.findings[0].line, Some(3));
        for finding in &checked.findings[1..] {
            assert!(finding.file.is_none() && finding.file_not_found && finding.line.is_none());
        }
    }
}
