//! Compatibility facade and composition root for native terminal services.

use crate::{
    llm::LlmRequest, secret_redactor::PreparedModelInput, terminals_host::CoreTerminalHost,
};
use ikore_terminal::host::{self, TerminalHost};
use serde_json::Value;
use std::{ops::Deref, path::PathBuf, sync::Arc, time::Instant};
use tokio::runtime::Handle;
use tokio_util::sync::CancellationToken;

pub use ikore_terminal::terminals::*;

fn host() -> Arc<dyn TerminalHost> {
    Arc::new(CoreTerminalHost)
}

#[derive(Debug, Clone)]
pub struct TerminalRepository(ikore_terminal::TerminalRepository);

impl TerminalRepository {
    pub fn new(db_path: impl Into<PathBuf>) -> Self {
        Self(ikore_terminal::TerminalRepository::new(db_path, host()))
    }
}
impl Deref for TerminalRepository {
    type Target = ikore_terminal::TerminalRepository;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Debug, Clone)]
pub struct CliModelCatalogService(ikore_terminal::CliModelCatalogService);
impl CliModelCatalogService {
    pub fn new(db_path: impl Into<PathBuf>) -> Self {
        Self(ikore_terminal::CliModelCatalogService::new(db_path, host()))
    }
}
impl Deref for CliModelCatalogService {
    type Target = ikore_terminal::CliModelCatalogService;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

pub struct TerminalService {
    inner: Arc<ikore_terminal::TerminalService>,
    repository: TerminalRepository,
}
impl TerminalService {
    pub fn new(
        db_path: impl Into<PathBuf>,
        runtime: Handle,
        event_sink: Arc<dyn TerminalEventSink>,
    ) -> Result<Self, TerminalError> {
        let inner = Arc::new(ikore_terminal::TerminalService::new(
            db_path,
            runtime,
            event_sink,
            host(),
        )?);
        let repository = TerminalRepository(inner.repository().clone());
        Ok(Self { inner, repository })
    }
    pub fn repository(&self) -> &TerminalRepository {
        &self.repository
    }

    pub async fn execute_llm_request(
        self: &Arc<Self>,
        request: &LlmRequest,
        cancellation: &CancellationToken,
    ) -> Result<Option<CliCompletion>, TerminalError> {
        let request = host::LlmRequest {
            execution_id: host::ExecutionId(request.execution_id.0.clone()),
            project_id: request.project_id.clone(),
            pulsation_id: request.pulsation_id.clone(),
            job_id: request.job_id.clone(),
            agent_run_id: request.agent_run_id.clone(),
            job_chain: request.job_chain.clone(),
            target: request.target.clone(),
            stage: request.stage.clone(),
            system_prompt: request.system_prompt.clone(),
            user_prompt: request.user_prompt.clone(),
            response_format: request.response_format.clone(),
            max_output_tokens: request.max_output_tokens,
            deadline: request.deadline,
            dispatch: host::TerminalDispatchPolicy {
                prepared_input: request
                    .dispatch
                    .prepared_input
                    .clone()
                    .map(|proof| Arc::new(proof) as host::PreparedModelInput),
                cli_timeout: request.dispatch.cli_timeout,
                unit: request.dispatch.unit.clone(),
                strict_schema: request.dispatch.strict_schema.clone(),
            },
        };
        self.inner.execute_llm_request(&request, cancellation).await
    }

    pub async fn execute_companion_round(
        &self,
        request: CompanionCliRoundRequest,
        cancellation: &CancellationToken,
        deadline: Instant,
    ) -> Result<CompanionCliRoundOutput, TerminalError> {
        self.inner
            .execute_companion_round(
                ikore_terminal::CompanionCliRoundRequest {
                    usage_context: request.usage_context,
                    project_id: request.project_id,
                    backend: request.backend,
                    model: request.model,
                    model_profile: request.model_profile,
                    model_source: request.model_source,
                    prompt: request.prompt,
                    prepared_input: request
                        .prepared_input
                        .map(|proof| Arc::new(proof) as host::PreparedModelInput),
                    schema: request.schema,
                },
                cancellation,
                deadline,
            )
            .await
    }
}
impl Deref for TerminalService {
    type Target = Arc<ikore_terminal::TerminalService>;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

pub fn build_process_spec(request: CliInvocationRequest) -> Result<CliProcessSpec, TerminalError> {
    ikore_terminal::build_process_spec(host().as_ref(), request)
}
pub async fn discover_clis_with_probes() -> Vec<CliAvailability> {
    ikore_terminal::discover_clis_with_probes(host()).await
}

#[derive(Debug, Clone)]
pub struct CompanionCliRoundRequest {
    pub usage_context: crate::usage::UsageContext,
    pub project_id: String,
    pub backend: CliBackend,
    pub model: Option<String>,
    pub model_profile: Option<String>,
    pub model_source: CliModelSelectionSource,
    pub prompt: String,
    pub prepared_input: Option<PreparedModelInput>,
    pub schema: Value,
}
