//! Feature 27 (Camada B): canonical role step execution.
//!
//! Runs the five out-of-pulsation canonical roles (`FeatureAgent`,
//! `LearningAgent`, `Visualizer`, `NotesScribe`, `DocumentationReconciler`)
//! using their hardened role prompts, token budgets, timeout policies, and
//! project role routing while emitting a plain Markdown `StepOutcome::Text`.

use std::time::{Duration, Instant};

use rusqlite::{OptionalExtension, params};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::custom_pipeline_engine::{AutomationClaim, StepFailure, StepOutcome, StepResult};
use super::custom_pipeline_store::load_step_runs;
use super::{PersistenceService, PersistenceServiceError};
use crate::agent_timeout::{AgentTimeoutPolicy, CliTimeoutBudget};
use crate::custom_pipelines::prompt::{self, GateNote, PreviousOutput, STEP_TOOL_CONTRACT};
use crate::custom_pipelines::{CanonicalPipelineRole, CanonicalStep, PipelineStep};
use crate::exchange::render::PROVENANCE_LEGEND;
use crate::llm::{
    AgentExecutionTarget, DispatchPolicy, ExecutionId, GenerationId, LlmErrorKind, LlmGateway,
    LlmRequest, ResponseFormat,
};

fn role_routing_stage(role: CanonicalPipelineRole) -> &'static str {
    match role {
        CanonicalPipelineRole::FeatureAgent => "Forge",
        CanonicalPipelineRole::LearningAgent => "Learning",
        CanonicalPipelineRole::Visualizer => "Review",
        CanonicalPipelineRole::NotesScribe => "Notes",
        CanonicalPipelineRole::DocumentationReconciler => "Docs",
    }
}

fn role_system_prompt(pipeline_name: &str, step: &CanonicalStep) -> String {
    let label = prompt::quoted_name(&step.label);
    let pipeline = prompt::quoted_name(pipeline_name);
    let hardening = match step.role {
        CanonicalPipelineRole::NotesScribe => {
            "This is Notes Scribe in strict isolation: no tools, no shell, no repository access, and no workspace inspection. Base your Markdown reply strictly on the developer's note and prompt inputs provided here."
        }
        CanonicalPipelineRole::DocumentationReconciler => {
            "This is Documentation Reconciler in strict isolation: no tools, no shell, no repository browsing, and no direct file edits. Propose concise Markdown guidance strictly from the inputs provided here."
        }
        CanonicalPipelineRole::Visualizer => {
            "This is Visualizer in strict read-only no-tools isolation: return brief, non-normative Markdown review highlights from the checkpoint and trigger context provided here. Never claim files or behaviors not shown in the inputs."
        }
        CanonicalPipelineRole::LearningAgent => {
            "This is Learning agent in advisory read-only mode. Propose structured context observations and memory candidates in Markdown without modifying files or claims of automatic writes."
        }
        CanonicalPipelineRole::FeatureAgent => {
            "This is Feature agent in read-only planning mode. Return a crisp Markdown readiness summary, open risks, and scope verification without writing files or running commands."
        }
    };
    let tools = if step.role.is_no_tools_no_repo() {
        "TOOLS: All tools, filesystem access, and commands are disabled by the host."
    } else {
        STEP_TOOL_CONTRACT
    };
    format!(
        "You are {}, running canonical role {} inside pipeline {} in Ikore.\n{}\n{}\nOUTPUT: Return concise Markdown.\n{}",
        label,
        step.role.label(),
        pipeline,
        hardening,
        tools,
        PROVENANCE_LEGEND
    )
}

impl PersistenceService {
    fn reject_custom_cli_for_canonical(
        &self,
        project_id: &str,
        role: CanonicalPipelineRole,
    ) -> Result<(), StepFailure> {
        let db = self
            .connection()
            .map_err(|error| StepFailure::Failed(error.to_string()))?;
        let configured: Option<(String, String)> = db
            .query_row(
                "SELECT execution_mode, backend FROM cli_agent_configs
                  WHERE project_id=?1 AND agent_role=?2",
                params![project_id, role.to_agent_role().as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|error| StepFailure::Failed(error.to_string()))?;
        if let Some((mode, backend)) = configured
            && mode == "cli_process"
            && backend.trim().eq_ignore_ascii_case("custom")
        {
            return Err(StepFailure::Failed(format!(
                "Canonical role {} refuses the custom CLI backend in pipeline steps.",
                role.label()
            )));
        }
        Ok(())
    }

    pub(super) fn canonical_step_context(
        &self,
        claim: &AutomationClaim,
        step: &CanonicalStep,
    ) -> Result<(String, String), PersistenceServiceError> {
        let db = self.connection()?;
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
        let system = role_system_prompt(&claim.pipeline_name, step);
        let user = prompt::canonical_user_prompt(step, &trigger, previous.as_ref(), &notes);
        Ok((system, user))
    }

    pub(super) async fn execute_canonical_step(
        &self,
        claim: &AutomationClaim,
        step: &CanonicalStep,
        token: &CancellationToken,
    ) -> StepResult {
        if token.is_cancelled() {
            return StepResult::cancelled();
        }
        if let Err(failure) = self.reject_custom_cli_for_canonical(&claim.project_id, step.role) {
            return StepResult::with_failure(failure);
        }
        let service = self.clone();
        let owned_claim = claim.clone();
        let owned_step = step.clone();
        let (system_prompt, user_prompt) = match tokio::task::spawn_blocking(move || {
            service.canonical_step_context(&owned_claim, &owned_step)
        })
        .await
        {
            Ok(Ok(prompts)) => prompts,
            Ok(Err(error)) => return StepResult::failed(error.to_string()),
            Err(error) => return StepResult::failed(error.to_string()),
        };
        let service = self.clone();
        let config = match tokio::task::spawn_blocking(move || service.load_runtime_config()).await
        {
            Ok(Ok(config)) => config,
            Ok(Err(error)) => return StepResult::failed(error.to_string()),
            Err(error) => return StepResult::failed(error.to_string()),
        };
        let role = step.role.to_agent_role();
        let execution_id = Uuid::new_v4().to_string();
        let request = LlmRequest {
            execution_id: ExecutionId::new(execution_id.clone()),
            generation_id: GenerationId::new(format!(
                "{}:{}:{}:1",
                claim.run_id, claim.step_index, claim.attempt
            )),
            project_id: claim.project_id.clone(),
            pulsation_id: None,
            job_id: None,
            agent_run_id: None,
            job_chain: format!("pipeline:{}", claim.run_id),
            target: AgentExecutionTarget::canonical(role),
            stage: role_routing_stage(step.role).into(),
            system_prompt,
            user_prompt,
            response_format: ResponseFormat::Text,
            max_output_tokens: config.output_budget_for(role),
            temperature: config.temperature,
            deadline: Instant::now()
                + AgentTimeoutPolicy::for_role(role)
                    .base_timeout()
                    .max(Duration::from_millis(config.timeout_ms)),
            stream: true,
            legacy_provider: config.provider.as_str().into(),
            legacy_model: config.model,
            dispatch: DispatchPolicy {
                cli_timeout: Some(CliTimeoutBudget::for_role(role, config.timeout_ms)),
                ..Default::default()
            },
        };
        let terminals = self
            .terminal_service
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(terminals) = terminals {
            match terminals.execute_llm_request(&request, token).await {
                Ok(Some(completion)) => {
                    return match crate::llm::adapter::sanitize_model_text(&completion.raw_output) {
                        Ok(clean) => StepResult {
                            outcome: Ok(StepOutcome::Text(clean)),
                            terminal_run_id: Some(completion.terminal_run_id),
                            llm_execution_id: None,
                            git_operation_id: None,
                        },
                        Err(error) => StepResult {
                            outcome: Err(StepFailure::Failed(format!(
                                "The step's output could not be cleaned: {error}"
                            ))),
                            terminal_run_id: Some(completion.terminal_run_id),
                            llm_execution_id: None,
                            git_operation_id: None,
                        },
                    };
                }
                Ok(None) => {}
                Err(error) => {
                    return if token.is_cancelled() {
                        StepResult::cancelled()
                    } else {
                        StepResult::failed(error.to_string())
                    };
                }
            }
        }
        match self.llm_router.execute(&request, token).await {
            Ok(response) => match crate::llm::adapter::sanitize_model_text(&response.content) {
                Ok(clean) => StepResult {
                    outcome: Ok(StepOutcome::Text(clean)),
                    terminal_run_id: None,
                    llm_execution_id: Some(execution_id),
                    git_operation_id: None,
                },
                Err(error) => StepResult {
                    outcome: Err(StepFailure::Failed(format!(
                        "The step's output could not be cleaned: {error}"
                    ))),
                    terminal_run_id: None,
                    llm_execution_id: Some(execution_id),
                    git_operation_id: None,
                },
            },
            Err(error) => {
                if error.kind == LlmErrorKind::Cancelled || token.is_cancelled() {
                    StepResult::cancelled()
                } else {
                    StepResult::failed(error.to_string())
                }
            }
        }
    }
}
