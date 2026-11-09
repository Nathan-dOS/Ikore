use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::agent_runtime::{
    AdaptiveContextMode, AgentRuntimeConfig, AgentRuntimeError, ContextRetrievalTrace,
    ContextStrategy, DynamicContextPolicy, EmbeddingProvider, LlmProvider, ProviderKeyStatus,
    StageExecutionInput, StageExecutionResult, execute_stage_with_cli,
};
use crate::embedding_service::{
    EmbeddingGateway, ModelPullEvent, ModelPullEventSink, OllamaEmbeddingGateway,
};
use crate::llm::{
    AgentExecutionTarget, AgentRole, ApiModelCatalogService, ApiModelCatalogSnapshot, Clock,
    CompressionPreview, CompressionPreviewRequest, CredentialStore,
    DeleteProviderConnectionRequest, DeleteProviderRouteRequest, DispatchPolicy, ExecutionId,
    FreeFormProviderRouteBinding, GenerationId, JitterSource, KeyringCredentialStore, LlmError,
    LlmErrorKind, LlmEventSink, LlmExecutionListItem, LlmExecutionSnapshot, LlmGateway, LlmRequest,
    LlmResilienceSnapshot, LlmRoutingMetric, NoopEventSink, ProviderConnection, ProviderProfile,
    ProviderRoute, ProviderRouteBinding, ProviderRouter, ResetLlmResilienceStateRequest,
    ResponseFormat, RoutingMode, SqliteLlmRepository, StableJitter, SystemClock,
    TestProviderConnectionRequest, TestProviderConnectionResponse,
    UpsertFreeFormProviderRouteBindingRequest, UpsertProviderConnectionRequest,
    UpsertProviderRouteBindingRequest, UpsertProviderRouteRequest, preview_compression,
    provider_profiles,
};
use chrono::Utc;
use reqwest::Client;
use rusqlite::{Connection, Error as SqlError, ErrorCode, OptionalExtension, params, types::Type};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub use crate::orchestration::decisions::{
    PendingDecision, PendingDecisionAction, PendingDecisionKind,
};

use crate::orchestration::brownfield::{
    BrownfieldConsentState, BrownfieldCoverageMetrics, BrownfieldOnboardingStatus,
};
use crate::orchestration::{IndexingEvent, IndexingEventSink};
use crate::terminals::{
    CliBackend, CliObservationReport, CliObservationTrial, TerminalRepository, TerminalService,
};

fn sanitized_observation_text(text: &str) -> String {
    crate::llm::adapter::sanitize_model_text(text)
        .unwrap_or_else(|_| crate::secret_redactor::OMITTED_CONTENT.into())
}

fn sanitized_observation_json(value: &Value) -> Value {
    let mut value = value.clone();
    if crate::llm::adapter::sanitize_json_strings(&mut value).is_err() {
        return Value::String(crate::secret_redactor::OMITTED_CONTENT.into());
    }
    value
}

#[path = "orchestration/anticipation.rs"]
mod anticipation;
#[path = "orchestration/attestation_persistence.rs"]
mod attestation_persistence;
pub use crate::attestation::{
    AttestationExport, AttestationFileCheck, AttestationKeyRecord, AttestationKeyState,
    AttestationKeyStatus, AttestationState, AttestationStatus, AttestationVerification, FileKey,
    RederivationCheck, SignatureCheck, SubjectCheck, WorkspaceCheck,
};
pub use attestation_persistence::ATTESTATION_SEALING_SETTING;
#[path = "orchestration/brownfield_context.rs"]
mod brownfield_context;
#[path = "orchestration/brownfield_enrichment.rs"]
mod brownfield_enrichment;
#[path = "orchestration/brownfield_maintenance.rs"]
mod brownfield_maintenance;
#[path = "orchestration/brownfield_mapping.rs"]
mod brownfield_mapping;
#[path = "orchestration/brownfield_resolution.rs"]
mod brownfield_resolution;
#[path = "orchestration/clarification.rs"]
mod clarification;
#[path = "orchestration/cleanup.rs"]
mod cleanup;
#[path = "orchestration/component_notes.rs"]
mod component_notes;
#[path = "orchestration/composition/host_cargo_validation.rs"]
mod composition_cargo_validation;
#[path = "orchestration/composition/host_coordinator.rs"]
mod composition_coordinator;
#[path = "orchestration/composition/host_persistence.rs"]
mod composition_persistence;
#[path = "orchestration/convergence.rs"]
mod convergence;
#[path = "orchestration/composition/risk_persistence.rs"]
mod risk_persistence;
pub use risk_persistence::{
    CheckpointRiskStatus, MAX_ACKNOWLEDGEMENT_CHARS, RiskAcknowledgement, RiskAcknowledgementRecord,
};
#[path = "orchestration/coordination.rs"]
mod coordination;
#[path = "feature_forge/persistence.rs"]
mod forge_batches;
pub(crate) use forge_batches::StructuredRequest;
#[path = "orchestration/indexing.rs"]
mod indexing;
#[path = "orchestration/integration.rs"]
mod integration;
#[path = "orchestration/layers_persistence.rs"]
mod layers_persistence;
#[path = "orchestration/learning.rs"]
mod learning;
#[path = "orchestration/mandate_persistence.rs"]
mod mandate_persistence;
pub use mandate_persistence::{MandateEnforcementEvent, MandatePreview, MandateRevisionRecord};
#[path = "orchestration/preparation.rs"]
mod preparation;
#[path = "orchestration/pulsation_report.rs"]
mod pulsation_report;
#[path = "orchestration/reconciliation.rs"]
mod reconciliation;
#[path = "orchestration/resolution.rs"]
mod resolution;
#[path = "orchestration/scope_persistence.rs"]
mod scope_persistence;
pub use scope_persistence::ScopeRequestView;
#[path = "orchestration/serial.rs"]
mod serial;
#[path = "orchestration/supervisor.rs"]
mod supervisor;
#[path = "orchestration/visualizer.rs"]
mod visualizer;
// ADR-037 baseline: ignored measurements, rerun at each milestone.
#[cfg(test)]
#[path = "exchange/baseline_tests.rs"]
mod exchange_baseline_tests;
#[cfg(test)]
#[path = "exchange/writers_tests.rs"]
mod exchange_writers_tests;

mod embedded {
    use refinery::embed_migrations;
    embed_migrations!("migrations");
}

impl PersistenceService {
    /// Starts the explicitly developer-enabled, twenty-run CLI observation
    /// experiment for one project. `TerminalService` consumes its slots only for
    /// automatic Pulsation CLI work.
    pub fn start_cli_observation_trial(
        &self,
        project_id: &str,
    ) -> Result<CliObservationTrial, PersistenceServiceError> {
        TerminalRepository::new(&self.db_path)
            .start_cli_observation_trial(project_id)
            .map_err(|error| PersistenceServiceError::Runtime(error.to_string()))
    }

    pub fn active_cli_observation_trial(
        &self,
        project_id: &str,
    ) -> Result<Option<CliObservationTrial>, PersistenceServiceError> {
        TerminalRepository::new(&self.db_path)
            .active_cli_observation_trial(project_id)
            .map_err(|error| PersistenceServiceError::Runtime(error.to_string()))
    }

    pub fn latest_cli_observation_report(
        &self,
        project_id: &str,
    ) -> Result<Option<CliObservationReport>, PersistenceServiceError> {
        TerminalRepository::new(&self.db_path)
            .latest_cli_observation_report(project_id)
            .map_err(|error| PersistenceServiceError::Runtime(error.to_string()))
    }
}

const SETTING_RUNTIME_PROVIDER: &str = "runtime.provider";
const SETTING_RUNTIME_MODEL: &str = "runtime.model";
const SETTING_RUNTIME_TEMPERATURE: &str = "runtime.temperature";
const SETTING_RUNTIME_MAX_OUTPUT_TOKENS: &str = "runtime.max_output_tokens";
const SETTING_RUNTIME_TIMEOUT_MS: &str = "runtime.timeout_ms";
const SETTING_RUNTIME_MAX_RETRIES: &str = "runtime.max_retries";
const SETTING_RUNTIME_CONTEXT_LIMIT: &str = "runtime.context_chars_limit";
const SETTING_RUNTIME_MAX_CONTEXT_FILES: &str = "runtime.max_context_files";
const SETTING_RUNTIME_CONTEXT_STRATEGY: &str = "runtime.context_strategy";
const SETTING_RUNTIME_EMBEDDING_ENABLED: &str = "runtime.embedding_enabled";
const SETTING_RUNTIME_EMBEDDING_PROVIDER: &str = "runtime.embedding_provider";
const SETTING_RUNTIME_EMBEDDING_MODEL: &str = "runtime.embedding_model";
const SETTING_RUNTIME_EMBEDDING_TOP_K: &str = "runtime.embedding_top_k";
const SETTING_RUNTIME_ADAPTIVE_CONTEXT_MODE: &str = "runtime.adaptive_context_mode";
const SETTING_RUNTIME_DYNAMIC_CONTEXT_POLICY: &str = "runtime.dynamic_context_policy_v1";
const SETTING_RUNTIME_ROUTING_V2_MODE: &str = "runtime.routing_v2_mode";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersistenceErrorCode {
    ValidationError,
    NotFound,
    DbError,
    MigrationError,
    IoError,
    RuntimeError,
    MissingApiKey,
    ProviderTimeout,
    ProviderBlocked,
    AllTargetsExhausted,
    SchemaViolation,
    RouteConflict,
    StreamFailed,
    CliExecutionFailed,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistenceCommandError {
    pub code: PersistenceErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

#[derive(Debug)]
pub enum PersistenceServiceError {
    Validation(String),
    NotFound(String),
    Db(SqlError),
    Migration(refinery::Error),
    Io(io::Error),
    Runtime(String),
    Retryable(String),
    /// The model's own structured output failed validation - distinct from
    /// `Validation`/`Runtime`, which also cover unrelated host-side checks
    /// that a fresh model attempt could never fix. Only this variant (plus
    /// `Llm` with `LlmErrorKind::OutputSchema`) is retried by `should_retry`.
    ModelOutputInvalid(String),
    Cli {
        code: String,
        message: String,
    },
    Cancelled(String),
    Llm(LlmError),
}

impl std::fmt::Display for PersistenceServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Validation(message)
            | Self::NotFound(message)
            | Self::Runtime(message)
            | Self::Retryable(message)
            | Self::ModelOutputInvalid(message)
            | Self::Cancelled(message) => write!(f, "{message}"),
            Self::Cli { message, .. } => write!(f, "{message}"),
            Self::Llm(error) => write!(f, "{error}"),
            Self::Db(error) => write!(f, "Local database failure: {error}"),
            Self::Migration(error) => write!(f, "Migration failure: {error}"),
            Self::Io(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for PersistenceServiceError {}

impl From<PersistenceServiceError> for PersistenceCommandError {
    fn from(value: PersistenceServiceError) -> Self {
        match value {
            PersistenceServiceError::Validation(message) => Self {
                code: PersistenceErrorCode::ValidationError,
                message,
                details: None,
                remediation: None,
            },
            PersistenceServiceError::NotFound(message) => Self {
                code: PersistenceErrorCode::NotFound,
                message,
                details: None,
                remediation: None,
            },
            PersistenceServiceError::Db(error) => Self {
                code: PersistenceErrorCode::DbError,
                message: format!("Local database failure: {error}"),
                details: None,
                remediation: None,
            },
            PersistenceServiceError::Migration(error) => Self {
                code: PersistenceErrorCode::MigrationError,
                message: format!("Failed to apply SQLite migrations: {error}"),
                details: None,
                remediation: None,
            },
            PersistenceServiceError::Io(error) => Self {
                code: PersistenceErrorCode::IoError,
                message: error.to_string(),
                details: None,
                remediation: None,
            },
            PersistenceServiceError::Runtime(message)
            | PersistenceServiceError::Retryable(message)
            | PersistenceServiceError::Cancelled(message) => Self {
                code: PersistenceErrorCode::RuntimeError,
                message,
                details: None,
                remediation: None,
            },
            PersistenceServiceError::ModelOutputInvalid(message) => Self {
                code: PersistenceErrorCode::SchemaViolation,
                message,
                details: None,
                remediation: None,
            },
            PersistenceServiceError::Cli { code, message } => Self {
                code: PersistenceErrorCode::CliExecutionFailed,
                message,
                details: Some(serde_json::json!({ "cliErrorCode": code })),
                remediation: Some(
                    "Review the diagnostic and choose between using the API, retrying the CLI or cancelling the pulsation."
                        .to_string(),
                ),
            },
            PersistenceServiceError::Llm(error) => {
                let code = match error.kind {
                    LlmErrorKind::MissingApiKey => PersistenceErrorCode::MissingApiKey,
                    LlmErrorKind::Timeout => PersistenceErrorCode::ProviderTimeout,
                    LlmErrorKind::Authentication
                    | LlmErrorKind::Billing
                    | LlmErrorKind::Permission => PersistenceErrorCode::ProviderBlocked,
                    LlmErrorKind::AllTargetsExhausted => PersistenceErrorCode::AllTargetsExhausted,
                    LlmErrorKind::OutputSchema => PersistenceErrorCode::SchemaViolation,
                    LlmErrorKind::RouteConflict => PersistenceErrorCode::RouteConflict,
                    LlmErrorKind::StreamFailed => PersistenceErrorCode::StreamFailed,
                    LlmErrorKind::Persistence => PersistenceErrorCode::DbError,
                    _ => PersistenceErrorCode::RuntimeError,
                };
                let remediation = match error.kind {
                    LlmErrorKind::MissingApiKey => Some("Add an API key to the connection and test it again.".to_string()),
                    LlmErrorKind::Authentication | LlmErrorKind::Billing => Some("Update the credential or fix the provider account.".to_string()),
                    LlmErrorKind::RouteConflict => Some("Reload the configuration and reapply the change on the current version.".to_string()),
                    LlmErrorKind::StreamFailed => Some("Review the partial text and use an explicit retry to start a new generation.".to_string()),
                    _ => None,
                };
                Self {
                    code,
                    message: error.message,
                    details: Some(serde_json::json!({
                        // The precise failure class, for surfaces that explain it (connection test).
                        "kind": format!("{:?}", error.kind),
                        "providerId": error.provider_id,
                        "connectionId": error.connection_id,
                        "modelId": error.model_id,
                        "httpStatus": error.http_status,
                        "nextEligibleAt": error.next_eligible_at,
                        "causes": error.causes,
                        "visiblePartial": error.visible_partial,
                    })),
                    remediation,
                }
            }
        }
    }
}

impl From<SqlError> for PersistenceServiceError {
    fn from(value: SqlError) -> Self {
        Self::Db(value)
    }
}

impl From<io::Error> for PersistenceServiceError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<AgentRuntimeError> for PersistenceServiceError {
    fn from(value: AgentRuntimeError) -> Self {
        match value {
            AgentRuntimeError::Validation(message) => Self::Validation(message),
            AgentRuntimeError::Io(error) => Self::Io(error),
            AgentRuntimeError::Json(error) => {
                Self::Runtime(format!("Failed to process the agent JSON: {error}"))
            }
            AgentRuntimeError::Provider(message) => Self::Runtime(message),
            AgentRuntimeError::Cli { code, message } => Self::Cli { code, message },
            AgentRuntimeError::Schema(message) => Self::Runtime(message),
            AgentRuntimeError::ModelOutputInvalid(message) => Self::ModelOutputInvalid(message),
            AgentRuntimeError::Cancelled => {
                Self::Cancelled("Run cancelled during the agent call.".to_string())
            }
            AgentRuntimeError::Llm(error) => Self::Llm(error),
        }
    }
}

impl From<LlmError> for PersistenceServiceError {
    fn from(value: LlmError) -> Self {
        Self::Llm(value)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub name: String,
    pub workspace_path: String,
    pub is_archived: bool,
    pub created_at: String,
    pub updated_at: String,
    pub last_opened_at: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateProjectRequest {
    pub workspace_path: String,
    pub name: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateProjectResponse {
    pub project: Project,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListProjectsRequest {
    #[serde(default)]
    pub include_archived: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListProjectsResponse {
    pub projects: Vec<Project>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenProjectRequest {
    pub project_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenProjectResponse {
    pub project: Project,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveProjectRequest {
    pub project_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveProjectResponse {
    pub project: Project,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnsureProjectForWorkspaceRequest {
    pub workspace_path: String,
    pub preferred_name: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnsureProjectForWorkspaceResponse {
    pub project: Project,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetLastActiveProjectResponse {
    pub project: Option<Project>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UiTabSnapshot {
    pub relative_path: String,
    pub draft_content: Option<String>,
    pub base_revision: Option<String>,
    pub cursor_line: Option<usize>,
    pub cursor_column: Option<usize>,
    pub scroll_top: Option<f64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UiSessionState {
    pub project_id: String,
    pub workspace_root: String,
    pub active_tab_path: Option<String>,
    pub workspace_view_mode: String,
    pub active_view: String,
    /// Since the shell redesign this column persists the right-hand context rail's fold; the
    /// left sidebar it was named after no longer exists. The name is kept to avoid a migration.
    pub sidebar_collapsed: bool,
    pub context_drawer_open: bool,
    pub active_pulsation_id: Option<String>,
    pub selected_vault_path: Option<String>,
    pub active_chat_conversation_id: Option<String>,
    pub scroll_top: Option<f64>,
    pub tabs: Vec<UiTabSnapshot>,
    #[serde(default)]
    pub auto_open_review: bool,
    pub updated_at: String,
}

fn default_active_view() -> String {
    "workspace".to_string()
}

fn default_context_drawer_open() -> bool {
    true
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadUiSessionRequest {
    pub project_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadUiSessionResponse {
    pub session: Option<UiSessionState>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveUiSessionRequest {
    pub project_id: String,
    pub workspace_root: String,
    pub active_tab_path: Option<String>,
    pub workspace_view_mode: String,
    #[serde(default = "default_active_view")]
    pub active_view: String,
    #[serde(default)]
    pub sidebar_collapsed: bool,
    #[serde(default = "default_context_drawer_open")]
    pub context_drawer_open: bool,
    #[serde(default)]
    pub active_pulsation_id: Option<String>,
    #[serde(default)]
    pub selected_vault_path: Option<String>,
    #[serde(default)]
    pub active_chat_conversation_id: Option<String>,
    pub scroll_top: Option<f64>,
    #[serde(default)]
    pub tabs: Vec<UiTabSnapshot>,
    #[serde(default)]
    pub auto_open_review: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveUiSessionResponse {
    pub updated_at: String,
}

pub use hvc_contracts::stages::PulsationStage;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PulsationStatus {
    Idle,
    Planning,
    Running,
    Checkpoint,
    Done,
    Failed,
}

impl PulsationStatus {
    fn as_db_value(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Planning => "planning",
            Self::Running => "running",
            Self::Checkpoint => "checkpoint",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }

    fn from_db_value(raw: &str) -> Option<Self> {
        match raw {
            "idle" => Some(Self::Idle),
            "planning" => Some(Self::Planning),
            "running" => Some(Self::Running),
            "checkpoint" => Some(Self::Checkpoint),
            "done" => Some(Self::Done),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HitlDecision {
    Approve,
    RequestRevision,
    Reject,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CliFallbackDecision {
    UseApi,
    RetryCli,
    Cancel,
}

impl CliFallbackDecision {
    const fn as_db_value(self) -> &'static str {
        match self {
            Self::UseApi => "use_api",
            Self::RetryCli => "retry_cli",
            Self::Cancel => "cancel",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageFailureDecision {
    /// Resume the same interrupted job. `attempts` is left alone, so the automatic
    /// budget stays spent and one click buys exactly one agent call.
    RetryStage,
    /// Re-enter at `failure_revision_target(stage)` with the operator's own prose.
    RequestRevision,
    /// What used to happen automatically: the pulsation becomes terminal.
    Abort,
}

impl StageFailureDecision {
    const fn as_db_value(self) -> &'static str {
        match self {
            Self::RetryStage => "retry_stage",
            Self::RequestRevision => "request_revision",
            Self::Abort => "abort",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointStatus {
    Waiting,
    Approved,
    RevisionRequested,
    Rejected,
}

impl CheckpointStatus {
    fn as_db_value(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Approved => "approved",
            Self::RevisionRequested => "revision_requested",
            Self::Rejected => "rejected",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Pending,
    Running,
    Done,
    Failed,
    Interrupted,
}

impl JobStatus {
    fn as_db_value(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
        }
    }

    fn from_db_value(raw: &str) -> Option<Self> {
        match raw {
            "pending" => Some(Self::Pending),
            "running" => Some(Self::Running),
            "done" => Some(Self::Done),
            "failed" => Some(Self::Failed),
            "interrupted" => Some(Self::Interrupted),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Pulsation {
    pub terminal_reason: Option<String>,
    pub id: String,
    pub project_id: String,
    pub feature_name: String,
    pub stage: PulsationStage,
    pub status: PulsationStatus,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Checkpoint {
    pub id: String,
    pub pulsation_id: String,
    pub stage: PulsationStage,
    pub status: CheckpointStatus,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: String,
    pub pulsation_id: Option<String>,
    pub kind: String,
    pub status: JobStatus,
    pub attempts: i64,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub cancel_requested_at: Option<String>,
    pub available_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRun {
    pub id: String,
    pub job_id: String,
    pub agent_name: String,
    pub status: String,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub estimated_cost_usd: Option<f64>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    pub id: String,
    pub pulsation_id: String,
    pub name: String,
    pub kind: String,
    pub relative_path: Option<String>,
    pub content_sha256: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PulseEvent {
    pub id: String,
    pub pulsation_id: String,
    pub event_type: String,
    pub from_status: Option<PulsationStatus>,
    pub to_status: Option<PulsationStatus>,
    pub from_stage: Option<PulsationStage>,
    pub to_stage: Option<PulsationStage>,
    pub message: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PulsationRuntimeSnapshot {
    pub details: Vec<crate::orchestration::PulsationCoordinationSnapshot>,
    pub coordination: Option<crate::orchestration::ProjectCoordinationSnapshot>,
    pub main_agents_count: usize,
    pub hitl_checkpoints: Vec<HitlCheckpoint>,
    pub failure_checkpoints: Vec<StageFailureCheckpoint>,
    pub pulsations: Vec<Pulsation>,
    pub hitl_waiting_count: usize,
    pub failure_waiting_count: usize,
    pub background_agents_count: usize,
    /// What holds the project's runtime slots right now — the names behind
    /// `main_agents_count` and `background_agents_count`. Main first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_jobs: Vec<ActiveRuntimeJob>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brownfield_coverage: Option<BrownfieldCoverageMetrics>,
}

/// One held runtime slot. Stage runs, Learning reports and index jobs own a `jobs` row;
/// publication and cleanup work holds a slot without one, so `kind` falls back to the slot
/// token's prefix (`publication`, `learning-publish`, `component-publish`, `busy`).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ActiveRuntimeJob {
    pub job_class: String,
    pub kind: String,
    pub pulsation_id: Option<String>,
    pub feature_name: Option<String>,
    pub acquired_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatePulsationRequest {
    pub project_id: String,
    pub feature_name: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatePulsationResponse {
    pub pulsation: Pulsation,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListPulsationsRequest {
    pub project_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListPulsationsResponse {
    pub pulsations: Vec<Pulsation>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetPulsationRuntimeSnapshotRequest {
    pub project_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetPulsationRuntimeSnapshotResponse {
    pub snapshot: PulsationRuntimeSnapshot,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyHitlDecisionRequest {
    pub pulsation_id: String,
    pub checkpoint_id: String,
    pub expected_state_version: u64,
    pub feedback: Option<String>,
    pub decision: HitlDecision,
}

/// The explicit decision a revision needs once the pulsation reached its
/// human-round threshold (ADR-032). The reason is the developer's own text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum ConvergenceAck {
    /// Request this revision anyway.
    ContinueBeyondThreshold { reason: String },
    /// Revise from the Specification instead; the round window restarts.
    Replan { reason: String },
}

impl ConvergenceAck {
    pub fn reason(&self) -> &str {
        match self {
            Self::ContinueBeyondThreshold { reason } | Self::Replan { reason } => reason,
        }
    }

    pub const fn as_db_value(&self) -> &'static str {
        match self {
            Self::ContinueBeyondThreshold { .. } => "continue_beyond_threshold",
            Self::Replan { .. } => "replan",
        }
    }
}

/// One recorded decision beyond the threshold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConvergenceDecisionRecord {
    pub stage: String,
    pub decision: String,
    pub rounds_at_decision: u32,
    pub threshold: u32,
    pub reason: String,
    pub created_at: String,
}

/// How far a pulsation's current input revision is from converging
/// (ADR-032 M4), derived from durable rows only. Human rounds and technical
/// attempts are counted separately; only human rounds meet the threshold.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConvergenceStatus {
    /// Pipeline v4 only: earlier pipelines keep their unlimited revisions.
    pub applies: bool,
    pub input_revision: u64,
    pub threshold: u32,
    /// Human rounds (checkpoint and failure-path revisions) since the last replan.
    pub rounds: u32,
    /// Request revision at artifact checkpoints, per stage, in this input revision.
    pub human_rounds: Vec<(String, u32)>,
    pub failure_revisions: u32,
    pub retries: u32,
    /// Model invocations of composed work units, corrections and fallbacks included.
    pub technical_attempts: u32,
    /// Of those, invocations drawn from the extra (repair/retry/fallback) pool.
    pub extra_invocations: u32,
    /// Accepted Code candidates of this input revision.
    pub candidate_generations: u32,
    pub decisions: Vec<ConvergenceDecisionRecord>,
}

impl ConvergenceStatus {
    /// Every further Request revision needs an explicit decision.
    pub fn threshold_reached(&self) -> bool {
        self.applies && self.rounds >= self.threshold
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HitlCheckpoint {
    pub pulsation_id: String,
    pub checkpoint_id: String,
    pub state_version: u64,
}

/// Complete persisted output of the job that opened an artifact-review checkpoint.
/// Workspace files may have changed since generation and are never a review source.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HitlReviewDocument {
    pub artifact_id: String,
    pub name: String,
    pub kind: String,
    pub relative_path: Option<String>,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliFallbackCheckpoint {
    pub checkpoint_id: String,
    pub pulsation_id: String,
    pub project_id: String,
    pub feature_name: String,
    pub agent_role: AgentRole,
    pub terminal_run_id: Option<String>,
    pub error_code: String,
    pub diagnostic: String,
    pub state_version: u64,
    pub stage: PulsationStage,
    pub created_at: String,
}

/// A stage failure waiting on a human decision. `allow_retry`/`allow_revision`
/// are persisted rather than recomputed so the UI and the host agree even if the
/// classification table changes under a waiting checkpoint.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StageFailureCheckpoint {
    pub checkpoint_id: String,
    pub pulsation_id: String,
    pub project_id: String,
    pub feature_name: String,
    pub stage: PulsationStage,
    pub failure_class: String,
    pub error_code: String,
    pub diagnostic: String,
    pub allow_retry: bool,
    pub allow_revision: bool,
    pub revision_stage: Option<PulsationStage>,
    pub attempts: i64,
    pub state_version: u64,
    pub created_at: String,
    /// Composed stages (ADR-022): every revision target the host permits,
    /// the extra permits the failed unit could still take, and the
    /// execution the checkpoint parks. Empty/`None` for legacy failures.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_revision_targets: Vec<PulsationStage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining_extra_invocations: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage_execution_id: Option<String>,
    /// The scope request a `scope_insufficient` kept with this checkpoint
    /// (ADR-035 M4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_request: Option<ScopeRequestView>,
}

/// The model output that was rejected on the last attempt, so the operator can see
/// what actually happened before deciding. Absent when nothing was persisted.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StageFailureDetail {
    pub checkpoint_id: String,
    pub raw_output: Option<String>,
    pub validation_error: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveStageFailureRequest {
    pub checkpoint_id: String,
    pub expected_state_version: u64,
    pub decision: StageFailureDecision,
    pub feedback: Option<String>,
}

/// A waiting failure checkpoint, as the decision that settles it finds it.
struct WaitingFailure {
    pulsation_id: String,
    stage_raw: String,
    producer_job_id: String,
    class_raw: String,
    allow_retry: bool,
    allow_revision: bool,
    revision_stage_raw: Option<String>,
    error_code: String,
    stage_execution_id: Option<String>,
    allowed_targets_json: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveStageFailureResponse {
    pub pulsation: Pulsation,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveCliFallbackRequest {
    pub checkpoint_id: String,
    pub expected_state_version: u64,
    pub decision: CliFallbackDecision,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveCliFallbackResponse {
    pub pulsation: Pulsation,
}

/// Ask the host to validate the (updated) CLI behind a `cli_version_unvalidated` fallback and,
/// when the contract probe passes, resume the pulsation exactly like `RetryCli`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidateCliFallbackRequest {
    pub checkpoint_id: String,
    pub expected_state_version: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidateCliFallbackResponse {
    pub report: crate::terminals::CliValidationReport,
    /// Present when the probe passed and the pulsation was resumed.
    pub pulsation: Option<Pulsation>,
}

/// Error code of a fallback opened before dispatch because the CLI is newer than validated.
pub const CLI_VERSION_UNVALIDATED: &str = "cli_version_unvalidated";
/// The stage's CLI is low on its plan window (ADR-028 pre-flight); advisory.
pub const CLI_QUOTA_LOW: &str = "cli_quota_low";
/// Error code the same fallback carries after a validation probe failed: only switching the
/// provider or aborting remain.
pub const CLI_VALIDATION_FAILED: &str = "cli_validation_failed";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearExecutionOverrideRequest {
    pub pulsation_id: String,
    pub agent_role: AgentRole,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearExecutionOverrideResponse {
    /// False when there was nothing to clear (the situation already
    /// resolved itself, e.g. the pulsation advanced past that stage).
    pub cleared: bool,
    /// True when a pending job was woken to retry on the default backend.
    pub retried: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyHitlDecisionResponse {
    pub pulsation: Pulsation,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListPulsationArtifactsRequest {
    pub pulsation_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListPulsationArtifactsResponse {
    pub artifacts: Vec<Artifact>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetAgentRuntimeConfigResponse {
    pub config: AgentRuntimeConfig,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAgentRuntimeConfigRequest {
    pub provider: Option<LlmProvider>,
    pub model: Option<String>,
    pub temperature: Option<f32>,
    pub max_output_tokens: Option<u32>,
    pub timeout_ms: Option<u64>,
    pub max_retries: Option<u8>,
    pub context_chars_limit: Option<usize>,
    pub max_context_files: Option<usize>,
    pub context_strategy: Option<ContextStrategy>,
    pub embedding_enabled: Option<bool>,
    pub embedding_provider: Option<EmbeddingProvider>,
    pub embedding_model: Option<String>,
    pub embedding_top_k: Option<usize>,
    pub adaptive_context_mode: Option<AdaptiveContextMode>,
    pub dynamic_context_policy: Option<DynamicContextPolicy>,
    pub routing_v2_mode: Option<RoutingMode>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAgentRuntimeConfigResponse {
    pub config: AgentRuntimeConfig,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReindexWorkspaceEmbeddingsRequest {
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReindexWorkspaceEmbeddingsResponse {
    pub queued_count: usize,
    pub workspace_path: String,
    pub model: String,
    pub indexed_count: usize,
    pub skipped_count: usize,
    pub deleted_count: usize,
    pub failed_count: usize,
}

/// Three independent, ordered facts about whether embeddings can work at all: each one gates
/// the next, so the UI can point at exactly the step that is blocking the developer instead of
/// surfacing a raw HTTP error.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingReadinessReport {
    pub model: String,
    pub ollama_reachable: bool,
    pub model_installed: bool,
    pub embedding_call_ok: bool,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveProviderApiKeyRequest {
    pub provider: LlmProvider,
    pub api_key: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveProviderApiKeyResponse {
    pub provider: LlmProvider,
    pub saved: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearProviderApiKeyRequest {
    pub provider: LlmProvider,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearProviderApiKeyResponse {
    pub provider: LlmProvider,
    pub removed: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetProviderKeyStatusesResponse {
    pub providers: Vec<ProviderKeyStatus>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListProviderProfilesResponse {
    pub profiles: Vec<ProviderProfile>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListProviderConnectionsResponse {
    pub connections: Vec<ProviderConnection>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpsertProviderConnectionResponse {
    pub connection: ProviderConnection,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteProviderConnectionResponse {
    pub deleted: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListProviderRoutesResponse {
    pub routes: Vec<ProviderRoute>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpsertProviderRouteResponse {
    pub route: ProviderRoute,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteProviderRouteResponse {
    pub deleted: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListProviderRouteBindingsResponse {
    pub bindings: Vec<ProviderRouteBinding>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpsertProviderRouteBindingResponse {
    pub binding: ProviderRouteBinding,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListFreeFormProviderRouteBindingsResponse {
    pub bindings: Vec<FreeFormProviderRouteBinding>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpsertFreeFormProviderRouteBindingResponse {
    pub binding: FreeFormProviderRouteBinding,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartFreeFormApiRunRequest {
    pub project_id: String,
    pub slot_id: String,
    pub prompt: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartFreeFormApiRunResponse {
    pub execution_id: String,
    pub generation_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelAgentApiExecutionRequest {
    pub execution_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelAgentApiExecutionResponse {
    pub cancelled: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListAgentApiExecutionsRequest {
    pub project_id: String,
    pub target: AgentExecutionTarget,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListAgentApiExecutionsResponse {
    pub executions: Vec<LlmExecutionListItem>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetLlmResilienceSnapshotResponse {
    pub snapshot: LlmResilienceSnapshot,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResetLlmResilienceStateResponse {
    pub reset_count: usize,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetLlmExecutionSnapshotRequest {
    pub execution_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetLlmExecutionSnapshotResponse {
    pub snapshot: LlmExecutionSnapshot,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetAgentApiExecutionSnapshotRequest {
    pub project_id: String,
    pub target: AgentExecutionTarget,
    pub execution_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetLlmRoutingMetricsResponse {
    pub metrics: Vec<LlmRoutingMetric>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewContextCompressionResponse {
    pub preview: CompressionPreview,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelPulsationJobsRequest {
    pub pulsation_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelPulsationJobsResponse {
    pub cancelled_jobs: usize,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListAgentRunsRequest {
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub pulsation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRunListItem {
    pub id: String,
    pub job_id: String,
    pub agent_name: String,
    pub status: String,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub estimated_cost_usd: Option<f64>,
    pub last_error: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub raw_output: Option<String>,
    pub structured_output: Option<Value>,
    pub created_at: String,
    pub updated_at: String,
    pub pulsation_id: Option<String>,
    pub pulsation_name: Option<String>,
    pub pulsation_status: Option<String>,
    pub job_kind: Option<String>,
    pub duration_ms: Option<i64>,
    pub cancellation_reason: Option<String>,
    pub validation_error: Option<String>,
    pub tool_trace: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListAgentRunsResponse {
    pub runs: Vec<AgentRunListItem>,
}

#[derive(Debug, Default)]
struct RuntimeControl {
    cancellations: Mutex<HashMap<String, CancellationToken>>,
    manual_api_cancellations: Mutex<HashMap<String, CancellationToken>>,
    wakeup: tokio::sync::Notify,
    pending_user_cancels: Mutex<HashMap<String, usize>>,
    #[cfg(test)]
    publication_gate: Mutex<Option<(Arc<std::sync::Barrier>, Arc<std::sync::Barrier>)>>,
    #[cfg(test)]
    preparation_gate: Mutex<Option<(Arc<std::sync::Barrier>, Arc<std::sync::Barrier>)>>,
}

#[derive(Clone)]
pub struct PersistenceService {
    db_path: PathBuf,
    runtime_control: Arc<RuntimeControl>,
    embedding_gateway: Arc<dyn EmbeddingGateway>,
    llm_repository: SqliteLlmRepository,
    credentials: Arc<dyn CredentialStore>,
    llm_router: Arc<ProviderRouter>,
    api_model_catalog: ApiModelCatalogService,
    terminal_service: Arc<Mutex<Option<Arc<TerminalService>>>>,
    // Attached post-construction (not a constructor param) purely to avoid changing the
    // signature of `initialize_with_dependencies`, which many tests call directly. Unlike
    // `terminal_service`, there is no circular-dependency reason for this shape here.
    indexing_events: Arc<Mutex<Option<Arc<dyn IndexingEventSink>>>>,
    indexing_last_emitted: Arc<Mutex<HashMap<String, IndexingEvent>>>,
    // Same "attach after construction" shape as `indexing_events`, for the same reason.
    model_pull_events: Arc<Mutex<Option<Arc<dyn ModelPullEventSink>>>>,
    pub(crate) document_cache: crate::document_cache::SharedDocumentCache,
    clock: Arc<dyn Clock>,
    /// ADR-037 M4: pulsations whose digest failed in this process. The
    /// background lane skips them until the app restarts.
    digest_failures: Arc<Mutex<HashSet<String>>>,
}

impl PersistenceService {
    pub fn database_path(&self) -> &Path {
        &self.db_path
    }

    pub fn initialize(app_data_dir: &Path) -> Result<Self, PersistenceServiceError> {
        Self::initialize_with_event_sink(app_data_dir, Arc::new(NoopEventSink))
    }

    pub fn initialize_with_event_sink(
        app_data_dir: &Path,
        event_sink: Arc<dyn LlmEventSink>,
    ) -> Result<Self, PersistenceServiceError> {
        let llm_client = Client::new();
        Self::initialize_with_dependencies(
            app_data_dir,
            event_sink,
            Arc::new(KeyringCredentialStore),
            Arc::new(OllamaEmbeddingGateway::new(llm_client.clone())),
            Arc::new(SystemClock),
            Arc::new(StableJitter),
            llm_client,
        )
    }

    pub fn initialize_with_dependencies(
        app_data_dir: &Path,
        event_sink: Arc<dyn LlmEventSink>,
        credentials: Arc<dyn CredentialStore>,
        embedding_gateway: Arc<dyn EmbeddingGateway>,
        clock: Arc<dyn Clock>,
        jitter: Arc<dyn JitterSource>,
        llm_client: Client,
    ) -> Result<Self, PersistenceServiceError> {
        fs::create_dir_all(app_data_dir)?;
        let db_path = app_data_dir.join("hard-vibe-code.db");
        let mut connection = Connection::open(&db_path)?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.busy_timeout(Duration::from_secs(5))?;

        crate::orchestration::upgrade::backup_before_upgrade(&connection, app_data_dir)?;
        let runner = embedded::migrations::runner();
        crate::orchestration::upgrade::reconcile_migration_checksums(&connection, &runner)?;
        // V24 rebuilds CHECK-constrained tables without cascading into their history.
        connection.pragma_update(None, "foreign_keys", "OFF")?;
        runner
            .run(&mut connection)
            .map_err(PersistenceServiceError::Migration)?;
        let broken_references: i64 =
            connection.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        if broken_references != 0 {
            return Err(PersistenceServiceError::Validation(
                "Migration left invalid foreign keys".into(),
            ));
        }
        crate::project_deletion::recover_project_deletions(&connection, app_data_dir)?;
        // A report outlives its pulsation only until the next start.
        if let Err(error) = crate::reports::store::sweep_orphan_reports(&connection, app_data_dir) {
            eprintln!("pulsation report cleanup deferred: {error}");
        }
        crate::orchestration::upgrade::adapt_legacy(&mut connection)?;

        let llm_repository = SqliteLlmRepository::new(&db_path);
        llm_repository.ensure_legacy_configuration()?;
        let llm_router = Arc::new(ProviderRouter::new(
            llm_repository.clone(),
            llm_client.clone(),
            credentials.clone(),
            event_sink,
            clock.clone(),
            jitter,
        ));
        let api_model_catalog =
            ApiModelCatalogService::new(llm_repository.clone(), credentials.clone(), llm_client);
        Ok(Self {
            db_path,
            runtime_control: Arc::new(RuntimeControl::default()),
            embedding_gateway,
            llm_repository,
            credentials,
            llm_router,
            api_model_catalog,
            terminal_service: Arc::new(Mutex::new(None)),
            indexing_events: Arc::new(Mutex::new(None)),
            indexing_last_emitted: Arc::new(Mutex::new(HashMap::new())),
            model_pull_events: Arc::new(Mutex::new(None)),
            document_cache: Default::default(),
            clock,
            digest_failures: Default::default(),
        })
    }

    /// ADR-037 M4: derives the digest of the newest finished pulsation that
    /// has none and whose seal is not pending. `false` when none waits. A
    /// pulsation whose derivation failed is skipped until the app restarts,
    /// so it neither blocks the older ones nor fails every second.
    pub fn digest_next_pulsation(&self) -> Result<bool, PersistenceServiceError> {
        let db = self.connection()?;
        let failed = self
            .digest_failures
            .lock()
            .map(|failed| failed.clone())
            .unwrap_or_default();
        let candidates = db
            .prepare(
                "SELECT p.id FROM pulsations p WHERE p.status IN ('done','failed')
                   AND NOT EXISTS(SELECT 1 FROM companion_pulsation_digests d WHERE d.pulsation_id=p.id)
                   AND NOT EXISTS(SELECT 1 FROM attestation_seal_intents i
                                  WHERE i.pulsation_id=p.id AND i.state='pending')
                 ORDER BY p.updated_at DESC,p.id LIMIT ?1",
            )?
            .query_map([failed.len() as i64 + 1], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let Some(pulsation_id) = candidates.into_iter().find(|id| !failed.contains(id)) else {
            return Ok(false);
        };
        if let Err(error) = crate::exchange::digest::refresh(&db, &pulsation_id) {
            if let Ok(mut failed) = self.digest_failures.lock() {
                failed.insert(pulsation_id);
            }
            return Err(error);
        }
        Ok(true)
    }

    /// ADR-037 M2: where a new follower of the project's exchange log starts.
    pub fn exchange_cursor(&self, project_id: &str) -> Result<i64, PersistenceServiceError> {
        Ok(crate::exchange::log::project_cursor(
            &self.connection()?,
            project_id,
        )?)
    }

    /// ADR-037 M2: the project's exchange log items after `after_global_seq`.
    pub fn exchange_read_after(
        &self,
        project_id: &str,
        after_global_seq: i64,
        limit: usize,
    ) -> Result<crate::exchange::log::Page, PersistenceServiceError> {
        Ok(crate::exchange::log::read_after(
            &self.connection()?,
            project_id,
            after_global_seq,
            limit,
        )?)
    }

    pub(crate) fn connection(&self) -> Result<Connection, PersistenceServiceError> {
        let connection = Connection::open(&self.db_path)?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.busy_timeout(Duration::from_secs(5))?;
        Ok(connection)
    }

    pub fn attach_terminal_service(&self, service: Arc<TerminalService>) {
        *self
            .terminal_service
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(service);
    }

    /// Whether every stage of a new pulsation can reach its model, from the
    /// same configuration the runtime reads and the last CLI discovery
    /// (never a new probe).
    pub(crate) fn stage_preflight(
        &self,
        db: &Connection,
        project_id: &str,
    ) -> Result<
        Vec<crate::orchestration::stage_preflight::StagePreflightIssue>,
        PersistenceServiceError,
    > {
        let cached = self
            .terminal_service
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .and_then(|terminals| terminals.cached_discovery());
        let discovery = cached
            .as_ref()
            .map(|(items, fresh)| (items.as_slice(), *fresh));
        let mut issues = crate::orchestration::stage_preflight::check_stages(
            db,
            &self.llm_repository,
            project_id,
            discovery,
        )?;
        issues.extend(self.quota_preflight_warnings(db, project_id)?);
        Ok(issues)
    }

    /// Non-blocking dispatch warnings (ADR-028 §6): one per CLI whose tightest plan window is
    /// low. Each stage on that CLI still asks before it runs (`quota_preflight_gate`).
    fn quota_preflight_warnings(
        &self,
        db: &Connection,
        project_id: &str,
    ) -> Result<
        Vec<crate::orchestration::stage_preflight::StagePreflightIssue>,
        PersistenceServiceError,
    > {
        let Some(terminals) = self
            .terminal_service
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        else {
            return Ok(Vec::new());
        };
        let mut seen = HashSet::new();
        let mut issues = Vec::new();
        for (_, stage, role) in crate::orchestration::catalog::STAGES {
            let backend: Option<String> = db
                .query_row(
                    "SELECT backend FROM cli_agent_configs
                     WHERE project_id=?1 AND agent_role=?2 AND execution_mode='cli_process'",
                    params![project_id, role.as_str()],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(backend) = backend.as_deref().and_then(CliBackend::parse) else {
                continue;
            };
            if !seen.insert(backend) {
                continue;
            }
            if let Some(finding) = terminals.quota_meter().preflight(backend, None) {
                issues.push(crate::orchestration::stage_preflight::StagePreflightIssue {
                    stage: stage.to_string(),
                    role: role.as_str().to_string(),
                    blocking: false,
                    message: format!(
                        "Plan quota: {} Stages on {} ask before they run.",
                        finding.message(),
                        backend.label()
                    ),
                });
            }
        }
        Ok(issues)
    }

    pub fn attach_indexing_event_sink(&self, sink: Arc<dyn IndexingEventSink>) {
        *self
            .indexing_events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(sink);
    }

    pub fn attach_model_pull_event_sink(&self, sink: Arc<dyn ModelPullEventSink>) {
        *self
            .model_pull_events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(sink);
    }

    /// Publishes `event` unless it is identical to the last event emitted for its workspace,
    /// so a paused/steady state does not re-publish every supervisor tick.
    fn emit_indexing_event(&self, event: IndexingEvent) {
        let key = event.workspace_path().to_string();
        let mut last = self
            .indexing_last_emitted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if last.get(&key) == Some(&event) {
            return;
        }
        last.insert(key, event.clone());
        drop(last);
        if let Some(sink) = self
            .indexing_events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        {
            sink.emit(&event);
        }
    }

    fn emit_model_pull_event(&self, event: ModelPullEvent) {
        if let Some(sink) = self
            .model_pull_events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        {
            sink.emit(&event);
        }
    }

    fn now_iso() -> String {
        Utc::now().to_rfc3339()
    }

    fn read_runtime_setting(
        connection: &Connection,
        key: &str,
    ) -> Result<Option<String>, PersistenceServiceError> {
        let mut statement = connection.prepare(
            "SELECT value
             FROM runtime_settings
             WHERE key = ?1
             LIMIT 1",
        )?;
        statement
            .query_row(params![key], |row| row.get("value"))
            .optional()
            .map_err(PersistenceServiceError::from)
    }

    fn upsert_runtime_setting(
        connection: &Connection,
        key: &str,
        value: &str,
    ) -> Result<(), PersistenceServiceError> {
        connection.execute(
            "INSERT INTO runtime_settings (key, value, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(key)
             DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![key, value, Self::now_iso()],
        )?;
        Ok(())
    }

    fn load_runtime_config(&self) -> Result<AgentRuntimeConfig, PersistenceServiceError> {
        Self::runtime_config_from(&self.connection()?)
    }

    /// The runtime configuration read on `connection`, for callers inside an
    /// open transaction: a second connection there can deadlock against a
    /// writer waiting to commit.
    pub(crate) fn runtime_config_from(
        connection: &Connection,
    ) -> Result<AgentRuntimeConfig, PersistenceServiceError> {
        let mut config = AgentRuntimeConfig::default();

        if let Some(raw_provider) =
            Self::read_runtime_setting(connection, SETTING_RUNTIME_PROVIDER)?
            && let Some(provider) = LlmProvider::parse(raw_provider.trim())
        {
            config.provider = provider;
        }

        if let Some(raw_model) = Self::read_runtime_setting(connection, SETTING_RUNTIME_MODEL)? {
            config.model = raw_model;
        }

        if let Some(raw_temperature) =
            Self::read_runtime_setting(connection, SETTING_RUNTIME_TEMPERATURE)?
            && let Ok(parsed) = raw_temperature.parse::<f32>()
        {
            config.temperature = parsed;
        }

        if let Some(raw_max_output_tokens) =
            Self::read_runtime_setting(connection, SETTING_RUNTIME_MAX_OUTPUT_TOKENS)?
            && let Ok(parsed) = raw_max_output_tokens.parse::<u32>()
        {
            config.max_output_tokens = parsed;
        }

        if let Some(raw_timeout_ms) =
            Self::read_runtime_setting(connection, SETTING_RUNTIME_TIMEOUT_MS)?
            && let Ok(parsed) = raw_timeout_ms.parse::<u64>()
        {
            config.timeout_ms = parsed;
        }

        if let Some(raw_max_retries) =
            Self::read_runtime_setting(connection, SETTING_RUNTIME_MAX_RETRIES)?
            && let Ok(parsed) = raw_max_retries.parse::<u8>()
        {
            config.max_retries = parsed;
        }

        if let Some(raw_context_limit) =
            Self::read_runtime_setting(connection, SETTING_RUNTIME_CONTEXT_LIMIT)?
            && let Ok(parsed) = raw_context_limit.parse::<usize>()
        {
            config.context_chars_limit = parsed;
        }

        if let Some(raw_max_context_files) =
            Self::read_runtime_setting(connection, SETTING_RUNTIME_MAX_CONTEXT_FILES)?
            && let Ok(parsed) = raw_max_context_files.parse::<usize>()
        {
            config.max_context_files = parsed;
        }

        if let Some(raw_strategy) =
            Self::read_runtime_setting(connection, SETTING_RUNTIME_CONTEXT_STRATEGY)?
            && let Some(parsed) = ContextStrategy::parse(raw_strategy.trim())
        {
            config.context_strategy = parsed;
        }

        if let Some(raw_embedding_enabled) =
            Self::read_runtime_setting(connection, SETTING_RUNTIME_EMBEDDING_ENABLED)?
        {
            config.embedding_enabled = matches!(
                raw_embedding_enabled.trim(),
                "1" | "true" | "TRUE" | "yes" | "YES"
            );
        }

        if let Some(raw_embedding_provider) =
            Self::read_runtime_setting(connection, SETTING_RUNTIME_EMBEDDING_PROVIDER)?
            && let Some(parsed) = EmbeddingProvider::parse(raw_embedding_provider.trim())
        {
            config.embedding_provider = parsed;
        }

        if let Some(raw_embedding_model) =
            Self::read_runtime_setting(connection, SETTING_RUNTIME_EMBEDDING_MODEL)?
        {
            config.embedding_model = raw_embedding_model;
        }

        if let Some(raw_embedding_top_k) =
            Self::read_runtime_setting(connection, SETTING_RUNTIME_EMBEDDING_TOP_K)?
            && let Ok(parsed) = raw_embedding_top_k.parse::<usize>()
        {
            config.embedding_top_k = parsed;
        }

        if let Some(raw_mode) =
            Self::read_runtime_setting(connection, SETTING_RUNTIME_ADAPTIVE_CONTEXT_MODE)?
            && let Some(parsed) = AdaptiveContextMode::parse(raw_mode.trim())
        {
            config.adaptive_context_mode = parsed;
        }

        if let Some(raw_policy) =
            Self::read_runtime_setting(connection, SETTING_RUNTIME_DYNAMIC_CONTEXT_POLICY)?
            && let Ok(parsed) = serde_json::from_str::<DynamicContextPolicy>(&raw_policy)
        {
            config.dynamic_context_policy = parsed;
        }

        if let Some(raw_mode) =
            Self::read_runtime_setting(connection, SETTING_RUNTIME_ROUTING_V2_MODE)?
        {
            config.routing_v2_mode = RoutingMode::parse(&raw_mode);
        }

        Ok(config.sanitized())
    }

    fn persist_runtime_config(
        &self,
        config: &AgentRuntimeConfig,
    ) -> Result<AgentRuntimeConfig, PersistenceServiceError> {
        let sanitized = config.clone().sanitized();
        let connection = self.connection()?;

        Self::upsert_runtime_setting(
            &connection,
            SETTING_RUNTIME_PROVIDER,
            sanitized.provider.as_str(),
        )?;
        Self::upsert_runtime_setting(&connection, SETTING_RUNTIME_MODEL, &sanitized.model)?;
        Self::upsert_runtime_setting(
            &connection,
            SETTING_RUNTIME_TEMPERATURE,
            &sanitized.temperature.to_string(),
        )?;
        Self::upsert_runtime_setting(
            &connection,
            SETTING_RUNTIME_MAX_OUTPUT_TOKENS,
            &sanitized.max_output_tokens.to_string(),
        )?;
        Self::upsert_runtime_setting(
            &connection,
            SETTING_RUNTIME_TIMEOUT_MS,
            &sanitized.timeout_ms.to_string(),
        )?;
        Self::upsert_runtime_setting(
            &connection,
            SETTING_RUNTIME_MAX_RETRIES,
            &sanitized.max_retries.to_string(),
        )?;
        Self::upsert_runtime_setting(
            &connection,
            SETTING_RUNTIME_CONTEXT_LIMIT,
            &sanitized.context_chars_limit.to_string(),
        )?;
        Self::upsert_runtime_setting(
            &connection,
            SETTING_RUNTIME_MAX_CONTEXT_FILES,
            &sanitized.max_context_files.to_string(),
        )?;
        Self::upsert_runtime_setting(
            &connection,
            SETTING_RUNTIME_CONTEXT_STRATEGY,
            sanitized.context_strategy.as_str(),
        )?;
        Self::upsert_runtime_setting(
            &connection,
            SETTING_RUNTIME_EMBEDDING_ENABLED,
            if sanitized.embedding_enabled {
                "true"
            } else {
                "false"
            },
        )?;
        Self::upsert_runtime_setting(
            &connection,
            SETTING_RUNTIME_EMBEDDING_PROVIDER,
            sanitized.embedding_provider.as_str(),
        )?;
        Self::upsert_runtime_setting(
            &connection,
            SETTING_RUNTIME_EMBEDDING_MODEL,
            &sanitized.embedding_model,
        )?;
        Self::upsert_runtime_setting(
            &connection,
            SETTING_RUNTIME_EMBEDDING_TOP_K,
            &sanitized.embedding_top_k.to_string(),
        )?;
        Self::upsert_runtime_setting(
            &connection,
            SETTING_RUNTIME_ADAPTIVE_CONTEXT_MODE,
            sanitized.adaptive_context_mode.as_str(),
        )?;
        Self::upsert_runtime_setting(
            &connection,
            SETTING_RUNTIME_DYNAMIC_CONTEXT_POLICY,
            &serde_json::to_string(&sanitized.dynamic_context_policy)
                .map_err(|error| PersistenceServiceError::Validation(error.to_string()))?,
        )?;
        Self::upsert_runtime_setting(
            &connection,
            SETTING_RUNTIME_ROUTING_V2_MODE,
            sanitized.routing_v2_mode.as_str(),
        )?;

        Ok(sanitized)
    }

    fn register_job_cancellation(&self, job_id: &str, token: CancellationToken) {
        if let Ok(mut guard) = self.runtime_control.cancellations.lock() {
            guard.insert(job_id.to_string(), token);
        }
    }

    fn unregister_job_cancellation(&self, job_id: &str) {
        if let Ok(mut guard) = self.runtime_control.cancellations.lock() {
            guard.remove(job_id);
        }
    }

    pub fn cancel_all_active_jobs(&self) -> usize {
        let Ok(guard) = self.runtime_control.cancellations.lock() else {
            return 0;
        };
        let cancellations = guard.clone();
        drop(guard);
        for (id, cancellation) in &cancellations {
            if let Ok(db) = self.connection() {
                let _ = db.execute("UPDATE jobs SET cancellation_reason='shutdown',cancel_requested_at=?2 WHERE id=?1 AND status='running' AND cancellation_reason IS NULL",params![id,Self::now_iso()]);
            }
            cancellation.cancel();
        }
        let manual_api = self
            .runtime_control
            .manual_api_cancellations
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        for cancellation in manual_api.values() {
            cancellation.cancel();
        }
        cancellations.len() + manual_api.len()
    }

    fn request_job_cancellation(&self, job_id: &str) -> bool {
        if let Ok(guard) = self.runtime_control.cancellations.lock()
            && let Some(token) = guard.get(job_id)
        {
            token.cancel();
            return true;
        }

        false
    }

    fn job_cancellation_requested(&self, job_id: &str) -> Result<bool, PersistenceServiceError> {
        let connection = self.connection()?;
        let requested: bool = connection.query_row(
            "SELECT cancel_requested_at IS NOT NULL FROM jobs WHERE id = ?1",
            params![job_id],
            |row| row.get(0),
        )?;
        Ok(requested)
    }

    fn normalize_workspace_path(raw: &str) -> Result<String, PersistenceServiceError> {
        let normalized = raw.trim();
        if normalized.is_empty() {
            return Err(PersistenceServiceError::Validation(
                "workspacePath cannot be empty.".to_string(),
            ));
        }
        Ok(normalized.to_string())
    }

    fn normalize_project_id(raw: &str) -> Result<String, PersistenceServiceError> {
        let normalized = raw.trim();
        if normalized.is_empty() {
            return Err(PersistenceServiceError::Validation(
                "projectId cannot be empty.".to_string(),
            ));
        }
        Ok(normalized.to_string())
    }

    fn normalize_workspace_view_mode(raw: &str) -> Result<String, PersistenceServiceError> {
        let normalized = raw.trim();
        if normalized != "editor" && normalized != "hitl" {
            return Err(PersistenceServiceError::Validation(
                "workspaceViewMode must be `editor` or `hitl`.".to_string(),
            ));
        }

        Ok(normalized.to_string())
    }

    fn normalize_active_view(raw: &str) -> Result<String, PersistenceServiceError> {
        let normalized = raw.trim();
        if !matches!(
            normalized,
            "home"
                | "workspace"
                | "pulsations"
                | "vault"
                | "chat"
                | "notes"
                | "canvas"
                | "agents"
                | "settings"
                | "logs"
                | "forge"
                | "tasks"
                | "feature-forge"
        ) {
            return Err(PersistenceServiceError::Validation(
                "activeView must be `home`, `workspace`, `pulsations`, `vault`, `chat`, `notes`, `canvas`, `agents`, `settings`, `logs` or `forge`."
                    .to_string(),
            ));
        }

        Ok(normalized.to_string())
    }

    fn derive_project_name(workspace_path: &str) -> String {
        Path::new(workspace_path)
            .file_name()
            .and_then(|value| value.to_str())
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .unwrap_or("Workspace")
            .to_string()
    }

    fn map_project(row: &rusqlite::Row<'_>) -> rusqlite::Result<Project> {
        let is_archived: i64 = row.get("is_archived")?;
        Ok(Project {
            id: row.get("id")?,
            name: row.get("name")?,
            workspace_path: row.get("workspace_path")?,
            is_archived: is_archived != 0,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
            last_opened_at: row.get("last_opened_at")?,
        })
    }

    fn normalize_pulsation_id(raw: &str) -> Result<String, PersistenceServiceError> {
        let normalized = raw.trim();
        if normalized.is_empty() {
            return Err(PersistenceServiceError::Validation(
                "pulsationId cannot be empty.".to_string(),
            ));
        }
        Ok(normalized.to_string())
    }

    fn normalize_feature_name(raw: &str) -> Result<String, PersistenceServiceError> {
        let normalized = raw.trim();
        if normalized.is_empty()
            || raw.chars().count() > crate::orchestration::MAX_DESCRIPTION_CHARS
        {
            return Err(PersistenceServiceError::Validation(
                "The request must have 1 to 16,000 Unicode characters.".to_string(),
            ));
        }

        Ok(raw.to_string())
    }

    fn invalid_enum_error(column: &str, value: &str) -> SqlError {
        SqlError::FromSqlConversionFailure(
            0,
            Type::Text,
            Box::new(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Invalid value `{value}` in column `{column}`."),
            )),
        )
    }

    fn map_pulsation(row: &rusqlite::Row<'_>) -> rusqlite::Result<Pulsation> {
        let stage_value: String = row.get("stage")?;
        let status_value: String = row.get("status")?;
        let stage = PulsationStage::from_db_value(&stage_value)
            .ok_or_else(|| Self::invalid_enum_error("stage", &stage_value))?;
        let status = PulsationStatus::from_db_value(&status_value)
            .ok_or_else(|| Self::invalid_enum_error("status", &status_value))?;

        Ok(Pulsation {
            terminal_reason: row.get("terminal_reason")?,
            id: row.get("id")?,
            project_id: row.get("project_id")?,
            feature_name: row.get("feature_name")?,
            stage,
            status,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }

    fn map_artifact(row: &rusqlite::Row<'_>) -> rusqlite::Result<Artifact> {
        Ok(Artifact {
            id: row.get("id")?,
            pulsation_id: row.get("pulsation_id")?,
            name: row.get("name")?,
            kind: row.get("kind")?,
            relative_path: row.get("relative_path")?,
            content_sha256: row.get("content_sha256")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }

    fn map_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<Job> {
        let status_value: String = row.get("status")?;
        let status = JobStatus::from_db_value(&status_value)
            .ok_or_else(|| Self::invalid_enum_error("status", &status_value))?;

        Ok(Job {
            id: row.get("id")?,
            pulsation_id: row.get("pulsation_id")?,
            kind: row.get("kind")?,
            status,
            attempts: row.get("attempts")?,
            last_error: row.get("last_error")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
            started_at: row.get("started_at")?,
            finished_at: row.get("finished_at")?,
            cancel_requested_at: row.get("cancel_requested_at")?,
            available_at: row.get("available_at")?,
        })
    }

    fn find_pulsation_by_id(
        connection: &Connection,
        pulsation_id: &str,
    ) -> Result<Option<Pulsation>, PersistenceServiceError> {
        let mut statement = connection.prepare(
            "SELECT id, project_id, feature_name, stage, status, created_at, updated_at, terminal_reason
             FROM pulsations
             WHERE id = ?1",
        )?;
        let pulsation = statement
            .query_row(params![pulsation_id], Self::map_pulsation)
            .optional()?;
        Ok(pulsation)
    }

    fn is_transition_allowed(from: PulsationStatus, to: PulsationStatus) -> bool {
        match from {
            PulsationStatus::Idle => {
                matches!(to, PulsationStatus::Planning | PulsationStatus::Failed)
            }
            // Checkpoint is reachable from Planning because `close_abandoned_claim`
            // admits `planning` (coordination.rs) and setup can fail before
            // `execute_claimed_job` performs the Planning -> Running transition.
            PulsationStatus::Planning => matches!(
                to,
                PulsationStatus::Running | PulsationStatus::Checkpoint | PulsationStatus::Failed
            ),
            PulsationStatus::Running => matches!(
                to,
                PulsationStatus::Planning
                    | PulsationStatus::Checkpoint
                    | PulsationStatus::Done
                    | PulsationStatus::Failed
            ),
            PulsationStatus::Checkpoint => matches!(
                to,
                PulsationStatus::Planning
                    | PulsationStatus::Running
                    | PulsationStatus::Done
                    | PulsationStatus::Failed
            ),
            PulsationStatus::Done | PulsationStatus::Failed => false,
        }
    }

    /// ADR-037 M2: the exchange log item of a CLI run status the host
    /// changed outside the terminal crate (the API fallback decision).
    fn record_terminal_run_status(
        transaction: &Connection,
        run_id: &str,
        status: &str,
    ) -> Result<(), PersistenceServiceError> {
        let project_id: String = transaction.query_row(
            "SELECT project_id FROM terminal_runs WHERE id=?1",
            [run_id],
            |row| row.get(0),
        )?;
        crate::exchange::log::record_terminal_run_status(transaction, &project_id, run_id, status)?;
        Ok(())
    }

    /// ADR-037 M2: one exchange log item per pulsation transition, in its
    /// transaction. Opening or resolving a checkpoint and resolving a stage
    /// failure always transition the pulsation, so the item's kind names
    /// them and no second item is written.
    fn record_pulsation_transition(
        transaction: &Connection,
        current: &Pulsation,
        next_status: PulsationStatus,
        event_type: &str,
        event_id: &str,
        state_version: u64,
    ) -> Result<(), PersistenceServiceError> {
        use crate::exchange::log::{
            AppendRequest, LogRecordKind, PayloadRef, StreamKind, StreamRef,
        };
        let kind = if next_status == PulsationStatus::Checkpoint {
            LogRecordKind::CheckpointOpened
        } else if current.status == PulsationStatus::Checkpoint
            && event_type.starts_with("stage_failure_")
        {
            LogRecordKind::StageFailureResolved
        } else if current.status == PulsationStatus::Checkpoint {
            LogRecordKind::CheckpointResolved
        } else {
            LogRecordKind::PulsationTransition
        };
        crate::exchange::log::record(
            transaction,
            &AppendRequest::host(
                StreamRef::new(&current.project_id, StreamKind::Pulsation, &current.id),
                kind,
                hvc_contracts::exchange::HostComponent::PipelineState,
                Some(PayloadRef {
                    table: "pulse_events",
                    id: event_id.to_owned(),
                }),
                format!("pulsation:{}:state:{state_version}", current.id),
            ),
        )?;
        Ok(())
    }

    fn transition_pulsation(
        transaction: &rusqlite::Transaction<'_>,
        current: &Pulsation,
        next_status: PulsationStatus,
        next_stage: PulsationStage,
        event_type: &str,
        message: Option<&str>,
    ) -> Result<Pulsation, PersistenceServiceError> {
        if !Self::is_transition_allowed(current.status, next_status) {
            return Err(PersistenceServiceError::Validation(format!(
                "Invalid transition: {:?} -> {:?}.",
                current.status, next_status
            )));
        }

        let now = Self::now_iso();
        let state_version: u64 = transaction.query_row(
            "UPDATE pulsations
             SET status = ?1, stage = ?2, updated_at = ?3, state_version = state_version + 1
             WHERE id = ?4
             RETURNING state_version",
            params![
                next_status.as_db_value(),
                next_stage.as_db_value(),
                now,
                current.id.as_str()
            ],
            |row| row.get(0),
        )?;
        // ADR-032: base bytes serve the review of a live pulsation only; the
        // hashes and states stay for its report.
        if matches!(next_status, PulsationStatus::Done | PulsationStatus::Failed) {
            transaction.execute(
                "UPDATE pulsation_file_bases SET content=NULL WHERE pulsation_id=?1 AND content IS NOT NULL",
                [current.id.as_str()],
            )?;
            // ADR-034 M3: the same for the REVIEW.md texts captured at Intake.
            transaction.execute(
                "UPDATE pulsation_review_policy_snapshots SET content_json=NULL WHERE pulsation_id=?1 AND content_json IS NOT NULL",
                [current.id.as_str()],
            )?;
            // ADR-036: sealing runs later, from this intent.
            Self::enqueue_attestation_seal(transaction, &current.id)?;
        }

        let event_id = Uuid::new_v4().to_string();
        transaction.execute(
            "INSERT INTO pulse_events (
               id,
               pulsation_id,
               event_type,
               from_status,
               to_status,
               from_stage,
               to_stage,
               message,
               created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                event_id,
                current.id.as_str(),
                event_type,
                current.status.as_db_value(),
                next_status.as_db_value(),
                current.stage.as_db_value(),
                next_stage.as_db_value(),
                message,
                now
            ],
        )?;
        Self::record_pulsation_transition(
            transaction,
            current,
            next_status,
            event_type,
            &event_id,
            state_version,
        )?;

        Ok(Pulsation {
            terminal_reason: current.terminal_reason.clone(),
            id: current.id.clone(),
            project_id: current.project_id.clone(),
            feature_name: current.feature_name.clone(),
            stage: next_stage,
            status: next_status,
            created_at: current.created_at.clone(),
            updated_at: now,
        })
    }

    fn enqueue_stage_job(
        transaction: &rusqlite::Transaction<'_>,
        pulsation_id: &str,
        stage: PulsationStage,
    ) -> Result<String, PersistenceServiceError> {
        let job_id = Uuid::new_v4().to_string();
        let now = Self::now_iso();
        let kind = format!("run_stage::{}", stage.as_db_value());

        transaction.execute(
            "INSERT INTO jobs (
               id,
               pulsation_id,
               kind,
               status,
               attempts,
               created_at,
               updated_at, project_id, input_revision, state_version, idempotency_key, generation
             ) SELECT ?1, ?2, ?3, ?4, 0, ?5, ?5, project_id, input_revision, state_version,
               id || ':' || input_revision || ':' || state_version || ':' || ?3,
               (SELECT COALESCE(MAX(generation),0) FROM jobs prior WHERE prior.pulsation_id=?2 AND prior.kind=?3) FROM pulsations WHERE id=?2",
            params![
                job_id,
                pulsation_id,
                kind,
                JobStatus::Pending.as_db_value(),
                now
            ],
        )?;

        Ok(job_id)
    }

    fn checkpoint_for_stage_exists(
        connection: &Connection,
        pulsation_id: &str,
        stage: PulsationStage,
    ) -> Result<bool, PersistenceServiceError> {
        let mut statement = connection.prepare(
            "SELECT COUNT(*)
             FROM checkpoints
             WHERE pulsation_id = ?1 AND stage = ?2 AND status = 'waiting'",
        )?;
        let count: i64 =
            statement.query_row(params![pulsation_id, stage.as_db_value()], |row| row.get(0))?;
        Ok(count > 0)
    }

    fn open_checkpoint(
        connection: &Connection,
        pulsation_id: &str,
        stage: PulsationStage,
    ) -> Result<(), PersistenceServiceError> {
        if Self::checkpoint_for_stage_exists(connection, pulsation_id, stage)? {
            return Ok(());
        }

        let checkpoint_id = Uuid::new_v4().to_string();
        let now = Self::now_iso();
        connection.execute(
            "INSERT INTO checkpoints (
               id,
               pulsation_id,
               stage,
               status,
               created_at,
               updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            params![
                checkpoint_id,
                pulsation_id,
                stage.as_db_value(),
                CheckpointStatus::Waiting.as_db_value(),
                now
            ],
        )?;

        Ok(())
    }

    fn find_project_by_id(
        connection: &Connection,
        project_id: &str,
        include_archived: bool,
    ) -> Result<Option<Project>, PersistenceServiceError> {
        let mut statement = if include_archived {
            connection.prepare(
                "SELECT id, name, workspace_path, is_archived, created_at, updated_at, last_opened_at
                 FROM projects
                 WHERE id = ?1",
            )?
        } else {
            connection.prepare(
                "SELECT id, name, workspace_path, is_archived, created_at, updated_at, last_opened_at
                 FROM projects
                 WHERE id = ?1 AND is_archived = 0",
            )?
        };

        let project = statement
            .query_row(params![project_id], Self::map_project)
            .optional()?;
        Ok(project)
    }

    fn find_project_by_workspace_path(
        connection: &Connection,
        workspace_path: &str,
    ) -> Result<Option<Project>, PersistenceServiceError> {
        let mut statement = connection.prepare(
            "SELECT id, name, workspace_path, is_archived, created_at, updated_at, last_opened_at
             FROM projects
             WHERE workspace_path = ?1
             LIMIT 1",
        )?;

        let project = statement
            .query_row(params![workspace_path], Self::map_project)
            .optional()?;
        Ok(project)
    }

    pub fn create_project(
        &self,
        request: CreateProjectRequest,
    ) -> Result<CreateProjectResponse, PersistenceServiceError> {
        let workspace_path = Self::normalize_workspace_path(&request.workspace_path)?;
        let now = Self::now_iso();
        let project_id = Uuid::new_v4().to_string();
        let project_name = request
            .name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| Self::derive_project_name(&workspace_path));

        let connection = self.connection()?;
        let insert_result = connection.execute(
            "INSERT INTO projects (
                id,
                name,
                workspace_path,
                is_archived,
                created_at,
                updated_at,
                last_opened_at
             ) VALUES (?1, ?2, ?3, 0, ?4, ?4, ?4)",
            params![project_id, project_name, workspace_path, now],
        );

        if let Err(error) = insert_result {
            if is_unique_constraint(&error) {
                return Err(PersistenceServiceError::Validation(
                    "A project already exists for this workspace.".to_string(),
                ));
            }
            return Err(PersistenceServiceError::Db(error));
        }

        let project =
            Self::find_project_by_id(&connection, &project_id, true)?.ok_or_else(|| {
                PersistenceServiceError::NotFound("The new project was not found.".to_string())
            })?;

        Ok(CreateProjectResponse { project })
    }

    pub fn list_projects(
        &self,
        request: ListProjectsRequest,
    ) -> Result<ListProjectsResponse, PersistenceServiceError> {
        let connection = self.connection()?;
        let include_archived = if request.include_archived {
            1_i64
        } else {
            0_i64
        };
        let mut statement = connection.prepare(
            "SELECT id, name, workspace_path, is_archived, created_at, updated_at, last_opened_at
             FROM projects
             WHERE (?1 = 1 OR is_archived = 0)
             ORDER BY
               CASE WHEN last_opened_at IS NULL THEN 1 ELSE 0 END ASC,
               last_opened_at DESC,
               updated_at DESC",
        )?;

        let rows = statement.query_map(params![include_archived], Self::map_project)?;
        let mut projects = Vec::new();
        for row in rows {
            projects.push(row?);
        }

        Ok(ListProjectsResponse { projects })
    }

    pub fn open_project(
        &self,
        request: OpenProjectRequest,
    ) -> Result<OpenProjectResponse, PersistenceServiceError> {
        let project_id = Self::normalize_project_id(&request.project_id)?;
        let now = Self::now_iso();
        let connection = self.connection()?;

        let updated_rows = connection.execute(
            "UPDATE projects
             SET last_opened_at = ?1, updated_at = ?1
             WHERE id = ?2 AND is_archived = 0",
            params![now, project_id],
        )?;

        if updated_rows == 0 {
            return Err(PersistenceServiceError::NotFound(
                "Project not found or archived.".to_string(),
            ));
        }

        let project =
            Self::find_project_by_id(&connection, &project_id, false)?.ok_or_else(|| {
                PersistenceServiceError::NotFound("Project not found after the update.".to_string())
            })?;

        Ok(OpenProjectResponse { project })
    }

    pub fn archive_project(
        &self,
        request: ArchiveProjectRequest,
    ) -> Result<ArchiveProjectResponse, PersistenceServiceError> {
        let project_id = Self::normalize_project_id(&request.project_id)?;
        let now = Self::now_iso();
        let connection = self.connection()?;

        let updated_rows = connection.execute(
            "UPDATE projects
             SET is_archived = 1, updated_at = ?1
             WHERE id = ?2 AND is_archived = 0",
            params![now, project_id],
        )?;

        if updated_rows == 0 {
            return Err(PersistenceServiceError::NotFound(
                "Project to archive not found.".to_string(),
            ));
        }

        let project = Self::find_project_by_id(&connection, &request.project_id, true)?
            .ok_or_else(|| {
                PersistenceServiceError::NotFound("Archived project not found.".to_string())
            })?;

        Ok(ArchiveProjectResponse { project })
    }

    pub fn ensure_project_for_workspace(
        &self,
        request: EnsureProjectForWorkspaceRequest,
    ) -> Result<EnsureProjectForWorkspaceResponse, PersistenceServiceError> {
        let workspace_path = Self::normalize_workspace_path(&request.workspace_path)?;
        let connection = self.connection()?;
        let now = Self::now_iso();

        if let Some(existing) = Self::find_project_by_workspace_path(&connection, &workspace_path)?
        {
            let deleting: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM project_deletion_journal WHERE project_id=?1)",
                [&existing.id],
                |row| row.get(0),
            )?;
            if deleting {
                return Err(PersistenceServiceError::Validation(
                    "This project is being permanently deleted. Wait for cleanup or restart the app.".into(),
                ));
            }
            connection.execute(
                "UPDATE projects
                 SET is_archived = 0, last_opened_at = ?1, updated_at = ?1
                 WHERE id = ?2",
                params![now, existing.id],
            )?;

            let project =
                Self::find_project_by_id(&connection, &existing.id, true)?.ok_or_else(|| {
                    PersistenceServiceError::NotFound(
                        "The existing project was not found.".to_string(),
                    )
                })?;

            return Ok(EnsureProjectForWorkspaceResponse { project });
        }

        let project_name = request
            .preferred_name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| Self::derive_project_name(&workspace_path));

        let created = self.create_project(CreateProjectRequest {
            workspace_path: workspace_path.clone(),
            name: Some(project_name),
        })?;
        let opened = self.open_project(OpenProjectRequest {
            project_id: created.project.id.clone(),
        })?;

        Ok(EnsureProjectForWorkspaceResponse {
            project: opened.project,
        })
    }

    pub fn get_brownfield_onboarding(
        &self,
        project_id: &str,
    ) -> Result<BrownfieldOnboardingStatus, PersistenceServiceError> {
        let project_id = Self::normalize_project_id(project_id)?;
        let connection = self.connection()?;
        Self::find_project_by_id(&connection, &project_id, false)?.ok_or_else(|| {
            PersistenceServiceError::NotFound(
                "Active project not found for brownfield onboarding.".to_string(),
            )
        })?;
        let consent =
            match Self::read_runtime_setting(&connection, &brownfield_consent_key(&project_id))? {
                None => BrownfieldConsentState::NotDecided,
                Some(value) if value == "true" => BrownfieldConsentState::Granted,
                Some(value) if value == "false" => BrownfieldConsentState::Declined,
                Some(_) => {
                    return Err(PersistenceServiceError::Validation(
                        "The stored brownfield consent has an invalid value.".to_string(),
                    ));
                }
            };
        let ignored_paths = Self::ignored_brownfield_paths(&connection, &project_id)?;
        let coverage = (consent == BrownfieldConsentState::Granted)
            .then(|| Self::brownfield_coverage_metrics(&connection, &project_id))
            .transpose()?;
        Ok(BrownfieldOnboardingStatus {
            project_id,
            consent,
            ignored_paths,
            coverage,
        })
    }

    pub fn set_brownfield_onboarding_consent(
        &self,
        project_id: &str,
        consent: bool,
    ) -> Result<BrownfieldOnboardingStatus, PersistenceServiceError> {
        let project_id = Self::normalize_project_id(project_id)?;
        let mut connection = self.connection()?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        Self::find_project_by_id(&transaction, &project_id, false)?.ok_or_else(|| {
            PersistenceServiceError::NotFound(
                "Active project not found for brownfield onboarding.".to_string(),
            )
        })?;
        Self::upsert_runtime_setting(
            &transaction,
            &brownfield_consent_key(&project_id),
            if consent { "true" } else { "false" },
        )?;
        transaction.commit()?;
        if !consent {
            self.cancel_brownfield_mapping(&project_id)?;
        }
        let connection = self.connection()?;
        let ignored_paths = Self::ignored_brownfield_paths(&connection, &project_id)?;
        let coverage = consent
            .then(|| Self::brownfield_coverage_metrics(&connection, &project_id))
            .transpose()?;
        Ok(BrownfieldOnboardingStatus {
            ignored_paths,
            project_id,
            consent: if consent {
                BrownfieldConsentState::Granted
            } else {
                BrownfieldConsentState::Declined
            },
            coverage,
        })
    }

    pub(crate) fn active_project_by_id(
        &self,
        project_id: &str,
    ) -> Result<Project, PersistenceServiceError> {
        let project_id = Self::normalize_project_id(project_id)?;
        let connection = self.connection()?;
        Self::find_project_by_id(&connection, &project_id, false)?.ok_or_else(|| {
            PersistenceServiceError::NotFound("Active project not found.".to_string())
        })
    }

    pub fn get_last_active_project(
        &self,
    ) -> Result<GetLastActiveProjectResponse, PersistenceServiceError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, name, workspace_path, is_archived, created_at, updated_at, last_opened_at
             FROM projects
             WHERE is_archived = 0
             ORDER BY
               CASE WHEN last_opened_at IS NULL THEN 1 ELSE 0 END ASC,
               last_opened_at DESC,
               updated_at DESC
             LIMIT 1",
        )?;

        let project = statement.query_row([], Self::map_project).optional()?;
        Ok(GetLastActiveProjectResponse { project })
    }

    pub fn load_ui_session(
        &self,
        request: LoadUiSessionRequest,
    ) -> Result<LoadUiSessionResponse, PersistenceServiceError> {
        let project_id = Self::normalize_project_id(&request.project_id)?;
        let connection = self.connection()?;

        let mut statement = connection.prepare(
            "SELECT project_id, workspace_root, active_tab_path, workspace_view_mode,
                    active_view, sidebar_collapsed, context_drawer_open,
                    active_pulsation_id, selected_vault_path, active_chat_conversation_id,
                    scroll_top, auto_open_review, updated_at
             FROM ui_sessions
             WHERE project_id = ?1",
        )?;

        let session_row = statement
            .query_row(params![project_id], |row| {
                Ok(UiSessionState {
                    project_id: row.get("project_id")?,
                    workspace_root: row.get("workspace_root")?,
                    active_tab_path: row.get("active_tab_path")?,
                    workspace_view_mode: row.get("workspace_view_mode")?,
                    active_view: row.get("active_view")?,
                    sidebar_collapsed: row.get::<_, i64>("sidebar_collapsed")? != 0,
                    context_drawer_open: row.get::<_, i64>("context_drawer_open")? != 0,
                    active_pulsation_id: row.get("active_pulsation_id")?,
                    selected_vault_path: row.get("selected_vault_path")?,
                    active_chat_conversation_id: row.get("active_chat_conversation_id")?,
                    scroll_top: row.get("scroll_top")?,
                    tabs: Vec::new(),
                    auto_open_review: row.get::<_, i64>("auto_open_review")? != 0,
                    updated_at: row.get("updated_at")?,
                })
            })
            .optional()?;

        let Some(mut session) = session_row else {
            return Ok(LoadUiSessionResponse { session: None });
        };
        session.workspace_view_mode =
            Self::normalize_workspace_view_mode(&session.workspace_view_mode)?;
        session.active_view = Self::normalize_active_view(&session.active_view)?;

        let mut tabs_statement = connection.prepare(
            "SELECT relative_path, draft_content, base_revision, cursor_line, cursor_column, scroll_top
             FROM ui_session_tabs
             WHERE project_id = ?1
             ORDER BY tab_order ASC",
        )?;

        let rows = tabs_statement.query_map(params![session.project_id], |row| {
            let cursor_line: Option<i64> = row.get("cursor_line")?;
            let cursor_column: Option<i64> = row.get("cursor_column")?;

            Ok(UiTabSnapshot {
                relative_path: row.get("relative_path")?,
                draft_content: row.get("draft_content")?,
                base_revision: row.get("base_revision")?,
                cursor_line: cursor_line.map(|value| value.max(1) as usize),
                cursor_column: cursor_column.map(|value| value.max(1) as usize),
                scroll_top: row.get("scroll_top")?,
            })
        })?;

        let mut tabs = Vec::new();
        for row in rows {
            tabs.push(row?);
        }
        session.tabs = tabs;

        Ok(LoadUiSessionResponse {
            session: Some(session),
        })
    }

    pub fn save_ui_session(
        &self,
        request: SaveUiSessionRequest,
    ) -> Result<SaveUiSessionResponse, PersistenceServiceError> {
        let project_id = Self::normalize_project_id(&request.project_id)?;
        let workspace_root = Self::normalize_workspace_path(&request.workspace_root)?;
        let workspace_view_mode =
            Self::normalize_workspace_view_mode(&request.workspace_view_mode)?;
        let active_view = Self::normalize_active_view(&request.active_view)?;
        let updated_at = Self::now_iso();
        let connection = self.connection()?;
        let active_tab_path = request.active_tab_path.clone();
        let active_pulsation_id = request.active_pulsation_id.clone();
        let selected_vault_path = request.selected_vault_path.clone();
        let active_chat_conversation_id = request.active_chat_conversation_id.clone();
        let scroll_top = request.scroll_top;
        let tabs = request.tabs;

        if Self::find_project_by_id(&connection, &project_id, false)?.is_none() {
            return Err(PersistenceServiceError::NotFound(
                "Project not found to store the UI session.".to_string(),
            ));
        }

        let transaction = connection.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO ui_sessions (
               project_id,
               workspace_root,
               active_tab_path,
               workspace_view_mode,
               active_view,
               sidebar_collapsed,
               context_drawer_open,
               active_pulsation_id,
               selected_vault_path,
               active_chat_conversation_id,
               scroll_top,
               auto_open_review,
               updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(project_id) DO UPDATE SET
               workspace_root = excluded.workspace_root,
               active_tab_path = excluded.active_tab_path,
               workspace_view_mode = excluded.workspace_view_mode,
               active_view = excluded.active_view,
               sidebar_collapsed = excluded.sidebar_collapsed,
               context_drawer_open = excluded.context_drawer_open,
               active_pulsation_id = excluded.active_pulsation_id,
               selected_vault_path = excluded.selected_vault_path,
               active_chat_conversation_id = excluded.active_chat_conversation_id,
               scroll_top = excluded.scroll_top,
               auto_open_review = excluded.auto_open_review,
               updated_at = excluded.updated_at",
            params![
                project_id.as_str(),
                workspace_root,
                active_tab_path,
                workspace_view_mode,
                active_view,
                i64::from(request.sidebar_collapsed),
                i64::from(request.context_drawer_open),
                active_pulsation_id,
                selected_vault_path,
                active_chat_conversation_id,
                scroll_top,
                i64::from(request.auto_open_review),
                updated_at
            ],
        )?;

        transaction.execute(
            "DELETE FROM ui_session_tabs WHERE project_id = ?1",
            params![project_id.as_str()],
        )?;

        for (index, tab) in tabs.into_iter().enumerate() {
            if tab.relative_path.trim().is_empty() {
                continue;
            }

            transaction.execute(
                "INSERT INTO ui_session_tabs (
                   project_id,
                   tab_order,
                   relative_path,
                   draft_content,
                   base_revision,
                   cursor_line,
                   cursor_column,
                   scroll_top,
                   created_at,
                   updated_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
                params![
                    project_id.as_str(),
                    index as i64,
                    tab.relative_path,
                    tab.draft_content,
                    tab.base_revision,
                    tab.cursor_line.map(|value| value as i64),
                    tab.cursor_column.map(|value| value as i64),
                    tab.scroll_top,
                    updated_at
                ],
            )?;
        }

        transaction.commit()?;
        Ok(SaveUiSessionResponse { updated_at })
    }

    pub fn create_pulsation(
        &self,
        request: CreatePulsationRequest,
    ) -> Result<CreatePulsationResponse, PersistenceServiceError> {
        let project_id = Self::normalize_project_id(&request.project_id)?;
        let feature_name = Self::normalize_feature_name(&request.feature_name)?;
        let connection = self.connection()?;

        if Self::find_project_by_id(&connection, &project_id, false)?.is_none() {
            return Err(PersistenceServiceError::NotFound(
                "Project not found to create the pulsation.".to_string(),
            ));
        }

        let now = Self::now_iso();
        let pulsation_id = Uuid::new_v4().to_string();
        let transaction = connection.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO pulsations (
               id,
               project_id,
               feature_name,
               stage,
               status,
               created_at,
               updated_at, queue_sequence, pipeline_version
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6,
               (SELECT COALESCE(MAX(queue_sequence),0)+1 FROM pulsations WHERE project_id=?2), ?7)",
            params![
                pulsation_id.as_str(),
                project_id.as_str(),
                feature_name.as_str(),
                PulsationStage::Intake.as_db_value(),
                PulsationStatus::Idle.as_db_value(),
                now,
                crate::orchestration::PipelineVersion::FREE_FORM.as_u8()
            ],
        )?;

        let event_id = Uuid::new_v4().to_string();
        transaction.execute(
            "INSERT INTO pulse_events (
               id,
               pulsation_id,
               event_type,
               to_status,
               to_stage,
               message,
               created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                event_id,
                pulsation_id.as_str(),
                "pulse_created",
                PulsationStatus::Idle.as_db_value(),
                PulsationStage::Intake.as_db_value(),
                "Pulsation created.",
                now
            ],
        )?;

        let mut current = Pulsation {
            terminal_reason: None,
            id: pulsation_id.clone(),
            project_id: project_id.clone(),
            feature_name: feature_name.clone(),
            stage: PulsationStage::Intake,
            status: PulsationStatus::Idle,
            created_at: now.clone(),
            updated_at: now,
        };

        current = Self::transition_pulsation(
            &transaction,
            &current,
            PulsationStatus::Planning,
            PulsationStage::Intake,
            "pulse_queued",
            Some("Pulsation queued."),
        )?;

        Self::enqueue_stage_job(&transaction, &current.id, current.stage)?;
        Self::assess_description(&transaction, &current.id, &feature_name, 1)?;
        transaction.commit()?;

        self.wake_scheduler();
        Ok(CreatePulsationResponse { pulsation: current })
    }

    pub fn list_pulsations(
        &self,
        request: ListPulsationsRequest,
    ) -> Result<ListPulsationsResponse, PersistenceServiceError> {
        let project_id = Self::normalize_project_id(&request.project_id)?;
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, project_id, feature_name, stage, status, created_at, updated_at, terminal_reason
             FROM pulsations
             WHERE project_id = ?1
             ORDER BY queue_sequence ASC",
        )?;
        let rows = statement.query_map(params![project_id], Self::map_pulsation)?;

        let mut pulsations = Vec::new();
        for row in rows {
            pulsations.push(row?);
        }

        Ok(ListPulsationsResponse { pulsations })
    }

    pub fn list_cli_fallbacks(
        &self,
        project_id: &str,
    ) -> Result<Vec<CliFallbackCheckpoint>, PersistenceServiceError> {
        let project_id = Self::normalize_project_id(project_id)?;
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT c.id,c.pulsation_id,p.project_id,p.feature_name,f.agent_role,
                    f.terminal_run_id,f.error_code,f.diagnostic,p.state_version,c.stage,c.created_at
             FROM cli_fallback_requests f
             JOIN checkpoints c ON c.id=f.checkpoint_id
             JOIN pulsations p ON p.id=f.pulsation_id
             WHERE p.project_id=?1 AND p.status='checkpoint' AND c.status='waiting'
               AND c.purpose='cli_fallback' AND f.decision IS NULL
             ORDER BY c.created_at ASC",
        )?;
        let rows = statement.query_map([project_id], |row| {
            let role: String = row.get(4)?;
            let stage: String = row.get(9)?;
            Ok(CliFallbackCheckpoint {
                checkpoint_id: row.get(0)?,
                pulsation_id: row.get(1)?,
                project_id: row.get(2)?,
                feature_name: row.get(3)?,
                agent_role: AgentRole::parse(&role)
                    .ok_or_else(|| Self::invalid_enum_error("agent_role", &role))?,
                terminal_run_id: row.get(5)?,
                error_code: row.get(6)?,
                diagnostic: row.get(7)?,
                state_version: row.get(8)?,
                stage: PulsationStage::from_db_value(&stage)
                    .ok_or_else(|| Self::invalid_enum_error("stage", &stage))?,
                created_at: row.get(10)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(PersistenceServiceError::from)
    }

    pub fn resolve_cli_fallback(
        &self,
        request: ResolveCliFallbackRequest,
    ) -> Result<ResolveCliFallbackResponse, PersistenceServiceError> {
        let checkpoint_id = request.checkpoint_id.trim();
        if checkpoint_id.is_empty() {
            return Err(PersistenceServiceError::Validation(
                "checkpointId cannot be empty.".to_string(),
            ));
        }
        let mut connection = self.connection()?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let fallback = transaction
            .query_row(
                "SELECT c.pulsation_id,c.stage,c.producer_job_id,f.agent_role,f.terminal_run_id
                 FROM cli_fallback_requests f
                 JOIN checkpoints c ON c.id=f.checkpoint_id
                 JOIN pulsations p ON p.id=f.pulsation_id
                 WHERE c.id=?1 AND c.purpose='cli_fallback' AND c.status='waiting'
                   AND f.decision IS NULL AND p.status='checkpoint'
                   AND c.state_version=?2 AND p.state_version=?2
                   AND c.input_revision=p.input_revision",
                params![checkpoint_id, request.expected_state_version],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((pulsation_id, stage_raw, producer_job_id, role_raw, terminal_run_id)) = fallback
        else {
            return Err(PersistenceServiceError::Validation(
                "The fallback checkpoint is missing or stale. Refresh before deciding.".to_string(),
            ));
        };
        let waiting: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM checkpoints WHERE pulsation_id=?1 AND status='waiting'",
            [&pulsation_id],
            |row| row.get(0),
        )?;
        if waiting != 1 {
            return Err(PersistenceServiceError::Validation(
                "The fallback checkpoint is ambiguous. Refresh before deciding.".to_string(),
            ));
        }
        let stage = PulsationStage::from_db_value(&stage_raw).ok_or_else(|| {
            PersistenceServiceError::Runtime("Invalid stage in the CLI checkpoint.".to_string())
        })?;
        let role = AgentRole::parse(&role_raw).ok_or_else(|| {
            PersistenceServiceError::Runtime("Invalid agent in the CLI checkpoint.".to_string())
        })?;
        let current =
            Self::find_pulsation_by_id(&transaction, &pulsation_id)?.ok_or_else(|| {
                PersistenceServiceError::NotFound(
                    "The fallback's pulsation was not found.".to_string(),
                )
            })?;
        if current.stage != stage {
            return Err(PersistenceServiceError::Validation(
                "The pulsation stage changed; refresh the checkpoint.".to_string(),
            ));
        }
        let now = Self::now_iso();
        let checkpoint_status = if request.decision == CliFallbackDecision::Cancel {
            CheckpointStatus::Rejected
        } else {
            CheckpointStatus::Approved
        };
        transaction.execute(
            "UPDATE checkpoints SET status=?2,updated_at=?3 WHERE id=?1",
            params![checkpoint_id, checkpoint_status.as_db_value(), now],
        )?;
        transaction.execute(
            "UPDATE cli_fallback_requests SET decision=?2,resolved_at=?3,updated_at=?3
             WHERE checkpoint_id=?1",
            params![checkpoint_id, request.decision.as_db_value(), now],
        )?;

        let mut cancellation_ids = Vec::new();
        let result = match request.decision {
            CliFallbackDecision::Cancel => {
                cancellation_ids = transaction
                    .prepare(
                        "SELECT id FROM jobs WHERE pulsation_id=?1
                         AND status IN ('pending','running','interrupted')",
                    )?
                    .query_map([&pulsation_id], |row| row.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                transaction.execute(
                    "UPDATE jobs SET status='failed',cancellation_reason='user_cancel',
                       cancel_requested_at=?2,finished_at=?2,updated_at=?2
                     WHERE pulsation_id=?1 AND status IN ('pending','running','interrupted')",
                    params![pulsation_id, now],
                )?;
                let mut updated = Self::transition_pulsation(
                    &transaction,
                    &current,
                    PulsationStatus::Failed,
                    stage,
                    "cli_fallback_cancelled",
                    Some("Pulsation cancelled at the CLI fallback decision."),
                )?;
                transaction.execute(
                    "UPDATE pulsations SET terminal_reason='user_cancel' WHERE id=?1",
                    [&pulsation_id],
                )?;
                updated.terminal_reason = Some("user_cancel".to_string());
                transaction.execute(
                    "UPDATE agent_runs SET status='interrupted',updated_at=?2
                     WHERE job_id IN (SELECT id FROM jobs WHERE pulsation_id=?1)
                       AND status IN ('running','awaiting_fallback')",
                    params![pulsation_id, now],
                )?;
                Self::mark_effects_for_review(
                    &transaction,
                    &pulsation_id,
                    "cli_fallback_cancelled_with_effects",
                )?;
                transaction.execute(
                    "UPDATE artifact_publications SET state='cancelled'
                     WHERE pulsation_id=?1 AND state IN ('pending','applying')",
                    [&pulsation_id],
                )?;
                Self::enqueue_learning(&transaction, &pulsation_id)?;
                updated
            }
            CliFallbackDecision::UseApi | CliFallbackDecision::RetryCli => {
                if request.decision == CliFallbackDecision::UseApi {
                    transaction.execute(
                        "INSERT INTO pulse_execution_overrides(
                           pulsation_id,agent_role,execution_mode,source_checkpoint_id,created_at
                         ) VALUES(?1,?2,'api_direct',?3,?4)
                         ON CONFLICT(pulsation_id,agent_role) DO UPDATE SET
                           execution_mode='api_direct',
                           -- The ladder may have already elected a CLI backend
                           -- here; an api_direct row must carry none.
                           backend=NULL,
                           source_checkpoint_id=excluded.source_checkpoint_id,
                           created_at=excluded.created_at",
                        params![pulsation_id, role.as_str(), checkpoint_id, now],
                    )?;
                }
                let updated = Self::transition_pulsation(
                    &transaction,
                    &current,
                    PulsationStatus::Planning,
                    stage,
                    if request.decision == CliFallbackDecision::UseApi {
                        "cli_fallback_api_authorized"
                    } else {
                        "cli_retry_requested"
                    },
                    Some(if request.decision == CliFallbackDecision::UseApi {
                        "API use authorized for this agent in this pulsation."
                    } else {
                        "CLI retry requested by the user."
                    }),
                )?;
                let producer_job_id = producer_job_id.as_deref().ok_or_else(|| {
                    PersistenceServiceError::Runtime(
                        "Fallback checkpoint without a producing job.".to_string(),
                    )
                })?;
                let changed = transaction.execute(
                    "UPDATE jobs SET status='pending',last_error=NULL,started_at=NULL,finished_at=NULL,
                       cancel_requested_at=NULL,cancellation_reason=NULL,available_at=?2,updated_at=?2,
                       state_version=(SELECT state_version FROM pulsations WHERE id=?3),
                       generation=generation+1
                     WHERE id=?1 AND pulsation_id=?3 AND status='interrupted'",
                    params![producer_job_id, now, pulsation_id],
                )?;
                if changed != 1 {
                    return Err(PersistenceServiceError::Validation(
                        "The fallback job is no longer available to resume.".to_string(),
                    ));
                }
                transaction.execute(
                    "UPDATE agent_runs SET status='failed',updated_at=?2
                     WHERE job_id=?1 AND status='awaiting_fallback'",
                    params![producer_job_id, now],
                )?;
                updated
            }
        };
        if let Some(run_id) = terminal_run_id.as_deref() {
            let status = if request.decision == CliFallbackDecision::Cancel {
                "cancelled"
            } else {
                "failed"
            };
            let changed = transaction.execute(
                "UPDATE terminal_runs SET status=?2,updated_at=?3,finished_at=COALESCE(finished_at,?3)
                 WHERE id=?1 AND status='awaiting_fallback'",
                params![run_id, status, now],
            )?;
            if changed == 1 {
                Self::record_terminal_run_status(&transaction, run_id, status)?;
            }
        }
        // ADR-035 M1: the approved API fallback is a transport amendment of
        // the mandate in force; before the Specification there is none, and
        // the issued mandate then records the override as resolved.
        if request.decision == CliFallbackDecision::UseApi {
            Self::amend_mandate_transport(
                &transaction,
                &pulsation_id,
                role,
                checkpoint_id,
                stage,
                request.expected_state_version,
            )?;
        }
        Self::record_approval_receipt(
            &transaction,
            mandate_persistence::ReceiptInput {
                checkpoint_id,
                pulsation_id: &pulsation_id,
                purpose: "cli_fallback",
                stage: stage.as_db_value(),
                decision: request.decision.as_db_value(),
                state_version: request.expected_state_version,
                reviewed: Vec::new(),
                confirmed_paths: Vec::new(),
                feedback: None,
            },
        )?;
        transaction.commit()?;
        for job_id in cancellation_ids {
            self.request_job_cancellation(&job_id);
        }
        self.wake_scheduler();
        Ok(ResolveCliFallbackResponse { pulsation: result })
    }

    /// The model output rejected on the last attempt of this checkpoint's job.
    ///
    /// Follows the discipline of `read_hitl_review`: never fabricate. A checkpoint
    /// whose job persisted nothing returns `None` fields rather than a plausible
    /// reconstruction, and the UI says so.
    /// The project's held runtime slots, main first. A slot without a job row reports its
    /// token prefix as `kind` so the UI can still name the work.
    fn active_runtime_jobs(
        connection: &Connection,
        project_id: &str,
    ) -> Result<Vec<ActiveRuntimeJob>, PersistenceServiceError> {
        let mut statement = connection.prepare(
            "SELECT s.job_class,j.kind,s.token,j.pulsation_id,p.feature_name,s.acquired_at
             FROM runtime_slots s
             LEFT JOIN jobs j ON j.id=s.job_id
             LEFT JOIN pulsations p ON p.id=j.pulsation_id
             WHERE s.project_id=?1
             ORDER BY CASE s.job_class WHEN 'main' THEN 0 ELSE 1 END,s.acquired_at",
        )?;
        let jobs = statement
            .query_map([project_id], |row| {
                let kind: Option<String> = row.get(1)?;
                let token: String = row.get(2)?;
                Ok(ActiveRuntimeJob {
                    job_class: row.get(0)?,
                    kind: kind
                        .unwrap_or_else(|| token.split(':').next().unwrap_or_default().to_string()),
                    pulsation_id: row.get(3)?,
                    feature_name: row.get(4)?,
                    acquired_at: row.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(jobs)
    }

    /// Waiting stage-failure checkpoints for a project, newest last.
    ///
    /// `attempts` is read live from the job so the panel can say how much of the
    /// automatic budget was already spent before the human was asked.
    fn stage_failure_checkpoints(
        connection: &Connection,
        project_id: &str,
    ) -> Result<Vec<StageFailureCheckpoint>, PersistenceServiceError> {
        let mut statement = connection.prepare(
            "SELECT c.id,c.pulsation_id,p.project_id,p.feature_name,f.stage,f.failure_class,
                    f.error_code,f.diagnostic,f.allow_retry,f.allow_revision,f.revision_stage,
                    COALESCE(j.attempts,0),p.state_version,c.created_at,
                    f.allowed_revision_targets_json,f.remaining_budget_json,f.stage_execution_id
             FROM stage_failure_requests f
             JOIN checkpoints c ON c.id=f.checkpoint_id
             JOIN pulsations p ON p.id=f.pulsation_id
             LEFT JOIN jobs j ON j.id=f.job_id
             WHERE p.project_id=?1 AND p.status='checkpoint' AND c.status='waiting'
               AND c.purpose='stage_failure' AND f.decision IS NULL
             ORDER BY c.created_at ASC",
        )?;
        let rows = statement.query_map([project_id], |row| {
            let stage: String = row.get(4)?;
            let revision_stage: Option<String> = row.get(10)?;
            let targets_json: Option<String> = row.get(14)?;
            let remaining_json: Option<String> = row.get(15)?;
            let allowed_revision_targets = targets_json
                .as_deref()
                .and_then(|json| serde_json::from_str::<Vec<String>>(json).ok())
                .map(|targets| {
                    targets
                        .iter()
                        .filter_map(|raw| PulsationStage::from_db_value(raw))
                        .collect()
                })
                .unwrap_or_default();
            let remaining_extra_invocations = remaining_json
                .as_deref()
                .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
                .and_then(|value| value["extraInvocations"].as_u64())
                .map(|value| value as u32);
            Ok(StageFailureCheckpoint {
                checkpoint_id: row.get(0)?,
                pulsation_id: row.get(1)?,
                project_id: row.get(2)?,
                feature_name: row.get(3)?,
                stage: PulsationStage::from_db_value(&stage)
                    .ok_or_else(|| Self::invalid_enum_error("stage", &stage))?,
                failure_class: row.get(5)?,
                error_code: row.get(6)?,
                diagnostic: row.get(7)?,
                allow_retry: row.get::<_, i64>(8)? != 0,
                allow_revision: row.get::<_, i64>(9)? != 0,
                revision_stage: revision_stage
                    .as_deref()
                    .and_then(PulsationStage::from_db_value),
                attempts: row.get(11)?,
                state_version: row.get(12)?,
                created_at: row.get(13)?,
                allowed_revision_targets,
                remaining_extra_invocations,
                stage_execution_id: row.get(16)?,
                scope_request: None,
            })
        })?;
        let mut checkpoints = rows.collect::<Result<Vec<_>, _>>()?;
        for checkpoint in &mut checkpoints {
            checkpoint.scope_request =
                Self::scope_request_view(connection, &checkpoint.checkpoint_id)?;
        }
        Ok(checkpoints)
    }

    pub fn read_stage_failure_detail(
        &self,
        checkpoint_id: &str,
    ) -> Result<StageFailureDetail, PersistenceServiceError> {
        let checkpoint_id = checkpoint_id.trim();
        if checkpoint_id.is_empty() {
            return Err(PersistenceServiceError::Validation(
                "checkpointId cannot be empty.".to_string(),
            ));
        }
        let connection = self.connection()?;
        let job_id: String = connection
            .query_row(
                "SELECT f.job_id FROM stage_failure_requests f
                 JOIN checkpoints c ON c.id=f.checkpoint_id
                 WHERE c.id=?1 AND c.purpose='stage_failure' AND c.status='waiting'",
                [checkpoint_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| {
                PersistenceServiceError::NotFound(
                    "The failure checkpoint is missing or already resolved.".to_string(),
                )
            })?;
        // Same row the corrective retry reads back into `PreviousAttempt`.
        let attempt: Option<(Option<String>, Option<String>)> = connection
            .query_row(
                "SELECT raw_output,validation_error FROM agent_run_outputs
                 WHERE job_id=?1 AND validation_error IS NOT NULL
                 ORDER BY created_at DESC LIMIT 1",
                [&job_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (raw_output, validation_error) = attempt.unwrap_or((None, None));
        Ok(StageFailureDetail {
            checkpoint_id: checkpoint_id.to_string(),
            raw_output: raw_output.map(|text| sanitized_observation_text(&text)),
            validation_error: validation_error.map(|text| sanitized_observation_text(&text)),
        })
    }

    pub fn resolve_stage_failure(
        &self,
        request: ResolveStageFailureRequest,
    ) -> Result<ResolveStageFailureResponse, PersistenceServiceError> {
        self.resolve_stage_failure_with_target(request, None)
    }

    /// `revision_target` selects among the targets a composed stage failure
    /// permits (ADR-022); `None` resolves to the checkpoint's primary target.
    /// A target outside the permitted set is refused by the host regardless of
    /// what the UI offered.
    pub fn resolve_stage_failure_with_target(
        &self,
        request: ResolveStageFailureRequest,
        revision_target: Option<PulsationStage>,
    ) -> Result<ResolveStageFailureResponse, PersistenceServiceError> {
        self.resolve_stage_failure_with_convergence(request, revision_target, None)
    }

    /// `ack` is the developer's explicit decision once the pulsation reached
    /// its revision-round threshold (ADR-032); a replan revises from the
    /// Specification instead of the failure's permitted targets.
    pub fn resolve_stage_failure_with_convergence(
        &self,
        request: ResolveStageFailureRequest,
        revision_target: Option<PulsationStage>,
        ack: Option<ConvergenceAck>,
    ) -> Result<ResolveStageFailureResponse, PersistenceServiceError> {
        let checkpoint_id = request.checkpoint_id.trim();
        if checkpoint_id.is_empty() {
            return Err(PersistenceServiceError::Validation(
                "checkpointId cannot be empty.".to_string(),
            ));
        }
        if request
            .feedback
            .as_ref()
            .is_some_and(|text| text.chars().count() > 16_000)
        {
            return Err(PersistenceServiceError::Validation(
                "Feedback excede 16.000 caracteres.".to_string(),
            ));
        }
        let mut connection = self.connection()?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let WaitingFailure {
            pulsation_id,
            stage_raw,
            producer_job_id,
            class_raw,
            allow_retry,
            allow_revision,
            revision_stage_raw,
            error_code,
            stage_execution_id,
            allowed_targets_json,
        } = Self::waiting_stage_failure(
            &transaction,
            checkpoint_id,
            request.expected_state_version,
        )?;
        // The UI hides unavailable buttons; the host must not rely on that.
        let permitted = match request.decision {
            StageFailureDecision::RetryStage => allow_retry,
            StageFailureDecision::RequestRevision => allow_revision,
            StageFailureDecision::Abort => true,
        };
        if !permitted {
            return Err(PersistenceServiceError::Validation(
                "This decision is not offered for this kind of failure.".to_string(),
            ));
        }
        let stage = PulsationStage::from_db_value(&stage_raw).ok_or_else(|| {
            PersistenceServiceError::Runtime("Invalid stage in the failure checkpoint.".to_string())
        })?;
        let class =
            crate::orchestration::FailureClass::from_db_value(&class_raw).ok_or_else(|| {
                PersistenceServiceError::Runtime("Invalid failure class.".to_string())
            })?;
        let current =
            Self::find_pulsation_by_id(&transaction, &pulsation_id)?.ok_or_else(|| {
                PersistenceServiceError::NotFound(
                    "The failure's pulsation was not found.".to_string(),
                )
            })?;
        if current.stage != stage {
            return Err(PersistenceServiceError::Validation(
                "The pulsation stage changed; refresh the checkpoint.".to_string(),
            ));
        }
        // ADR-032: a failure-path revision is a human round too.
        let replan_target = if request.decision == StageFailureDecision::RequestRevision {
            Self::admit_revision_round(
                &transaction,
                &pulsation_id,
                checkpoint_id,
                stage,
                ack.as_ref(),
            )?
        } else {
            None
        };
        let now = Self::now_iso();
        let checkpoint_status = match request.decision {
            StageFailureDecision::Abort => CheckpointStatus::Rejected,
            StageFailureDecision::RequestRevision => CheckpointStatus::RevisionRequested,
            StageFailureDecision::RetryStage => CheckpointStatus::Approved,
        };
        transaction.execute(
            "UPDATE checkpoints SET status=?2,updated_at=?3 WHERE id=?1",
            params![checkpoint_id, checkpoint_status.as_db_value(), now],
        )?;
        transaction.execute(
            "UPDATE stage_failure_requests SET decision=?2,resolved_at=?3,updated_at=?3
             WHERE checkpoint_id=?1",
            params![checkpoint_id, request.decision.as_db_value(), now],
        )?;
        // ADR-035 M4: any other action declines the checkpoint's scope request.
        Self::decline_scope_request(
            &transaction,
            checkpoint_id,
            request.decision.as_db_value(),
            &now,
        )?;

        let mut cancellation_ids = Vec::new();
        let result = match request.decision {
            StageFailureDecision::Abort => {
                cancellation_ids = transaction
                    .prepare(
                        "SELECT id FROM jobs WHERE pulsation_id=?1
                         AND status IN ('pending','running','interrupted')",
                    )?
                    .query_map([&pulsation_id], |row| row.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                transaction.execute(
                    "UPDATE jobs SET status='failed',cancellation_reason='user_cancel',
                       cancel_requested_at=?2,finished_at=?2,updated_at=?2
                     WHERE pulsation_id=?1 AND status IN ('pending','running','interrupted')",
                    params![pulsation_id, now],
                )?;
                let mut updated = Self::transition_pulsation(
                    &transaction,
                    &current,
                    PulsationStatus::Failed,
                    stage,
                    "stage_failure_aborted",
                    Some("Failure closed by the user at the attention checkpoint."),
                )?;
                let terminal_reason = class.terminal_reason();
                transaction.execute(
                    "UPDATE pulsations SET terminal_reason=?2 WHERE id=?1",
                    params![pulsation_id, terminal_reason],
                )?;
                updated.terminal_reason = Some(terminal_reason.to_string());
                transaction.execute(
                    "UPDATE agent_runs SET status='failed',updated_at=?2
                     WHERE job_id IN (SELECT id FROM jobs WHERE pulsation_id=?1)
                       AND status IN ('running','awaiting_decision')",
                    params![pulsation_id, now],
                )?;
                // Deferred from checkpoint-open time so Retry stayed executable.
                Self::mark_effects_for_review(
                    &transaction,
                    &pulsation_id,
                    "stage_failure_aborted_with_effects",
                )?;
                transaction.execute(
                    "UPDATE artifact_publications SET state='cancelled'
                     WHERE pulsation_id=?1 AND state IN ('pending','applying')",
                    [&pulsation_id],
                )?;
                Self::enqueue_learning(&transaction, &pulsation_id)?;
                if stage_execution_id.is_some() {
                    Self::cancel_stage_executions(&transaction, &pulsation_id)?;
                }
                updated
            }
            StageFailureDecision::RetryStage => {
                Self::resume_failed_stage(&transaction, &current, stage, &producer_job_id, &now)?
            }
            StageFailureDecision::RequestRevision => {
                let target_raw = revision_stage_raw.ok_or_else(|| {
                    PersistenceServiceError::Runtime(
                        "The failure checkpoint has no revision target.".to_string(),
                    )
                })?;
                let primary = PulsationStage::from_db_value(&target_raw).ok_or_else(|| {
                    PersistenceServiceError::Runtime("Invalid revision target.".to_string())
                })?;
                let permitted: Vec<PulsationStage> = match &allowed_targets_json {
                    Some(json) => serde_json::from_str::<Vec<String>>(json)
                        .map_err(|e| PersistenceServiceError::Runtime(e.to_string()))?
                        .iter()
                        .filter_map(|raw| PulsationStage::from_db_value(raw))
                        .collect(),
                    None => vec![primary],
                };
                let target = match (replan_target, revision_target) {
                    (Some(replan), _) => replan,
                    (None, Some(requested)) if permitted.contains(&requested) => requested,
                    (None, Some(_)) => {
                        return Err(PersistenceServiceError::Validation(
                            "This revision target is not allowed for this failure.".to_string(),
                        ));
                    }
                    (None, None) => primary,
                };
                if stage_execution_id.is_some() {
                    Self::supersede_stage_executions(&transaction, &pulsation_id, target)?;
                }
                // The producer job is `interrupted`, not `done` as in the HITL path.
                // `complete_integrations` refuses to finalize while any job on the
                // pulsation is pending/running/interrupted, so leaving it behind would
                // deadlock the pipeline at the Integrator with no visible error.
                transaction.execute(
                    "UPDATE jobs SET status='failed',cancellation_reason='superseded',
                       finished_at=?2,updated_at=?2
                     WHERE pulsation_id=?1 AND status IN ('pending','running','interrupted')",
                    params![pulsation_id, now],
                )?;
                transaction.execute(
                    "UPDATE agent_runs SET status='failed',updated_at=?2
                     WHERE job_id IN (SELECT id FROM jobs WHERE pulsation_id=?1)
                       AND status='awaiting_decision'",
                    params![pulsation_id, now],
                )?;
                transaction.execute(
                    "UPDATE pulsations SET integration_checkpoint_id=NULL,
                       integration_state_version=NULL WHERE id=?1",
                    [&pulsation_id],
                )?;
                let target_index = crate::orchestration::catalog::STAGES
                    .iter()
                    .position(|(s, _, _)| *s == target)
                    .expect("stage");
                for (_, stage_value, _) in &crate::orchestration::catalog::STAGES[target_index..] {
                    transaction.execute(
                        "UPDATE artifacts SET acceptance='superseded'
                         WHERE pulsation_id=?1 AND producer_stage=?2",
                        params![pulsation_id, stage_value],
                    )?;
                }
                transaction.execute(
                    "UPDATE artifact_publications SET state='cancelled'
                     WHERE pulsation_id=?1 AND state IN ('pending','applying')
                       AND artifact_id IN (SELECT id FROM artifacts WHERE acceptance='superseded')",
                    [&pulsation_id],
                )?;
                let feedback =
                    Self::stage_failure_revision_feedback(request.feedback.as_deref(), &error_code);
                transaction.execute(
                    "UPDATE pulsations SET revision_feedback=?2 WHERE id=?1",
                    params![pulsation_id, feedback],
                )?;
                let updated = Self::transition_pulsation(
                    &transaction,
                    &current,
                    PulsationStatus::Planning,
                    target,
                    "stage_failure_revision_requested",
                    Some("Revision requested after a failure; later artifacts were superseded."),
                )?;
                Self::enqueue_stage_job(&transaction, &updated.id, target)?;
                updated
            }
        };
        Self::record_approval_receipt(
            &transaction,
            mandate_persistence::ReceiptInput {
                checkpoint_id,
                pulsation_id: &pulsation_id,
                purpose: "stage_failure",
                stage: stage.as_db_value(),
                decision: request.decision.as_db_value(),
                state_version: request.expected_state_version,
                reviewed: Vec::new(),
                confirmed_paths: Vec::new(),
                feedback: request.feedback.as_deref(),
            },
        )?;
        transaction.commit()?;
        for job_id in cancellation_ids {
            self.request_job_cancellation(&job_id);
        }
        self.wake_scheduler();
        Ok(ResolveStageFailureResponse { pulsation: result })
    }

    /// The waiting failure checkpoint a decision settles, fresh against the
    /// state version the developer saw and the only open checkpoint.
    fn waiting_stage_failure(
        transaction: &Connection,
        checkpoint_id: &str,
        expected_state_version: u64,
    ) -> Result<WaitingFailure, PersistenceServiceError> {
        // `Specification` stringifies as SPECIFICATION but legacy rows say Specification.
        let found = transaction
            .query_row(
                "SELECT c.pulsation_id,f.stage,f.job_id,f.failure_class,f.allow_retry,
                        f.allow_revision,f.revision_stage,f.error_code,f.stage_execution_id,
                        f.allowed_revision_targets_json
                 FROM stage_failure_requests f
                 JOIN checkpoints c ON c.id=f.checkpoint_id
                 JOIN pulsations p ON p.id=f.pulsation_id
                 WHERE c.id=?1 AND c.purpose='stage_failure' AND c.status='waiting'
                   AND f.decision IS NULL AND p.status='checkpoint'
                   AND c.state_version=?2 AND p.state_version=?2
                   AND c.input_revision=p.input_revision
                   AND (c.stage=p.stage OR (c.stage='Specification' AND p.stage='SPECIFICATION'))",
                params![checkpoint_id, expected_state_version],
                |row| {
                    Ok(WaitingFailure {
                        pulsation_id: row.get(0)?,
                        stage_raw: row.get(1)?,
                        producer_job_id: row.get(2)?,
                        class_raw: row.get(3)?,
                        allow_retry: row.get::<_, i64>(4)? != 0,
                        allow_revision: row.get::<_, i64>(5)? != 0,
                        revision_stage_raw: row.get(6)?,
                        error_code: row.get(7)?,
                        stage_execution_id: row.get(8)?,
                        allowed_targets_json: row.get(9)?,
                    })
                },
            )
            .optional()?;
        let Some(found) = found else {
            return Err(PersistenceServiceError::Validation(
                "The failure checkpoint is missing or stale. Refresh before deciding.".to_string(),
            ));
        };
        let waiting: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM checkpoints WHERE pulsation_id=?1 AND status='waiting'",
            [&found.pulsation_id],
            |row| row.get(0),
        )?;
        if waiting != 1 {
            return Err(PersistenceServiceError::Validation(
                "The failure checkpoint is ambiguous. Refresh before deciding.".to_string(),
            ));
        }
        Ok(found)
    }

    /// The Retry of a parked stage: the interrupted job runs again under a new
    /// generation, on the same shared budget.
    fn resume_failed_stage(
        transaction: &rusqlite::Transaction<'_>,
        current: &Pulsation,
        stage: PulsationStage,
        producer_job_id: &str,
        now: &str,
    ) -> Result<Pulsation, PersistenceServiceError> {
        // Transition first: the claim requires j.state_version=p.state_version,
        // and the resume UPDATE below reads the post-transition value.
        let updated = Self::transition_pulsation(
            transaction,
            current,
            PulsationStatus::Planning,
            stage,
            "stage_failure_retry",
            Some("Stage retry requested by the user."),
        )?;
        // `attempts` is intentionally NOT reset: the automatic budget stays
        // spent, so one click buys exactly one agent call and a fresh failure
        // reopens this checkpoint instead of looping.
        let changed = transaction.execute(
            "UPDATE jobs SET status='pending',last_error=NULL,started_at=NULL,finished_at=NULL,
               cancel_requested_at=NULL,cancellation_reason=NULL,available_at=?2,updated_at=?2,
               state_version=(SELECT state_version FROM pulsations WHERE id=?3),
               generation=generation+1
             WHERE id=?1 AND pulsation_id=?3 AND status='interrupted'",
            params![producer_job_id, now, current.id],
        )?;
        if changed != 1 {
            return Err(PersistenceServiceError::Validation(
                "The failed job is no longer available to resume.".to_string(),
            ));
        }
        transaction.execute(
            "UPDATE agent_runs SET status='failed',updated_at=?2
             WHERE job_id=?1 AND status='awaiting_decision'",
            params![producer_job_id, now],
        )?;
        Ok(updated)
    }

    /// The agent sees the operator's guidance first, then the machine diagnostic that
    /// prompted it, so a bare diagnostic never masquerades as a human instruction.
    fn stage_failure_revision_feedback(feedback: Option<&str>, error_code: &str) -> String {
        let written = feedback.map(str::trim).unwrap_or_default();
        if written.is_empty() {
            format!("The previous stage failed ({error_code}). Fix it and deliver again.")
        } else {
            format!("{written}\n\n---\nPrevious failure recorded by the host: {error_code}.")
        }
    }

    pub fn get_pulsation_runtime_snapshot(
        &self,
        request: GetPulsationRuntimeSnapshotRequest,
    ) -> Result<GetPulsationRuntimeSnapshotResponse, PersistenceServiceError> {
        self.get_pulsation_runtime_snapshot_for_detail(request, None)
    }

    /// The shell polls a cheap project-wide summary but asks for expensive coordination data only
    /// for the selected pulsation. Explicit persistence callers retain the complete snapshot API.
    pub fn get_pulsation_runtime_snapshot_for_detail(
        &self,
        request: GetPulsationRuntimeSnapshotRequest,
        detail_pulsation_id: Option<&str>,
    ) -> Result<GetPulsationRuntimeSnapshotResponse, PersistenceServiceError> {
        let project_id = Self::normalize_project_id(&request.project_id)?;
        let db = self.connection()?;
        let connection = db.unchecked_transaction()?;
        let pulsations = connection.prepare("SELECT id,project_id,feature_name,stage,status,created_at,updated_at,terminal_reason FROM pulsations WHERE project_id=?1 ORDER BY queue_sequence")?
            .query_map([&project_id],Self::map_pulsation)?.collect::<Result<Vec<_>,_>>()?;
        // Counted per purpose: a stage failure inflating `hitl_waiting_count` would
        // suppress the notification for a genuine artifact review opening afterwards.
        let hitl_waiting_count: i64 = connection.query_row(
            "SELECT COUNT(*)
             FROM pulsations p
             WHERE p.project_id = ?1 AND p.status = 'checkpoint'
               AND EXISTS(SELECT 1 FROM checkpoints c
                 WHERE c.pulsation_id=p.id AND c.status='waiting'
                   AND c.purpose='artifact_review')",
            params![project_id.as_str()],
            |row| row.get(0),
        )?;
        let failure_waiting_count: i64 = connection.query_row(
            "SELECT COUNT(DISTINCT c.pulsation_id)
             FROM checkpoints c JOIN pulsations p ON p.id=c.pulsation_id
             WHERE p.project_id = ?1 AND p.status = 'checkpoint'
               AND c.status='waiting' AND c.purpose='stage_failure'",
            params![project_id.as_str()],
            |row| row.get(0),
        )?;

        let background_agents_count: i64 = connection.query_row(
            "SELECT COUNT(*)
             FROM runtime_slots
             WHERE job_class='background' AND project_id=?1",
            params![project_id.as_str()],
            |row| row.get(0),
        )?;

        Ok(GetPulsationRuntimeSnapshotResponse {
            snapshot: PulsationRuntimeSnapshot {
                details: Self::coordination_details_for(
                    &connection,
                    &project_id,
                    detail_pulsation_id,
                )?,
                coordination: Some(Self::coordination_snapshot(&connection, &project_id)?),
                main_agents_count: connection.query_row(
                    "SELECT COUNT(*) FROM runtime_slots WHERE project_id=?1 AND job_class='main'",
                    [&project_id],
                    |r| r.get(0),
                )?,
                hitl_checkpoints: {
                    let mut stmt=connection.prepare("SELECT p.id,c.id,p.state_version FROM pulsations p JOIN checkpoints c ON c.pulsation_id=p.id WHERE p.project_id=?1 AND p.status='checkpoint' AND c.status='waiting' AND c.purpose='artifact_review'")?;
                    stmt.query_map([&project_id], |r| {
                        Ok(HitlCheckpoint {
                            pulsation_id: r.get(0)?,
                            checkpoint_id: r.get(1)?,
                            state_version: r.get(2)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?
                },
                failure_checkpoints: Self::stage_failure_checkpoints(&connection, &project_id)?,
                pulsations,
                hitl_waiting_count: hitl_waiting_count.max(0) as usize,
                failure_waiting_count: failure_waiting_count.max(0) as usize,
                background_agents_count: background_agents_count.max(0) as usize,
                active_jobs: Self::active_runtime_jobs(&connection, &project_id)?,
                brownfield_coverage: if Self::read_runtime_setting(
                    &connection,
                    &brownfield_consent_key(&project_id),
                )?
                .as_deref()
                    == Some("true")
                {
                    Some(Self::brownfield_coverage_metrics(&connection, &project_id)?)
                } else {
                    None
                },
            },
        })
    }

    pub fn list_pulsation_artifacts(
        &self,
        request: ListPulsationArtifactsRequest,
    ) -> Result<ListPulsationArtifactsResponse, PersistenceServiceError> {
        let pulsation_id = Self::normalize_pulsation_id(&request.pulsation_id)?;
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, pulsation_id, name, kind, relative_path, content_sha256, created_at, updated_at
             FROM artifacts
             WHERE pulsation_id = ?1
             ORDER BY created_at ASC",
        )?;
        let rows = statement.query_map(params![pulsation_id], Self::map_artifact)?;
        let mut artifacts = Vec::new();
        for row in rows {
            artifacts.push(row?);
        }
        Ok(ListPulsationArtifactsResponse { artifacts })
    }

    pub fn get_agent_runtime_config(
        &self,
    ) -> Result<GetAgentRuntimeConfigResponse, PersistenceServiceError> {
        let config = self.load_runtime_config()?;
        Ok(GetAgentRuntimeConfigResponse { config })
    }

    pub fn update_agent_runtime_config(
        &self,
        request: UpdateAgentRuntimeConfigRequest,
    ) -> Result<UpdateAgentRuntimeConfigResponse, PersistenceServiceError> {
        let mut config = self.load_runtime_config()?;

        if let Some(provider) = request.provider {
            config.provider = provider;
        }
        if let Some(model) = request.model {
            config.model = model;
        }
        if let Some(temperature) = request.temperature {
            config.temperature = temperature;
        }
        if let Some(max_output_tokens) = request.max_output_tokens {
            config.max_output_tokens = max_output_tokens;
        }
        if let Some(timeout_ms) = request.timeout_ms {
            config.timeout_ms = timeout_ms;
        }
        if let Some(max_retries) = request.max_retries {
            config.max_retries = max_retries;
        }
        if let Some(context_chars_limit) = request.context_chars_limit {
            config.context_chars_limit = context_chars_limit;
        }
        if let Some(max_context_files) = request.max_context_files {
            config.max_context_files = max_context_files;
        }
        if let Some(context_strategy) = request.context_strategy {
            config.context_strategy = context_strategy;
        }
        if let Some(embedding_enabled) = request.embedding_enabled {
            config.embedding_enabled = embedding_enabled;
        }
        if let Some(embedding_provider) = request.embedding_provider {
            config.embedding_provider = embedding_provider;
        }
        if let Some(embedding_model) = request.embedding_model {
            config.embedding_model = embedding_model;
        }
        if let Some(embedding_top_k) = request.embedding_top_k {
            config.embedding_top_k = embedding_top_k;
        }
        if let Some(adaptive_context_mode) = request.adaptive_context_mode {
            config.adaptive_context_mode = adaptive_context_mode;
        }
        if let Some(dynamic_context_policy) = request.dynamic_context_policy {
            config.dynamic_context_policy = dynamic_context_policy;
        }
        if let Some(routing_v2_mode) = request.routing_v2_mode {
            config.routing_v2_mode = routing_v2_mode;
        }

        let saved = self.persist_runtime_config(&config)?;
        self.indexing_configuration_changed(&saved)?;
        Ok(UpdateAgentRuntimeConfigResponse { config: saved })
    }

    pub fn save_provider_api_key(
        &self,
        request: SaveProviderApiKeyRequest,
    ) -> Result<SaveProviderApiKeyResponse, PersistenceServiceError> {
        self.credentials
            .set(request.provider.as_str(), &request.api_key)?;
        Ok(SaveProviderApiKeyResponse {
            provider: request.provider,
            saved: true,
        })
    }

    pub fn clear_provider_api_key(
        &self,
        request: ClearProviderApiKeyRequest,
    ) -> Result<ClearProviderApiKeyResponse, PersistenceServiceError> {
        let removed = self.credentials.delete(request.provider.as_str())?;
        Ok(ClearProviderApiKeyResponse {
            provider: request.provider,
            removed,
        })
    }

    pub fn get_provider_key_statuses(
        &self,
    ) -> Result<GetProviderKeyStatusesResponse, PersistenceServiceError> {
        let providers = LlmProvider::all()
            .into_iter()
            .map(|provider| ProviderKeyStatus {
                provider,
                has_api_key: self
                    .credentials
                    .get(provider.as_str())
                    .ok()
                    .flatten()
                    .is_some(),
            })
            .collect();
        Ok(GetProviderKeyStatusesResponse { providers })
    }

    pub fn list_provider_profiles(&self) -> ListProviderProfilesResponse {
        ListProviderProfilesResponse {
            profiles: provider_profiles(),
        }
    }

    pub fn list_provider_connections(
        &self,
    ) -> Result<ListProviderConnectionsResponse, PersistenceServiceError> {
        Ok(ListProviderConnectionsResponse {
            connections: self
                .llm_repository
                .list_connections(self.credentials.as_ref())?,
        })
    }

    /// Reads only previously persisted API catalog snapshots. This method never performs network I/O.
    pub fn list_api_model_catalogs(
        &self,
    ) -> Result<Vec<ApiModelCatalogSnapshot>, PersistenceServiceError> {
        self.api_model_catalog
            .list()
            .map_err(PersistenceServiceError::from)
    }

    /// Explicit user action only. Failures return a stale last-known-good snapshot when present.
    pub async fn refresh_api_model_catalog(
        &self,
        connection_id: String,
    ) -> Result<ApiModelCatalogSnapshot, PersistenceServiceError> {
        self.api_model_catalog
            .refresh(&connection_id)
            .await
            .map_err(PersistenceServiceError::from)
    }

    pub fn upsert_provider_connection(
        &self,
        request: UpsertProviderConnectionRequest,
    ) -> Result<UpsertProviderConnectionResponse, PersistenceServiceError> {
        Ok(UpsertProviderConnectionResponse {
            connection: self
                .llm_repository
                .upsert_connection(self.credentials.as_ref(), request)?,
        })
    }

    pub fn delete_provider_connection(
        &self,
        request: DeleteProviderConnectionRequest,
    ) -> Result<DeleteProviderConnectionResponse, PersistenceServiceError> {
        Ok(DeleteProviderConnectionResponse {
            deleted: self
                .llm_repository
                .delete_connection(self.credentials.as_ref(), request)?,
        })
    }

    pub async fn test_provider_connection(
        &self,
        request: TestProviderConnectionRequest,
    ) -> Result<TestProviderConnectionResponse, PersistenceServiceError> {
        self.llm_router
            .test_connection(&request.connection_id)
            .await
            .map_err(PersistenceServiceError::from)
    }

    /// Longer and more "note-shaped" than a trivial "ping": the readiness check exists to catch
    /// problems before a real reindex hits them, and a one-word probe would miss failures that
    /// only show up on realistic input (e.g. a legacy Ollama endpoint that doesn't truncate long
    /// prompts and crashes instead of erroring cleanly).
    const READINESS_PROBE_PROMPT: &str = "# Fixture note\n\nThis is a readiness probe standing in for a real knowledge note: a short paragraph of ordinary prose, roughly the length of a note's opening summary, so the check exercises the same code path a real reindex would use rather than a trivial one-word prompt.";

    /// Three independent checks, each gating the next, so the UI can point at exactly the step
    /// blocking embeddings instead of a raw HTTP error. Never returns `Err` for an unready
    /// state — "not ready yet" is a normal result, not a command failure.
    pub async fn check_embedding_readiness(
        &self,
    ) -> Result<EmbeddingReadinessReport, PersistenceServiceError> {
        let config = self.load_runtime_config()?;
        let model = config.embedding_model.clone();
        let models = match self.embedding_gateway.list_models().await {
            Ok(models) => models,
            Err(error) => {
                return Ok(EmbeddingReadinessReport {
                    model,
                    ollama_reachable: false,
                    model_installed: false,
                    embedding_call_ok: false,
                    error: Some(error.to_string()),
                });
            }
        };
        let model_installed = models
            .iter()
            .any(|installed| crate::embedding_service::model_names_match(installed, &model));
        if !model_installed {
            return Ok(EmbeddingReadinessReport {
                model,
                ollama_reachable: true,
                model_installed: false,
                embedding_call_ok: false,
                error: None,
            });
        }
        match self
            .embedding_gateway
            .embed(&model, Self::READINESS_PROBE_PROMPT)
            .await
        {
            Ok(_) => Ok(EmbeddingReadinessReport {
                model,
                ollama_reachable: true,
                model_installed: true,
                embedding_call_ok: true,
                error: None,
            }),
            Err(error) => Ok(EmbeddingReadinessReport {
                model,
                ollama_reachable: true,
                model_installed: true,
                embedding_call_ok: false,
                error: Some(error.to_string()),
            }),
        }
    }

    /// Downloads `model` (or the configured embedding model when `None`), publishing
    /// `ModelPullEvent`s as it goes so Settings can show live progress without polling.
    pub async fn pull_embedding_model(
        &self,
        model: Option<String>,
    ) -> Result<(), PersistenceServiceError> {
        let model = match model {
            Some(model) => model,
            None => self.load_runtime_config()?.embedding_model,
        };
        self.embedding_gateway
            .pull_model(&model, &|event| self.emit_model_pull_event(event))
            .await
            .map_err(|error| PersistenceServiceError::Validation(error.to_string()))
    }

    pub fn list_provider_routes(
        &self,
    ) -> Result<ListProviderRoutesResponse, PersistenceServiceError> {
        Ok(ListProviderRoutesResponse {
            routes: self.llm_repository.list_routes()?,
        })
    }

    pub fn upsert_provider_route(
        &self,
        request: UpsertProviderRouteRequest,
    ) -> Result<UpsertProviderRouteResponse, PersistenceServiceError> {
        Ok(UpsertProviderRouteResponse {
            route: self.llm_repository.upsert_route(request)?,
        })
    }

    pub fn delete_provider_route(
        &self,
        request: DeleteProviderRouteRequest,
    ) -> Result<DeleteProviderRouteResponse, PersistenceServiceError> {
        Ok(DeleteProviderRouteResponse {
            deleted: self.llm_repository.delete_route(request)?,
        })
    }

    pub fn list_provider_route_bindings(
        &self,
    ) -> Result<ListProviderRouteBindingsResponse, PersistenceServiceError> {
        Ok(ListProviderRouteBindingsResponse {
            bindings: self.llm_repository.list_bindings()?,
        })
    }

    pub fn upsert_provider_route_binding(
        &self,
        request: UpsertProviderRouteBindingRequest,
    ) -> Result<UpsertProviderRouteBindingResponse, PersistenceServiceError> {
        Ok(UpsertProviderRouteBindingResponse {
            binding: self.llm_repository.upsert_binding(request)?,
        })
    }

    pub fn list_free_form_provider_route_bindings(
        &self,
        project_id: &str,
    ) -> Result<ListFreeFormProviderRouteBindingsResponse, PersistenceServiceError> {
        Ok(ListFreeFormProviderRouteBindingsResponse {
            bindings: self.llm_repository.list_free_form_bindings(project_id)?,
        })
    }

    pub fn upsert_free_form_provider_route_binding(
        &self,
        request: UpsertFreeFormProviderRouteBindingRequest,
    ) -> Result<UpsertFreeFormProviderRouteBindingResponse, PersistenceServiceError> {
        Ok(UpsertFreeFormProviderRouteBindingResponse {
            binding: self.llm_repository.upsert_free_form_binding(request)?,
        })
    }

    pub fn list_agent_api_executions(
        &self,
        request: ListAgentApiExecutionsRequest,
    ) -> Result<ListAgentApiExecutionsResponse, PersistenceServiceError> {
        Ok(ListAgentApiExecutionsResponse {
            executions: self.llm_repository.list_executions_for_target(
                &request.project_id,
                &request.target,
                request.limit.unwrap_or(20),
            )?,
        })
    }

    pub fn get_agent_api_execution_snapshot(
        &self,
        request: GetAgentApiExecutionSnapshotRequest,
    ) -> Result<GetLlmExecutionSnapshotResponse, PersistenceServiceError> {
        if !self.llm_repository.execution_belongs_to_target(
            &request.project_id,
            &request.target,
            &request.execution_id,
        )? {
            return Err(PersistenceServiceError::NotFound(
                "API execution was not found for this project and agent.".into(),
            ));
        }
        Ok(GetLlmExecutionSnapshotResponse {
            snapshot: self
                .llm_repository
                .execution_snapshot(&request.execution_id)?,
        })
    }

    pub fn export_agent_api_execution(
        &self,
        request: GetAgentApiExecutionSnapshotRequest,
    ) -> Result<PathBuf, PersistenceServiceError> {
        // Authorize the target before reading its private original evidence.
        if !self.llm_repository.execution_belongs_to_target(
            &request.project_id,
            &request.target,
            &request.execution_id,
        )? {
            return Err(PersistenceServiceError::NotFound(
                "API execution was not found for this project and agent.".into(),
            ));
        }
        let (snapshot, original_evidence_sha256) = self
            .llm_repository
            .execution_snapshot_with_evidence(&request.execution_id)?;
        let workspace_path: String = self
            .connection()?
            .query_row(
                "SELECT workspace_path FROM projects WHERE id=?1 AND is_archived=0",
                params![request.project_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| {
                PersistenceServiceError::NotFound("Project workspace was not found.".into())
            })?;
        let directory = Path::new(&workspace_path)
            .join(".hard-vibe-code")
            .join("api-exports");
        fs::create_dir_all(&directory)?;
        let destination = directory.join(format!("api-{}.json", request.execution_id));
        let mut export = serde_json::json!({
            "schemaVersion": 1,
            "redacted": true,
            "projectId": request.project_id,
            "target": request.target,
            "snapshot": snapshot,
        });
        crate::llm::adapter::sanitize_json_strings(&mut export)?;
        let canonical = crate::llm::repository::canonical_evidence_json(&export)?;
        export["sanitization"] = serde_json::json!({
            "policy": crate::secret_redactor::SANITIZATION_POLICY_VERSION,
            "originalEvidenceSha256": original_evidence_sha256,
            "sanitizedContentSha256": crate::secret_redactor::text_sha256(&canonical),
            "hashScope": "UTF-8 compact JSON with recursively sorted object keys and arrays in original order; original: selected raw execution evidence; sanitized: export envelope excluding sanitization metadata and final file bytes",
        });
        let payload = serde_json::to_vec_pretty(&export)
            .map_err(|_| PersistenceServiceError::Llm(crate::llm::adapter::sanitization_error()))?;
        if payload.len() > crate::secret_redactor::MAX_SANITIZATION_BYTES {
            return Err(crate::llm::adapter::sanitization_error().into());
        }
        let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
        std::io::Write::write_all(&mut temporary, &payload)?;
        temporary
            .persist(&destination)
            .map_err(|error| error.error)?;
        Ok(destination)
    }

    pub fn start_free_form_api(
        &self,
        request: StartFreeFormApiRunRequest,
    ) -> Result<StartFreeFormApiRunResponse, PersistenceServiceError> {
        let project_id = request.project_id.trim();
        let slot_id = request.slot_id.trim();
        let prompt = request.prompt.trim();
        if project_id.is_empty() || slot_id.is_empty() || prompt.is_empty() {
            return Err(PersistenceServiceError::Validation(
                "Project, FreeForm slot and prompt are required.".into(),
            ));
        }
        let connection = self.connection()?;
        let api_ready: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM terminal_slots
                 WHERE id=?1 AND project_id=?2 AND kind='free_form' AND archived=0
                   AND execution_mode='api_direct')",
            params![slot_id, project_id],
            |row| row.get(0),
        )?;
        if !api_ready {
            return Err(PersistenceServiceError::Validation(
                "FreeForm slot is missing, archived, or not in API Key Mode.".into(),
            ));
        }
        // Resolve before spawning so a missing/disabled binding fails synchronously in the UI.
        self.llm_repository.resolve_route_for_target(
            project_id,
            &AgentExecutionTarget::free_form(slot_id),
            "FreeForm",
        )?;
        let config = self.load_runtime_config()?;
        let execution_id = Uuid::new_v4().to_string();
        let generation_id = Uuid::new_v4().to_string();
        let llm_request = LlmRequest {
            execution_id: ExecutionId::new(execution_id.clone()),
            generation_id: GenerationId::new(generation_id.clone()),
            project_id: project_id.to_string(),
            pulsation_id: None,
            job_id: None,
            agent_run_id: None,
            job_chain: format!("free_form:{slot_id}"),
            target: AgentExecutionTarget::free_form(slot_id),
            stage: "FreeForm".into(),
            system_prompt: "You are a user-configured FreeForm agent. Return only the visible assistant response. Do not expose hidden reasoning, credentials, or internal provider payloads.".into(),
            user_prompt: prompt.to_string(),
            response_format: ResponseFormat::Text,
            max_output_tokens: config.max_output_tokens,
            temperature: config.temperature,
            deadline: std::time::Instant::now() + Duration::from_millis(config.timeout_ms),
            stream: true,
            legacy_provider: config.provider.as_str().into(),
            legacy_model: config.model,
            dispatch: DispatchPolicy::default(),
        };
        let cancellation = CancellationToken::new();
        self.runtime_control
            .manual_api_cancellations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(execution_id.clone(), cancellation.clone());
        let service = self.clone();
        let execution_for_task = execution_id.clone();
        tokio::spawn(async move {
            let _ = service
                .llm_router
                .execute(&llm_request, &cancellation)
                .await;
            service
                .runtime_control
                .manual_api_cancellations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&execution_for_task);
        });
        Ok(StartFreeFormApiRunResponse {
            execution_id,
            generation_id,
        })
    }

    pub fn cancel_agent_api_execution(
        &self,
        request: CancelAgentApiExecutionRequest,
    ) -> Result<CancelAgentApiExecutionResponse, PersistenceServiceError> {
        let execution_id = request.execution_id.trim();
        if execution_id.is_empty() {
            return Err(PersistenceServiceError::Validation(
                "Execution ID is required.".into(),
            ));
        }
        let cancelled = self
            .runtime_control
            .manual_api_cancellations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(execution_id)
            .map(|token| {
                token.cancel();
                true
            })
            .unwrap_or(false);
        Ok(CancelAgentApiExecutionResponse { cancelled })
    }

    pub fn get_llm_resilience_snapshot(
        &self,
    ) -> Result<GetLlmResilienceSnapshotResponse, PersistenceServiceError> {
        Ok(GetLlmResilienceSnapshotResponse {
            snapshot: self.llm_repository.resilience_snapshot()?,
        })
    }

    pub fn reset_llm_resilience_state(
        &self,
        request: ResetLlmResilienceStateRequest,
    ) -> Result<ResetLlmResilienceStateResponse, PersistenceServiceError> {
        Ok(ResetLlmResilienceStateResponse {
            reset_count: self.llm_repository.reset_resilience(request)?,
        })
    }

    pub fn get_llm_execution_snapshot(
        &self,
        request: GetLlmExecutionSnapshotRequest,
    ) -> Result<GetLlmExecutionSnapshotResponse, PersistenceServiceError> {
        Ok(GetLlmExecutionSnapshotResponse {
            snapshot: self
                .llm_repository
                .execution_snapshot(request.execution_id.trim())?,
        })
    }

    pub fn get_llm_routing_metrics(
        &self,
    ) -> Result<GetLlmRoutingMetricsResponse, PersistenceServiceError> {
        Ok(GetLlmRoutingMetricsResponse {
            metrics: self.llm_repository.routing_metrics()?,
        })
    }

    pub fn preview_context_compression(
        &self,
        request: CompressionPreviewRequest,
    ) -> Result<PreviewContextCompressionResponse, PersistenceServiceError> {
        let preview = preview_compression(request);
        self.llm_repository
            .record_compression_metric(None, &preview)?;
        Ok(PreviewContextCompressionResponse { preview })
    }

    pub fn cancel_pulsation_jobs(
        &self,
        request: CancelPulsationJobsRequest,
    ) -> Result<CancelPulsationJobsResponse, PersistenceServiceError> {
        let pulse = Self::normalize_pulsation_id(&request.pulsation_id)?;
        let _intent = self.begin_user_cancellation(&pulse);
        let mut db = self.connection()?;
        // Shared SQLite write serialization includes each atomic filesystem publication.
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current = Self::find_pulsation_by_id(&tx, &pulse)?
            .ok_or_else(|| PersistenceServiceError::NotFound("Pulsation not found.".into()))?;
        if matches!(
            current.status,
            PulsationStatus::Done | PulsationStatus::Failed
        ) {
            return Ok(CancelPulsationJobsResponse { cancelled_jobs: 0 });
        }
        let ids=tx.prepare("SELECT id FROM jobs WHERE pulsation_id=?1 AND status IN ('pending','running','interrupted')")?.query_map([&pulse],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
        let now = Self::now_iso();
        tx.execute("UPDATE jobs SET status='failed',cancellation_reason='user_cancel',cancel_requested_at=?2,finished_at=?2,updated_at=?2 WHERE pulsation_id=?1 AND status IN ('pending','running','interrupted')",params![pulse,now])?;
        Self::transition_pulsation(
            &tx,
            &current,
            PulsationStatus::Failed,
            current.stage,
            "user_cancel",
            Some("Pulsation cancelled by the user; files already published were not undone."),
        )?;
        tx.execute(
            "UPDATE pulsations SET terminal_reason='user_cancel' WHERE id=?1",
            [&pulse],
        )?;
        Self::mark_effects_for_review(&tx, &pulse, "user_cancel_with_effects")?;
        tx.execute("UPDATE artifact_publications SET state='cancelled' WHERE pulsation_id=?1 AND state IN ('pending','applying')",[&pulse])?;
        tx.execute("UPDATE checkpoints SET status='rejected',updated_at=?2 WHERE pulsation_id=?1 AND status='waiting'",params![pulse,now])?;
        tx.execute("UPDATE stage_failure_requests SET decision='abort',resolved_at=?2,updated_at=?2 WHERE pulsation_id=?1 AND decision IS NULL",params![pulse,now])?;
        tx.execute("UPDATE cli_fallback_requests SET decision='cancel',resolved_at=?2,updated_at=?2 WHERE pulsation_id=?1 AND decision IS NULL",params![pulse,now])?;
        Self::enqueue_learning(&tx, &pulse)?;
        tx.commit()?;
        for id in &ids {
            self.request_job_cancellation(id);
        }
        self.wake_scheduler();
        Ok(CancelPulsationJobsResponse {
            cancelled_jobs: ids.len(),
        })
    }

    pub fn list_agent_runs(
        &self,
        request: ListAgentRunsRequest,
    ) -> Result<ListAgentRunsResponse, PersistenceServiceError> {
        let pulsation_id = request
            .pulsation_id
            .as_deref()
            .map(Self::normalize_pulsation_id)
            .transpose()?;
        let project_id = request
            .project_id
            .as_deref()
            .map(Self::normalize_project_id)
            .transpose()?;
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT
               ar.id AS run_id,
               ar.job_id AS job_id,
               ar.agent_name AS agent_name,
               ar.status AS run_status,
               ar.input_tokens AS input_tokens,
               ar.output_tokens AS output_tokens,
               ar.provider AS provider,
               ar.model AS model,
               ar.estimated_cost_usd AS estimated_cost_usd,
               ar.created_at AS created_at,
               ar.updated_at AS updated_at,
               j.last_error AS last_error,
               j.started_at AS started_at,
               j.finished_at AS finished_at,
               j.kind AS job_kind,
               j.cancellation_reason AS cancellation_reason,
               j.pulsation_id AS pulsation_id,
               p.feature_name AS pulsation_name,
               p.status AS pulsation_status,
               CAST(MAX(0, (julianday(COALESCE(j.finished_at, ar.updated_at)) - julianday(COALESCE(j.started_at, ar.created_at))) * 86400000) AS INTEGER) AS duration_ms,
               aro.raw_output AS raw_output,
               aro.structured_output AS structured_output,
               aro.validation_error AS validation_error,
               aro.tool_trace AS tool_trace
             FROM agent_runs ar
             INNER JOIN jobs j ON j.id = ar.job_id
             LEFT JOIN pulsations p ON p.id = j.pulsation_id
             LEFT JOIN agent_run_outputs aro ON aro.agent_run_id = ar.id
             WHERE (?1 IS NULL OR j.pulsation_id = ?1)
               AND (?2 IS NULL OR p.project_id = ?2 OR j.project_id = ?2)
             ORDER BY ar.created_at DESC",
        )?;

        let rows = statement.query_map(
            params![pulsation_id.as_deref(), project_id.as_deref()],
            |row| {
                let provider = row.get::<_, Option<String>>("provider")?;
                let structured_output_raw: Option<String> = row.get("structured_output")?;
                let structured_output = structured_output_raw
                    .as_deref()
                    .and_then(|raw| serde_json::from_str::<Value>(raw).ok());

                Ok(AgentRunListItem {
                    id: row.get("run_id")?,
                    job_id: row.get("job_id")?,
                    agent_name: row.get("agent_name")?,
                    status: row.get("run_status")?,
                    input_tokens: row.get("input_tokens")?,
                    output_tokens: row.get("output_tokens")?,
                    provider,
                    model: row.get("model")?,
                    estimated_cost_usd: row.get("estimated_cost_usd")?,
                    last_error: row.get("last_error")?,
                    started_at: row.get("started_at")?,
                    finished_at: row.get("finished_at")?,
                    raw_output: row.get("raw_output")?,
                    structured_output,
                    created_at: row.get("created_at")?,
                    updated_at: row.get("updated_at")?,
                    pulsation_id: row.get("pulsation_id")?,
                    pulsation_name: row.get("pulsation_name")?,
                    pulsation_status: row.get("pulsation_status")?,
                    job_kind: row.get("job_kind")?,
                    duration_ms: row.get("duration_ms")?,
                    cancellation_reason: row.get("cancellation_reason")?,
                    validation_error: row.get("validation_error")?,
                    tool_trace: row.get("tool_trace")?,
                })
            },
        )?;

        let mut runs = Vec::new();
        for row in rows {
            let mut run = row?;
            run.raw_output = run.raw_output.map(|text| sanitized_observation_text(&text));
            run.structured_output = run
                .structured_output
                .map(|value| sanitized_observation_json(&value));
            run.last_error = run.last_error.map(|text| sanitized_observation_text(&text));
            run.validation_error = run
                .validation_error
                .map(|text| sanitized_observation_text(&text));
            run.tool_trace = run.tool_trace.map(|text| sanitized_observation_text(&text));
            runs.push(run);
        }

        Ok(ListAgentRunsResponse { runs })
    }

    pub fn recover_interrupted_jobs(&self) -> Result<(), PersistenceServiceError> {
        // The AppCore data-dir lock must be held by the caller. Never call an LLM here.
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let now = Self::now_iso();
        let legacy_cancelled=tx.prepare("SELECT id FROM pulsations p WHERE p.status NOT IN ('failed','done') AND EXISTS(SELECT 1 FROM jobs j WHERE j.pulsation_id=p.id AND j.cancellation_reason='user_cancel')")?.query_map([],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
        for id in legacy_cancelled {
            let current = Self::find_pulsation_by_id(&tx, &id)?
                .ok_or_else(|| PersistenceServiceError::NotFound("Pulsation missing".into()))?;
            Self::transition_pulsation(
                &tx,
                &current,
                PulsationStatus::Failed,
                current.stage,
                "recovered_user_cancel",
                Some("User cancellation kept during recovery."),
            )?;
            tx.execute(
                "UPDATE pulsations SET terminal_reason='user_cancel' WHERE id=?1",
                [&id],
            )?;
            Self::mark_effects_for_review(&tx, &id, "recovered_user_cancel")?;
            Self::enqueue_learning(&tx, &id)?;
        }
        tx.execute("UPDATE project_pipeline_state SET base_state='needs_review',blocking_reason='stale_publication_uncertain' WHERE project_id IN (SELECT o.project_id FROM artifact_publications o JOIN artifacts a ON a.id=o.artifact_id JOIN pulsations p ON p.id=o.pulsation_id JOIN jobs j ON j.id=a.producer_job_id WHERE o.job_class='main' AND o.state='applying' AND (a.input_revision<>p.input_revision OR a.generation<>j.generation OR a.acceptance='superseded' OR p.status='failed'))",[])?;
        tx.execute("UPDATE artifact_publications SET state='cancelled' WHERE job_class='main' AND state IN ('pending','applying') AND artifact_id IN (SELECT a.id FROM artifacts a JOIN pulsations p ON p.id=a.pulsation_id JOIN jobs j ON j.id=a.producer_job_id WHERE a.input_revision<>p.input_revision OR a.generation<>j.generation OR a.acceptance='superseded' OR p.status='failed')",[])?;
        tx.execute("UPDATE jobs SET status='failed',cancellation_reason=COALESCE(cancellation_reason,'superseded'),updated_at=?1 WHERE status IN ('pending','running','interrupted') AND (cancellation_reason IN ('user_cancel','superseded') OR (kind!='learning_report' AND EXISTS(SELECT 1 FROM pulsations p WHERE p.id=jobs.pulsation_id AND (p.status IN ('failed','done') OR p.input_revision<>jobs.input_revision))))",[&now])?;
        tx.execute("UPDATE jobs SET status='pending',state_version=(SELECT state_version FROM pulsations WHERE id=jobs.pulsation_id),generation=generation+1,started_at=NULL,cancel_requested_at=NULL,cancellation_reason=NULL,available_at=?1,updated_at=?1 WHERE status IN ('running','interrupted') AND (cancellation_reason IS NULL OR cancellation_reason IN ('shutdown','preempted')) AND NOT EXISTS(SELECT 1 FROM stage_acceptances a WHERE a.job_id=jobs.id) AND EXISTS(SELECT 1 FROM pulsations p WHERE p.id=jobs.pulsation_id AND p.status IN ('planning','running') AND p.input_revision=jobs.input_revision)",[&now])?;
        tx.execute("UPDATE agent_runs SET status='interrupted',updated_at=?1 WHERE status='running' AND NOT EXISTS(SELECT 1 FROM stage_acceptances a WHERE a.agent_run_id=agent_runs.id)",[&now])?;
        Self::recover_coordination(&tx)?;
        Self::recover_stage_executions(&tx)?;
        Self::recover_knowledge_background(&tx)?;
        tx.execute("UPDATE jobs SET status='failed',cancellation_reason='superseded',last_error='Preparation interrupted by a restart; context is gathered on demand.' WHERE kind='prepare_context' AND status IN ('pending','running','interrupted')",[])?;
        tx.execute("UPDATE pulsation_preparation SET prespec_state='cancelled',diagnostic='Preparation interrupted by a restart; context is gathered on demand.' WHERE prespec_state IN ('pending','running')",[])?;
        tx.commit()?;
        self.reconcile_publications()
    }

    /// Startup self-heal for the `elect_next_backend` `state_version` desync:
    /// a job left `pending` by the CLI backend ladder could carry a
    /// `state_version` older than its pulsation's, fencing it out of
    /// `claim_next_pending_job` forever. Finds every such job, realigns its
    /// fence to the pulsation's current version, and records one
    /// `pulse_events` row per repaired pulsation.
    ///
    /// Idempotent: a database with no drift touches nothing and returns an
    /// empty vec. Safe to run on every boot, ahead of the scheduler.
    pub fn repair_state_version_drift(&self) -> Result<Vec<String>, PersistenceServiceError> {
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let now = Self::now_iso();

        let drifted_pulsations: Vec<String> = tx
            .prepare(
                "SELECT DISTINCT j.pulsation_id
                 FROM jobs j JOIN pulsations p ON p.id = j.pulsation_id
                 WHERE j.status = 'pending' AND j.state_version <> p.state_version",
            )?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut repaired = Vec::new();
        for pulsation_id in drifted_pulsations {
            let Some(current) = Self::find_pulsation_by_id(&tx, &pulsation_id)? else {
                continue;
            };
            let changed = tx.execute(
                "UPDATE jobs
                 SET state_version = (SELECT state_version FROM pulsations WHERE id = ?1),
                     updated_at = ?2
                 WHERE pulsation_id = ?1 AND status = 'pending'
                   AND state_version <> (SELECT state_version FROM pulsations WHERE id = ?1)",
                params![pulsation_id, now],
            )?;
            let event_id = Uuid::new_v4().to_string();
            tx.execute(
                "INSERT INTO pulse_events (
                   id, pulsation_id, event_type, from_status, to_status, from_stage, to_stage, message, created_at
                 ) VALUES (?1, ?2, 'state_version_drift_repaired', ?3, ?3, ?4, ?4, ?5, ?6)",
                params![
                    event_id,
                    pulsation_id,
                    current.status.as_db_value(),
                    current.stage.as_db_value(),
                    format!(
                        "Startup repair: {changed} pending job(s) with a stale state_version were realigned."
                    ),
                    now,
                ],
            )?;
            repaired.push(pulsation_id);
        }

        tx.commit()?;
        if !repaired.is_empty() {
            self.wake_scheduler();
        }
        Ok(repaired)
    }

    fn stage_from_job_kind(kind: &str) -> Option<PulsationStage> {
        let stage_raw = kind.strip_prefix("run_stage::")?;
        PulsationStage::from_db_value(stage_raw)
    }

    fn start_agent_run_for_job(
        &self,
        job: &Job,
        runtime_config: &AgentRuntimeConfig,
    ) -> Result<String, PersistenceServiceError> {
        let connection = self.connection()?;
        let id = Uuid::new_v4().to_string();
        let now = Self::now_iso();
        let agent_name = if let Some(stage) = Self::stage_from_job_kind(&job.kind) {
            crate::llm::AgentRole::for_stage(stage.as_db_value())
                .as_str()
                .to_string()
        } else {
            "orchestrator::unknown".to_string()
        };
        // Seed the transport that is actually about to run. Seeding the
        // API-direct legacy defaults instead attributes every failed attempt to
        // a provider that never executed, because only the success path
        // (`update_agent_run_metrics`) ever corrected them.
        let (provider, model) = Self::planned_transport(&connection, job, &agent_name)?
            .unwrap_or_else(|| {
                (
                    runtime_config.provider.as_str().to_string(),
                    runtime_config.model.clone(),
                )
            });

        connection.execute(
            "INSERT INTO agent_runs (
               id,
               job_id,
               agent_name,
               status,
               input_tokens,
               output_tokens,
               provider,
               model,
               estimated_cost_usd,
               created_at,
               updated_at
             ) VALUES (?1, ?2, ?3, 'running', NULL, NULL, ?4, ?5, NULL, ?6, ?6)",
            params![id, job.id.as_str(), agent_name, provider, model, now],
        )?;

        Ok(id)
    }

    /// The transport this job will use, as `("cli:<backend>", model)`, or
    /// `None` when the call goes to the HTTP router and the caller's legacy
    /// provider/model are the right answer. Mirrors the order
    /// `TerminalService::execute_llm_request` resolves in: a per-pulsation
    /// API override wins over the project's per-role configuration.
    fn planned_transport(
        connection: &Connection,
        job: &Job,
        agent_role: &str,
    ) -> Result<Option<(String, String)>, PersistenceServiceError> {
        let Some(pulsation_id) = job.pulsation_id.as_deref() else {
            return Ok(None);
        };
        let overridden: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM pulse_execution_overrides WHERE pulsation_id=?1 AND agent_role=?2)",
            params![pulsation_id, agent_role],
            |row| row.get(0),
        )?;
        if overridden {
            return Ok(None);
        }
        let configured = connection
            .query_row(
                "SELECT config.execution_mode,config.backend,config.model FROM cli_agent_configs config JOIN pulsations pulse ON pulse.project_id=config.project_id WHERE pulse.id=?1 AND config.agent_role=?2",
                params![pulsation_id, agent_role],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?;
        Ok(configured.and_then(|(execution_mode, backend, model)| {
            (execution_mode == "cli_process").then(|| {
                (
                    format!("cli:{backend}"),
                    model.unwrap_or_else(|| "default".into()),
                )
            })
        }))
    }

    fn finish_agent_run(
        &self,
        agent_run_id: &str,
        status: &str,
    ) -> Result<(), PersistenceServiceError> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE agent_runs
             SET status = ?1, updated_at = ?2
             WHERE id = ?3",
            params![status, Self::now_iso(), agent_run_id],
        )?;
        Ok(())
    }

    fn finalize_job(
        &self,
        job_id: &str,
        status: JobStatus,
        last_error: Option<String>,
    ) -> Result<(), PersistenceServiceError> {
        let connection = self.connection()?;
        let now = Self::now_iso();
        connection.execute(
            "UPDATE jobs
             SET status = ?1,
                 last_error = ?2,
                  cancel_requested_at = NULL,
                  available_at = NULL,
                  updated_at = ?3,
                 finished_at = ?3
             WHERE id = ?4",
            params![status.as_db_value(), last_error, now, job_id],
        )?;
        Ok(())
    }

    fn requeue_job(
        &self,
        job_id: &str,
        attempts: i64,
        last_error: &str,
    ) -> Result<(), PersistenceServiceError> {
        let connection = self.connection()?;
        let now = Utc::now();
        let base_ms = if attempts <= 1 { 2_000_u64 } else { 5_000_u64 };
        let mut hasher = Sha256::new();
        hasher.update(job_id.as_bytes());
        hasher.update(attempts.to_le_bytes());
        let digest = hasher.finalize();
        let bucket = u16::from_le_bytes([digest[0], digest[1]]) as f64 / u16::MAX as f64;
        let jitter = 0.8 + bucket * 0.4;
        let available_at =
            now + chrono::Duration::milliseconds((base_ms as f64 * jitter).round() as i64);
        let now_iso = now.to_rfc3339();
        connection.execute(
            "UPDATE jobs
             SET status = ?1,
                 last_error = ?2,
                 updated_at = ?3,
                 started_at = NULL,
                  finished_at = NULL,
                  cancel_requested_at = NULL,
                  available_at = ?4,
                  state_version = (SELECT p.state_version FROM pulsations p WHERE p.id=jobs.pulsation_id)
              WHERE id = ?5",
            params![
                JobStatus::Pending.as_db_value(),
                last_error,
                now_iso,
                available_at.to_rfc3339(),
                job_id
            ],
        )?;
        Ok(())
    }

    fn update_agent_run_metrics(
        connection: &Connection,
        agent_run_id: &str,
        execution: &StageExecutionResult,
    ) -> Result<(), PersistenceServiceError> {
        connection.execute(
            "UPDATE agent_runs
             SET input_tokens = ?1,
                 output_tokens = ?2,
                 provider = ?3,
                 model = ?4,
                  estimated_cost_usd = ?5,
                  route_id = ?6,
                  connection_id = ?7,
                  llm_execution_id = ?8,
                  updated_at = ?9
              WHERE id = ?10",
            params![
                execution.input_tokens,
                execution.output_tokens,
                execution.provider_used.as_str(),
                execution.model_used.as_str(),
                execution.estimated_cost_usd,
                execution.route_id.as_deref(),
                execution.connection_id.as_deref(),
                execution.llm_execution_id.as_deref(),
                Self::now_iso(),
                agent_run_id
            ],
        )?;
        Ok(())
    }

    fn persist_agent_run_output(
        connection: &Connection,
        agent_run_id: &str,
        job: &Job,
        execution: &StageExecutionResult,
        validation_error: Option<&str>,
    ) -> Result<(), PersistenceServiceError> {
        let now = Self::now_iso();
        connection.execute(
            "INSERT INTO agent_run_outputs (
               id,
               agent_run_id,
               job_id,
               pulsation_id,
               request_payload,
               response_payload,
               raw_output,
               structured_output,
               schema_json,
               validation_error,
               tool_trace,
               created_at,
               updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)
             ON CONFLICT(agent_run_id)
             DO UPDATE SET
               request_payload = excluded.request_payload,
               response_payload = excluded.response_payload,
               raw_output = excluded.raw_output,
               structured_output = excluded.structured_output,
               schema_json = excluded.schema_json,
               validation_error = excluded.validation_error,
               tool_trace = excluded.tool_trace,
               updated_at = excluded.updated_at",
            params![
                Uuid::new_v4().to_string(),
                agent_run_id,
                job.id.as_str(),
                job.pulsation_id.as_deref(),
                sanitized_observation_json(&execution.request_payload).to_string(),
                sanitized_observation_json(&execution.response_payload).to_string(),
                sanitized_observation_text(&execution.raw_output),
                sanitized_observation_json(&execution.structured_output).to_string(),
                sanitized_observation_json(&execution.schema_json).to_string(),
                validation_error.map(sanitized_observation_text),
                sanitized_observation_json(&execution.tool_trace).to_string(),
                now
            ],
        )?;
        Ok(())
    }

    /// Sibling of `persist_agent_run_output` for a failed attempt: no
    /// structured output/schema/tool trace exist yet (validation failed
    /// before they could be produced), only the raw text and the error that
    /// rejected it - captured so a corrective retry can show the model both.
    fn persist_failed_llm_output(
        connection: &Connection,
        agent_run_id: &str,
        job: &Job,
        output: &crate::agent_runtime::FailedCallOutput,
        validation_error: &str,
    ) -> Result<(), PersistenceServiceError> {
        let now = Self::now_iso();
        connection.execute(
            "INSERT INTO agent_run_outputs (
               id,
               agent_run_id,
               job_id,
               pulsation_id,
               request_payload,
               response_payload,
               raw_output,
               structured_output,
               schema_json,
               validation_error,
               tool_trace,
               created_at,
               updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, NULL, ?8, NULL, ?9, ?9)
             ON CONFLICT(agent_run_id)
             DO UPDATE SET
               request_payload = excluded.request_payload,
               response_payload = excluded.response_payload,
               raw_output = excluded.raw_output,
               validation_error = excluded.validation_error,
               updated_at = excluded.updated_at",
            params![
                Uuid::new_v4().to_string(),
                agent_run_id,
                job.id.as_str(),
                job.pulsation_id.as_deref(),
                sanitized_observation_json(&output.request_payload).to_string(),
                sanitized_observation_json(&output.response_payload).to_string(),
                sanitized_observation_text(&output.raw_output),
                sanitized_observation_text(validation_error),
                now
            ],
        )?;
        Ok(())
    }

    fn persist_context_retrieval_trace(
        connection: &Connection,
        agent_run_id: &str,
        job: &Job,
        trace: &ContextRetrievalTrace,
    ) -> Result<(), PersistenceServiceError> {
        let Some(pulsation_id) = job.pulsation_id.as_deref() else {
            return Ok(());
        };
        let Some((project_id, stage_name)) = connection
            .query_row(
                "SELECT p.project_id,r.agent_name FROM pulsations p JOIN agent_runs r ON r.id=?2 WHERE p.id=?1",
                params![pulsation_id, agent_run_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
        else {
            return Ok(());
        };
        let now = Self::now_iso();
        let retrieval_id = connection
            .query_row(
                "SELECT id FROM context_retrieval_runs WHERE agent_run_id=?1",
                [agent_run_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        connection.execute(
            "INSERT INTO context_retrieval_runs(
               id,project_id,pulsation_id,job_id,agent_run_id,stage_name,
               requested_strategy,strategy_used,adaptive_mode,expansion_depth,
               fallback_used,max_similarity,duration_ms,considered_total,
               delivered_total,cited_total,created_at,updated_at
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?17)
             ON CONFLICT(agent_run_id) DO UPDATE SET
               strategy_used=excluded.strategy_used,
               expansion_depth=excluded.expansion_depth,
               fallback_used=excluded.fallback_used,
               max_similarity=excluded.max_similarity,
               duration_ms=excluded.duration_ms,
               considered_total=excluded.considered_total,
               delivered_total=excluded.delivered_total,
               cited_total=excluded.cited_total,
               updated_at=excluded.updated_at",
            params![
                retrieval_id,
                project_id,
                pulsation_id,
                job.id,
                agent_run_id,
                stage_name,
                trace.requested_strategy,
                trace.strategy_used,
                trace.adaptive_mode,
                trace
                    .expansion_depth
                    .and_then(|value| i64::try_from(value).ok()),
                trace.fallback_used,
                trace.max_similarity,
                i64::try_from(trace.duration_ms).unwrap_or(i64::MAX),
                i64::try_from(trace.considered_total()).unwrap_or(i64::MAX),
                i64::try_from(trace.delivered_total()).unwrap_or(i64::MAX),
                i64::try_from(trace.cited_total()).unwrap_or(i64::MAX),
                now,
            ],
        )?;
        connection.execute(
            "DELETE FROM context_retrieval_items WHERE retrieval_run_id=?1",
            [&retrieval_id],
        )?;
        for item in &trace.items {
            connection.execute(
                "INSERT INTO context_retrieval_items(
                   id,retrieval_run_id,relative_path,item_kind,considered,delivered,cited,
                   content_sha256,rank,graph_score,semantic_similarity,final_score,
                   delivered_chars,citation_reason
                 ) VALUES(?1,?2,?3,?4,1,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
                params![
                    Uuid::new_v4().to_string(),
                    retrieval_id,
                    item.relative_path,
                    item.item_kind,
                    item.delivered,
                    item.cited,
                    item.content_sha256,
                    i64::try_from(item.rank).unwrap_or(i64::MAX),
                    item.graph_score.and_then(|value| i64::try_from(value).ok()),
                    item.semantic_similarity,
                    item.final_score.and_then(|value| i64::try_from(value).ok()),
                    i64::try_from(item.delivered_chars).unwrap_or(i64::MAX),
                    item.citation_reason,
                ],
            )?;
        }
        Ok(())
    }

    fn transition_for_failed_job(
        &self,
        job: &Job,
        failure: &PersistenceServiceError,
        will_retry: bool,
    ) -> Result<(), PersistenceServiceError> {
        let Some(pulsation_id) = job.pulsation_id.as_deref() else {
            return Ok(());
        };
        let Some(stage) = Self::stage_from_job_kind(&job.kind) else {
            return Ok(());
        };

        let mut connection = self.connection()?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current = Self::find_pulsation_by_id(&transaction, pulsation_id)?.ok_or_else(|| {
            PersistenceServiceError::NotFound("The job's pulsation was not found.".to_string())
        })?;

        if current.status != PulsationStatus::Running || current.stage != stage {
            transaction.commit()?;
            return Ok(());
        }

        if will_retry {
            let _ = Self::transition_pulsation(
                &transaction,
                &current,
                PulsationStatus::Planning,
                stage,
                "job_retry_scheduled",
                Some("Transient failure detected. Job rescheduled."),
            )?;
            transaction.commit()?;
            return Ok(());
        }

        let (event_type, message) = match failure {
            PersistenceServiceError::Cancelled(message) => ("job_cancelled", message.as_str()),
            _ => ("job_failed", "Failure while running the stage."),
        };
        let _ = Self::transition_pulsation(
            &transaction,
            &current,
            PulsationStatus::Failed,
            stage,
            event_type,
            Some(message),
        )?;
        Self::mark_effects_for_review(&transaction, pulsation_id, "execution_failed_with_effects")?;
        Self::enqueue_learning(&transaction, pulsation_id)?;
        transaction.commit()?;
        Ok(())
    }

    /// Whether a different CLI could plausibly succeed where this one failed.
    /// Host-side faults (bad configuration, our own storage, a cancelled run)
    /// travel with the host, so switching backend only wastes another call.
    /// The gated backend this job is about to launch, when it sits above its validated ceiling
    /// and the developer has not validated it yet. `None` for API transports, overridden
    /// pulsations and every backend that does not gate.
    async fn cli_validation_gate(
        &self,
        job: &Job,
    ) -> Result<Option<crate::terminals::UnvalidatedCliVersion>, PersistenceServiceError> {
        let Some(stage) = Self::stage_from_job_kind(&job.kind) else {
            return Ok(None);
        };
        let role = AgentRole::for_stage(stage.as_db_value());
        let planned = {
            let connection = self.connection()?;
            Self::planned_transport(&connection, job, role.as_str())?
        };
        let Some((transport, _)) = planned else {
            return Ok(None);
        };
        let Some(backend) = transport.strip_prefix("cli:").and_then(CliBackend::parse) else {
            return Ok(None);
        };
        if !backend.gates_unvalidated_versions() {
            return Ok(None);
        }
        let terminals = self
            .terminal_service
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let Some(terminals) = terminals else {
            return Ok(None);
        };
        terminals
            .unvalidated_newer_version(backend)
            .await
            .map_err(|error| PersistenceServiceError::Cli {
                code: error.code().to_string(),
                message: error.to_string(),
            })
    }

    /// ADR-028 pre-flight: the plan window of the CLI this job is about to launch is nearly used
    /// up, or projected to run out before the stage typically finishes. Advisory only: the job
    /// parks on the CLI fallback checkpoint and the developer continues, switches to the API or
    /// cancels; nothing switches on its own. `None` for API transports, stale or missing quota
    /// data, and when the developer already chose to continue on this CLI in the current window.
    async fn quota_preflight_gate(
        &self,
        job: &Job,
    ) -> Result<Option<crate::quota::QuotaPreflight>, PersistenceServiceError> {
        let Some(stage) = Self::stage_from_job_kind(&job.kind) else {
            return Ok(None);
        };
        let Some(pulsation_id) = job.pulsation_id.as_deref() else {
            return Ok(None);
        };
        let role = AgentRole::for_stage(stage.as_db_value());
        let (planned, project_id) = {
            let connection = self.connection()?;
            let project_id: Option<String> = connection
                .query_row(
                    "SELECT project_id FROM pulsations WHERE id=?1",
                    [pulsation_id],
                    |row| row.get(0),
                )
                .optional()?;
            (
                Self::planned_transport(&connection, job, role.as_str())?,
                project_id,
            )
        };
        let Some((transport, _)) = planned else {
            return Ok(None);
        };
        let Some(backend) = transport.strip_prefix("cli:").and_then(CliBackend::parse) else {
            return Ok(None);
        };
        let terminals = self
            .terminal_service
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let Some(terminals) = terminals else {
            return Ok(None);
        };
        let expected = match &project_id {
            Some(project_id) => self.typical_stage_duration(project_id, role)?,
            None => None,
        };
        let Some(finding) = terminals.quota_meter().preflight(backend, expected) else {
            return Ok(None);
        };
        let since = finding.window.starts_at().map(|start| start.to_rfc3339());
        let continued: bool = self.connection()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM cli_fallback_requests
               WHERE pulsation_id=?1 AND agent_role=?2 AND error_code=?3 AND decision='retry_cli'
                 AND (?4 IS NULL OR julianday(resolved_at)>=julianday(?4)))",
            params![pulsation_id, role.as_str(), CLI_QUOTA_LOW, since],
            |row| row.get(0),
        )?;
        Ok((!continued).then_some(finding))
    }

    /// Median duration of the role's last completed CLI runs in this project, if any.
    fn typical_stage_duration(
        &self,
        project_id: &str,
        role: AgentRole,
    ) -> Result<Option<chrono::Duration>, PersistenceServiceError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT (julianday(finished_at)-julianday(started_at))*86400.0 FROM terminal_runs
             WHERE project_id=?1 AND agent_role=?2 AND status='completed'
               AND started_at IS NOT NULL AND finished_at IS NOT NULL
             ORDER BY finished_at DESC LIMIT 9",
        )?;
        let mut seconds = statement
            .query_map(params![project_id, role.as_str()], |row| {
                row.get::<_, Option<f64>>(0)
            })?
            .filter_map(|value| value.ok().flatten())
            .filter(|value| value.is_finite() && *value > 0.0)
            .collect::<Vec<_>>();
        if seconds.is_empty() {
            return Ok(None);
        }
        seconds.sort_by(f64::total_cmp);
        Ok(Some(chrono::Duration::seconds(
            seconds[seconds.len() / 2] as i64,
        )))
    }

    /// Runs the contract probe for the CLI behind a `cli_version_unvalidated` (or
    /// `cli_validation_failed`) fallback. A passing probe resumes the pulsation as `RetryCli`
    /// would; a failing one rewrites the fallback as `cli_validation_failed`, which leaves the
    /// developer the two remaining decisions (switch provider, abort).
    pub async fn validate_cli_fallback(
        &self,
        request: ValidateCliFallbackRequest,
    ) -> Result<ValidateCliFallbackResponse, PersistenceServiceError> {
        let checkpoint_id = request.checkpoint_id.trim().to_string();
        if checkpoint_id.is_empty() {
            return Err(PersistenceServiceError::Validation(
                "checkpointId cannot be empty.".to_string(),
            ));
        }
        let (agent_role, error_code, project_id) = {
            let connection = self.connection()?;
            connection
                .query_row(
                    "SELECT f.agent_role,f.error_code,p.project_id
                     FROM cli_fallback_requests f
                     JOIN checkpoints c ON c.id=f.checkpoint_id
                     JOIN pulsations p ON p.id=f.pulsation_id
                     WHERE c.id=?1 AND c.purpose='cli_fallback' AND c.status='waiting'
                       AND f.decision IS NULL AND p.status='checkpoint'
                       AND c.state_version=?2 AND p.state_version=?2",
                    params![checkpoint_id, request.expected_state_version],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .optional()?
                .ok_or_else(|| {
                    PersistenceServiceError::Validation(
                        "The fallback checkpoint is no longer waiting for this decision."
                            .to_string(),
                    )
                })?
        };
        if error_code != CLI_VERSION_UNVALIDATED && error_code != CLI_VALIDATION_FAILED {
            return Err(PersistenceServiceError::Validation(
                "This fallback is not a CLI version validation.".to_string(),
            ));
        }
        let backend = {
            let connection = self.connection()?;
            let raw: Option<String> = connection
                .query_row(
                    "SELECT backend FROM cli_agent_configs WHERE project_id=?1 AND agent_role=?2",
                    params![project_id, agent_role],
                    |row| row.get(0),
                )
                .optional()?;
            raw.as_deref().and_then(CliBackend::parse).ok_or_else(|| {
                PersistenceServiceError::Validation(
                    "This fallback's agent is not set up for a CLI.".to_string(),
                )
            })?
        };
        let terminals = self
            .terminal_service
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .ok_or_else(|| {
                PersistenceServiceError::Validation(
                    "The terminal service is unavailable.".to_string(),
                )
            })?;
        let report = terminals
            .validate_cli_contract(backend)
            .await
            .map_err(|error| PersistenceServiceError::Cli {
                code: error.code().to_string(),
                message: error.to_string(),
            })?;
        if report.validated {
            let resolved = self.resolve_cli_fallback(ResolveCliFallbackRequest {
                checkpoint_id,
                expected_state_version: request.expected_state_version,
                decision: CliFallbackDecision::RetryCli,
            })?;
            return Ok(ValidateCliFallbackResponse {
                report,
                pulsation: Some(resolved.pulsation),
            });
        }
        self.connection()?.execute(
            "UPDATE cli_fallback_requests SET error_code=?2, diagnostic=?3, updated_at=?4
             WHERE checkpoint_id=?1 AND decision IS NULL",
            params![
                checkpoint_id,
                CLI_VALIDATION_FAILED,
                report.diagnostic,
                Self::now_iso()
            ],
        )?;
        Ok(ValidateCliFallbackResponse {
            report,
            pulsation: None,
        })
    }

    fn backend_switch_can_help(error_code: &str) -> bool {
        // `cli_version_unvalidated` is deliberately absent: the developer asked to decide
        // about updated CLIs, so the host never elects another backend on its own for it.
        matches!(
            error_code,
            "cli_process_failed"
                | "cli_timeout"
                | "cli_inactivity_timeout"
                | "cli_auth_required"
                | "cli_version_unsupported"
                | "cli_rate_limited"
                | "cli_not_found"
                | "cli_output_limit"
                | "cli_output_invalid"
        )
    }

    /// The first healthy backend this pulsation has not already burned on the
    /// role, in the fixed order availability reports. `None` means the ladder
    /// is exhausted and only a human can decide what happens next.
    fn elect_backend(available: &[CliBackend], spent: &[String]) -> Option<CliBackend> {
        available
            .iter()
            .copied()
            .find(|candidate| !spent.iter().any(|used| used == candidate.as_str()))
    }

    /// Elects the next healthy CLI backend for a role whose current one just
    /// failed, records the burned one, and requeues the job to run on it.
    ///
    /// Returns `false` when nothing is left to try, which is when the human
    /// checkpoint is the right answer. Before this existed, the first CLI fault
    /// always stopped the pipeline on a question the host could answer itself:
    /// in the reference run the developer eventually fixed the Integrator by
    /// editing project settings by hand, after five failed attempts and three
    /// checkpoints on a backend that could not produce the answer.
    async fn elect_next_backend(
        &self,
        job: &Job,
        error_code: &str,
        diagnostic: &str,
    ) -> Result<bool, PersistenceServiceError> {
        if !Self::backend_switch_can_help(error_code) {
            return Ok(false);
        }
        let Some(pulsation_id) = job.pulsation_id.clone() else {
            return Ok(false);
        };
        let Some(stage) = Self::stage_from_job_kind(&job.kind) else {
            return Ok(false);
        };
        let role = AgentRole::for_stage(stage.as_db_value());
        let terminals = self
            .terminal_service
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let Some(terminals) = terminals else {
            return Ok(false);
        };
        let available = terminals.available_backends().await;
        if available.is_empty() {
            return Ok(false);
        }

        let mut connection = self.connection()?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current =
            Self::find_pulsation_by_id(&transaction, &pulsation_id)?.ok_or_else(|| {
                PersistenceServiceError::NotFound("The job's pulsation was not found.".to_string())
            })?;
        if current.status != PulsationStatus::Running || current.stage != stage {
            transaction.commit()?;
            return Ok(false);
        }
        // The backend that just ran, read from the process that ran it rather
        // than from configuration, which an override may already have changed.
        let burned: Option<String> = transaction
            .query_row(
                "SELECT backend FROM terminal_runs WHERE pulsation_id=?1 AND agent_role=?2 ORDER BY created_at DESC LIMIT 1",
                params![pulsation_id, role.as_str()],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        if let Some(burned) = burned.as_deref() {
            transaction.execute(
                "INSERT INTO cli_backend_attempts(pulsation_id,agent_role,backend,error_code,diagnostic,created_at) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(pulsation_id,agent_role,backend) DO UPDATE SET error_code=excluded.error_code,diagnostic=excluded.diagnostic",
                params![pulsation_id, role.as_str(), burned, error_code, diagnostic, Self::now_iso()],
            )?;
        }
        let mut spent: Vec<String> = transaction
            .prepare(
                "SELECT backend FROM cli_backend_attempts WHERE pulsation_id=?1 AND agent_role=?2",
            )?
            .query_map(params![pulsation_id, role.as_str()], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        if let Some(burned) = burned {
            spent.push(burned);
        }
        let Some(elected) = Self::elect_backend(&available, &spent) else {
            transaction.commit()?;
            return Ok(false);
        };

        transaction.execute(
            "INSERT INTO pulse_execution_overrides(pulsation_id,agent_role,execution_mode,backend,source_checkpoint_id,created_at) VALUES(?1,?2,'cli_process',?3,NULL,?4) ON CONFLICT(pulsation_id,agent_role) DO UPDATE SET execution_mode='cli_process',backend=excluded.backend,source_checkpoint_id=NULL",
            params![pulsation_id, role.as_str(), elected.as_str(), Self::now_iso()],
        )?;
        let message = format!(
            "{} failed ({error_code}); the stage will be retried on {}.",
            spent
                .last()
                .map(String::as_str)
                .unwrap_or("A CLI configurada"),
            elected.label()
        );
        Self::transition_pulsation(
            &transaction,
            &current,
            PulsationStatus::Planning,
            stage,
            "cli_backend_elected",
            Some(&message),
        )?;
        let changed = transaction.execute(
            "UPDATE jobs SET status='pending',generation=generation+1,available_at=NULL,last_error=?2,updated_at=?3,\
             state_version=(SELECT state_version FROM pulsations WHERE id=?4) WHERE id=?1 AND status='running'",
            params![job.id, diagnostic, Self::now_iso(), pulsation_id],
        )?;
        if changed != 1 {
            return Err(PersistenceServiceError::Validation(
                "The job was no longer running when the next backend was elected; the pulsation may have been cancelled meanwhile.".to_string(),
            ));
        }
        transaction.commit()?;
        self.wake_scheduler();
        Ok(true)
    }

    /// Explicit, human-triggered undo for `elect_next_backend`'s automatic
    /// failover: removes the pinned backend override for a pulsation's
    /// current stage so the next job pickup falls back to the project's
    /// configured default backend (see `execute_llm_request` in
    /// terminals.rs, which only overrides `config.backend` when
    /// `backend_override` returns `Some`). Deliberately never automatic --
    /// mirrors the ADR-010 principle that switching execution transport is
    /// a human decision, not something a background refresh should trigger.
    pub fn clear_execution_override_and_retry(
        &self,
        request: ClearExecutionOverrideRequest,
    ) -> Result<ClearExecutionOverrideResponse, PersistenceServiceError> {
        let mut db = self.connection()?;
        let transaction = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

        let current = Self::find_pulsation_by_id(&transaction, &request.pulsation_id)?
            .ok_or_else(|| PersistenceServiceError::NotFound("Pulsation not found.".to_string()))?;

        let has_override: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM pulse_execution_overrides WHERE pulsation_id=?1 AND agent_role=?2 AND execution_mode='cli_process')",
            params![request.pulsation_id, request.agent_role.as_str()],
            |row| row.get(0),
        )?;
        if !has_override {
            transaction.commit()?;
            return Ok(ClearExecutionOverrideResponse {
                cleared: false,
                retried: false,
            });
        }

        let stage_value = current.stage.as_db_value();
        let job_status: Option<String> = transaction
            .query_row(
                "SELECT status FROM jobs
                 WHERE pulsation_id=?1
                   AND (kind='run_stage::'||?2 OR (?2='SPECIFICATION' AND kind='run_stage::Specification'))
                 ORDER BY created_at DESC LIMIT 1",
                params![request.pulsation_id, stage_value],
                |row| row.get(0),
            )
            .optional()?;

        match job_status.as_deref() {
            Some("pending") => {
                let now = Self::now_iso();
                transaction.execute(
                    "DELETE FROM pulse_execution_overrides WHERE pulsation_id=?1 AND agent_role=?2 AND execution_mode='cli_process'",
                    params![request.pulsation_id, request.agent_role.as_str()],
                )?;
                // Defensive realignment: a no-op after the elect_next_backend
                // state_version fix, cheap insurance against residual drift.
                transaction.execute(
                    "UPDATE jobs SET available_at=NULL, updated_at=?2,
                        state_version=(SELECT state_version FROM pulsations WHERE id=?1)
                     WHERE pulsation_id=?1 AND status='pending'
                       AND (kind='run_stage::'||?3 OR (?3='SPECIFICATION' AND kind='run_stage::Specification'))",
                    params![request.pulsation_id, now, stage_value],
                )?;
                transaction.execute(
                    "INSERT INTO pulse_events (
                       id, pulsation_id, event_type, from_status, to_status, from_stage, to_stage, message, created_at
                     ) VALUES (?1, ?2, 'cli_backend_override_cleared', ?3, ?3, ?4, ?4, ?5, ?6)",
                    params![
                        Uuid::new_v4().to_string(),
                        request.pulsation_id,
                        current.status.as_db_value(),
                        stage_value,
                        "Backend override cleared manually; the stage will be retried on the project's default backend.",
                        now,
                    ],
                )?;
                transaction.commit()?;
                self.wake_scheduler();
                Ok(ClearExecutionOverrideResponse {
                    cleared: true,
                    retried: true,
                })
            }
            Some(other) => Err(PersistenceServiceError::Validation(format!(
                "The override cannot be cleared now: this stage's job is '{other}'. Cancel the pulsation to act immediately."
            ))),
            None => {
                // No job matches the current stage anymore -- the situation
                // already moved on. Clearing the stale row is still safe
                // housekeeping.
                transaction.execute(
                    "DELETE FROM pulse_execution_overrides WHERE pulsation_id=?1 AND agent_role=?2 AND execution_mode='cli_process'",
                    params![request.pulsation_id, request.agent_role.as_str()],
                )?;
                transaction.commit()?;
                Ok(ClearExecutionOverrideResponse {
                    cleared: true,
                    retried: false,
                })
            }
        }
    }

    fn open_cli_fallback(
        &self,
        job: &Job,
        agent_run_id: &str,
        error_code: &str,
        diagnostic: &str,
    ) -> Result<(), PersistenceServiceError> {
        let pulsation_id = job.pulsation_id.as_deref().ok_or_else(|| {
            PersistenceServiceError::Validation(
                "CLI failure without a pulsation reference.".to_string(),
            )
        })?;
        let stage = Self::stage_from_job_kind(&job.kind).ok_or_else(|| {
            PersistenceServiceError::Validation(
                "CLI failure attached to a job that is not a stage job.".to_string(),
            )
        })?;
        let role = AgentRole::for_stage(stage.as_db_value());
        let mut connection = self.connection()?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current = Self::find_pulsation_by_id(&transaction, pulsation_id)?.ok_or_else(|| {
            PersistenceServiceError::NotFound("The job's pulsation was not found.".to_string())
        })?;

        if current.status != PulsationStatus::Running || current.stage != stage {
            transaction.commit()?;
            return Ok(());
        }
        let waiting: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM checkpoints WHERE pulsation_id=?1 AND status='waiting'",
            [pulsation_id],
            |row| row.get(0),
        )?;
        if waiting != 0 {
            return Err(PersistenceServiceError::Validation(
                "The pulsation already has a checkpoint waiting for a decision.".to_string(),
            ));
        }

        let updated = Self::transition_pulsation(
            &transaction,
            &current,
            PulsationStatus::Checkpoint,
            stage,
            "cli_fallback_requested",
            Some(if error_code == CLI_QUOTA_LOW {
                "The CLI's plan window is nearly used up; the stage waits for a decision before it runs."
            } else {
                "The CLI failed and needs an explicit decision before the API is used."
            }),
        )?;
        let state_version: u64 = transaction.query_row(
            "SELECT state_version FROM pulsations WHERE id=?1",
            [pulsation_id],
            |row| row.get(0),
        )?;
        let input_revision: u64 = transaction.query_row(
            "SELECT input_revision FROM pulsations WHERE id=?1",
            [pulsation_id],
            |row| row.get(0),
        )?;
        let generation: u64 = transaction.query_row(
            "SELECT generation FROM jobs WHERE id=?1",
            [&job.id],
            |row| row.get(0),
        )?;
        let terminal_run_id: Option<String> = transaction
            .query_row(
                "SELECT id FROM terminal_runs WHERE job_id=?1 ORDER BY created_at DESC LIMIT 1",
                [&job.id],
                |row| row.get(0),
            )
            .optional()?;
        let checkpoint_id = Uuid::new_v4().to_string();
        let now = Self::now_iso();
        transaction.execute(
            "INSERT INTO checkpoints(
               id,pulsation_id,stage,status,created_at,updated_at,kind,input_revision,
               state_version,producer_job_id,generation,purpose
             ) VALUES(?1,?2,?3,'waiting',?4,?4,'cli_fallback',?5,?6,?7,?8,'cli_fallback')",
            params![
                checkpoint_id,
                pulsation_id,
                stage.as_db_value(),
                now,
                input_revision,
                state_version,
                job.id,
                generation,
            ],
        )?;
        transaction.execute(
            "INSERT INTO cli_fallback_requests(
               checkpoint_id,pulsation_id,agent_role,terminal_run_id,error_code,
               diagnostic,created_at,updated_at
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?7)",
            params![
                checkpoint_id,
                pulsation_id,
                role.as_str(),
                terminal_run_id,
                error_code,
                diagnostic,
                now,
            ],
        )?;
        transaction.execute(
            "UPDATE jobs SET status='interrupted',last_error=?2,cancellation_reason=NULL,
               cancel_requested_at=NULL,available_at=NULL,finished_at=?3,updated_at=?3
             WHERE id=?1 AND status='running'",
            params![job.id, diagnostic, now],
        )?;
        transaction.execute(
            "UPDATE agent_runs SET status='awaiting_fallback',updated_at=?2 WHERE id=?1",
            params![agent_run_id, now],
        )?;
        if let Some(run_id) = terminal_run_id.as_deref() {
            let previous: Option<String> = transaction
                .query_row(
                    "SELECT status FROM terminal_runs WHERE id=?1",
                    [run_id],
                    |row| row.get(0),
                )
                .optional()?;
            transaction.execute(
                "UPDATE terminal_runs SET status='awaiting_fallback',error_code=?2,
                   error_message=?3,updated_at=?4 WHERE id=?1",
                params![run_id, error_code, diagnostic, now],
            )?;
            if previous.is_some_and(|status| status != "awaiting_fallback") {
                Self::record_terminal_run_status(&transaction, run_id, "awaiting_fallback")?;
            }
        }
        debug_assert_eq!(updated.status, PulsationStatus::Checkpoint);
        transaction.commit()?;
        Ok(())
    }

    /// Records a `purpose='stage_failure'` checkpoint inside an existing transaction.
    ///
    /// Returns `false` when the caller must keep today's terminal behaviour: the
    /// decision set collapsed to abort-only (a checkpoint that can only be aborted is
    /// a dead end), another checkpoint is already waiting, or the pulsation moved on.
    ///
    /// Deliberately does NOT call `mark_effects_for_review`, `enqueue_learning` or
    /// `release_safe_owner`. `mark_effects_for_review` sets `base_state='needs_review'`,
    /// which blocks `claim_next_pending_job` and would make Retry unexecutable, and the
    /// project owner must stay held while the pulsation is still resumable. All three
    /// run on Abort instead.
    pub(crate) fn record_stage_failure_checkpoint(
        transaction: &rusqlite::Transaction<'_>,
        current: &Pulsation,
        stage: PulsationStage,
        job_id: &str,
        agent_run_id: Option<&str>,
        class: crate::orchestration::FailureClass,
        error_code: &str,
        diagnostic: &str,
    ) -> Result<bool, PersistenceServiceError> {
        let Some(offered) = crate::orchestration::offered_decisions(class, stage) else {
            return Ok(false);
        };
        // `close_abandoned_claim` can reach here while the pulsation is still Planning:
        // setup fails before `execute_claimed_job` performs Planning -> Running.
        if !matches!(
            current.status,
            PulsationStatus::Running | PulsationStatus::Planning
        ) || current.stage != stage
        {
            return Ok(false);
        }
        let waiting: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM checkpoints WHERE pulsation_id=?1 AND status='waiting'",
            [&current.id],
            |row| row.get(0),
        )?;
        if waiting != 0 {
            // Never error out here. `open_cli_fallback` returns Err without committing,
            // which leaves the job `running` with nobody left to collect it.
            return Ok(false);
        }

        Self::transition_pulsation(
            transaction,
            current,
            PulsationStatus::Checkpoint,
            stage,
            "stage_failure_opened",
            Some("The stage failed and waits for an explicit decision before closing."),
        )?;
        let (state_version, input_revision): (u64, u64) = transaction.query_row(
            "SELECT state_version,input_revision FROM pulsations WHERE id=?1",
            [&current.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let generation: u64 =
            transaction.query_row("SELECT generation FROM jobs WHERE id=?1", [job_id], |row| {
                row.get(0)
            })?;
        let checkpoint_id = Uuid::new_v4().to_string();
        let now = Self::now_iso();
        transaction.execute(
            "INSERT INTO checkpoints(
               id,pulsation_id,stage,status,created_at,updated_at,kind,input_revision,
               state_version,producer_job_id,generation,purpose
             ) VALUES(?1,?2,?3,'waiting',?4,?4,'stage_failure',?5,?6,?7,?8,'stage_failure')",
            params![
                checkpoint_id,
                current.id,
                stage.as_db_value(),
                now,
                input_revision,
                state_version,
                job_id,
                generation,
            ],
        )?;
        transaction.execute(
            "INSERT INTO stage_failure_requests(
               checkpoint_id,pulsation_id,job_id,stage,failure_class,allow_retry,allow_revision,
               revision_stage,error_code,diagnostic,agent_run_id,created_at,updated_at
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?12)",
            params![
                checkpoint_id,
                current.id,
                job_id,
                stage.as_db_value(),
                class.as_db_value(),
                offered.allow_retry as i64,
                offered.allow_revision as i64,
                offered.revision_stage.map(PulsationStage::as_db_value),
                error_code,
                diagnostic,
                agent_run_id,
                now,
            ],
        )?;
        // The job is preserved for resumption, not failed. Leaving it `running` would
        // strand it: `close_abandoned_claim` no-ops once the pulsation is `checkpoint`.
        transaction.execute(
            "UPDATE jobs SET status='interrupted',last_error=?2,cancellation_reason=NULL,
               cancel_requested_at=NULL,available_at=NULL,finished_at=?3,updated_at=?3
             WHERE id=?1 AND status='running'",
            params![job_id, diagnostic, now],
        )?;
        if let Some(run_id) = agent_run_id {
            transaction.execute(
                "UPDATE agent_runs SET status='awaiting_decision',updated_at=?2 WHERE id=?1",
                params![run_id, now],
            )?;
        }
        Ok(true)
    }

    /// Transactional wrapper for the failure sites that are not already inside one.
    fn open_stage_failure(
        &self,
        job: &Job,
        agent_run_id: Option<&str>,
        class: crate::orchestration::FailureClass,
        error: &PersistenceServiceError,
    ) -> Result<bool, PersistenceServiceError> {
        let Some(pulsation_id) = job.pulsation_id.as_deref() else {
            return Ok(false);
        };
        let Some(stage) = Self::stage_from_job_kind(&job.kind) else {
            return Ok(false);
        };
        let error_code = crate::orchestration::stage_failure_error_code(error);
        let mut connection = self.connection()?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current = Self::find_pulsation_by_id(&transaction, pulsation_id)?.ok_or_else(|| {
            PersistenceServiceError::NotFound("The job's pulsation was not found.".to_string())
        })?;
        let opened = Self::record_stage_failure_checkpoint(
            &transaction,
            &current,
            stage,
            &job.id,
            agent_run_id,
            class,
            &error_code,
            &error.to_string(),
        )?;
        transaction.commit()?;
        Ok(opened)
    }

    fn should_retry(
        &self,
        error: &PersistenceServiceError,
        attempts: i64,
        runtime_config: &AgentRuntimeConfig,
    ) -> bool {
        if attempts > runtime_config.max_retries as i64 {
            return false;
        }

        matches!(error, PersistenceServiceError::Retryable(_))
            || matches!(error, PersistenceServiceError::ModelOutputInvalid(_))
            || matches!(
                error,
                PersistenceServiceError::Llm(llm_error)
                    if matches!(
                        llm_error.kind,
                        LlmErrorKind::RateLimited
                            | LlmErrorKind::Timeout
                            | LlmErrorKind::Network
                            | LlmErrorKind::ProviderUnavailable
                            | LlmErrorKind::AllTargetsExhausted
                            // The router's own output-schema rejection is the
                            // same category of failure as ModelOutputInvalid,
                            // just on the API-direct path; its raw text is
                            // not persisted anywhere today, so this retries
                            // blind (no correction section) rather than being
                            // silently excluded.
                            | LlmErrorKind::OutputSchema
                    )
            )
    }

    /// Marks a freshly claimed stage job as started: the pulsation leaves `Planning` for
    /// `Running` and the job's fence follows the pulsation's state version. Shared by the
    /// execution path and the pre-dispatch CLI gate, which parks the job from the same state.
    fn begin_stage_job(
        &self,
        job: &Job,
        pulsation_id: &str,
    ) -> Result<(), PersistenceServiceError> {
        // Reserve the writer BEFORE reading the state. A deferred read-to-write upgrade
        // can return SQLITE_BUSY immediately when the background lane holds a reservation,
        // bypassing busy_timeout and incorrectly failing an otherwise valid stage.
        let mut connection = self.connection()?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current = Self::find_pulsation_by_id(&transaction, pulsation_id)?.ok_or_else(|| {
            PersistenceServiceError::NotFound("The job's pulsation was not found.".to_string())
        })?;

        let valid:bool=transaction.query_row("SELECT status='running' AND attempts=?2 AND input_revision=(SELECT input_revision FROM pulsations WHERE id=?3) AND cancellation_reason IS NULL FROM jobs WHERE id=?1",params![job.id,job.attempts,pulsation_id],|r|r.get(0))?;
        if !valid {
            return Err(PersistenceServiceError::Validation(
                "Run superseded before its inputs were read.".into(),
            ));
        }
        if current.status == PulsationStatus::Planning {
            let _ = Self::transition_pulsation(
                &transaction,
                &current,
                PulsationStatus::Running,
                current.stage,
                "job_started",
                Some("Job da etapa iniciado."),
            )?;
        }
        transaction.execute("UPDATE jobs SET state_version=(SELECT state_version FROM pulsations WHERE id=?2) WHERE id=?1 AND status='running'",params![job.id,pulsation_id])?;

        transaction.commit()?;
        Ok(())
    }

    /// The supervisor's two lanes share a Tokio task. SQLite can wait for the writer for up to
    /// the configured busy timeout, so this complete transaction belongs on the bounded blocking
    /// pool rather than delaying transport progress in either lane.
    async fn begin_stage_job_async(
        &self,
        job: &Job,
        pulsation_id: &str,
    ) -> Result<(), PersistenceServiceError> {
        let service = self.clone();
        let job = job.clone();
        let pulsation_id = pulsation_id.to_string();
        tokio::task::spawn_blocking(move || service.begin_stage_job(&job, &pulsation_id))
            .await
            .map_err(|error| PersistenceServiceError::Runtime(error.to_string()))?
    }

    async fn execute_claimed_job(
        &self,
        job: &Job,
        agent_run_id: &str,
        runtime_config: &AgentRuntimeConfig,
        cancellation: &CancellationToken,
    ) -> Result<(), PersistenceServiceError> {
        let Some(stage) = Self::stage_from_job_kind(&job.kind) else {
            return Err(PersistenceServiceError::Validation(format!(
                "Tipo de job desconhecido: {}",
                job.kind
            )));
        };

        let pulsation_id = job.pulsation_id.as_ref().ok_or_else(|| {
            PersistenceServiceError::Validation("Job without a pulsation reference.".to_string())
        })?;

        self.begin_stage_job_async(job, pulsation_id).await?;

        let current = {
            let connection = self.connection()?;
            let transaction = connection.unchecked_transaction()?;
            let current =
                Self::find_pulsation_by_id(&transaction, pulsation_id)?.ok_or_else(|| {
                    PersistenceServiceError::NotFound(
                        "The job's pulsation was not found.".to_string(),
                    )
                })?;

            if current.status != PulsationStatus::Running || current.stage != stage {
                transaction.commit()?;
                return Err(
                    if cancellation.is_cancelled() || self.job_cancellation_requested(&job.id)? {
                        PersistenceServiceError::Cancelled(
                            "Run cancelled before the stage started.".into(),
                        )
                    } else {
                        PersistenceServiceError::Validation(
                            "Run stale before the stage started.".into(),
                        )
                    },
                );
            }

            transaction.commit()?;
            current
        };

        let connection = self.connection()?;
        let project = Self::find_project_by_id(&connection, &current.project_id, true)?
            .ok_or_else(|| {
                PersistenceServiceError::NotFound(
                    "The pulsation's project was not found.".to_string(),
                )
            })?;

        let _provider_demand = self.llm_router.foreground_demand(
            &current.project_id,
            crate::llm::AgentRole::for_stage(stage.as_db_value()),
            stage.as_db_value(),
            runtime_config.provider.as_str(),
        )?;
        // Single branch point for pipeline v4 (ADR-022): composed stages run
        // under the host coordinator; v1-v3 keep the legacy path untouched.
        let pipeline_version: u8 = connection.query_row(
            "SELECT pipeline_version FROM pulsations WHERE id=?1",
            [&current.id],
            |row| row.get(0),
        )?;
        if crate::orchestration::PipelineVersion::from_db(pipeline_version)
            .is_some_and(|version| version.supports_composition())
        {
            drop(connection);
            return self
                .execute_composed_stage(
                    job,
                    agent_run_id,
                    stage,
                    &current,
                    &project,
                    runtime_config,
                    cancellation,
                )
                .await;
        }
        let mut handoff = self.load_stage_handoff(
            &Self::execution_fence(&connection, &job.id)?,
            stage,
            runtime_config.context_chars_limit,
        )?;
        if stage == PulsationStage::Specification {
            handoff = self
                .enrich_specification(handoff, runtime_config.context_chars_limit, cancellation)
                .await?;
        }
        let anticipatory: bool = connection.query_row(
            "SELECT job_class='background' FROM jobs WHERE id=?1",
            [&job.id],
            |r| r.get(0),
        )?;
        // Only a failed attempt for THIS job leaves a row here (see
        // `persist_failed_llm_output`/`persist_agent_run_output`); a retry
        // triggered by a transport failure (rate limit, timeout) where the
        // model never produced output naturally finds none, and
        // `previous_attempt` stays `None`.
        let previous_attempt = connection
            .prepare(
                "SELECT raw_output, validation_error FROM agent_run_outputs
                 WHERE job_id = ?1 AND validation_error IS NOT NULL
                 ORDER BY created_at DESC LIMIT 1",
            )?
            .query_row(params![job.id], |row| {
                Ok(crate::agent_runtime::PreviousAttempt {
                    raw_output: row.get(0)?,
                    error: row.get(1)?,
                })
            })
            .optional()?
            .map(|mut attempt| {
                attempt.raw_output = sanitized_observation_text(&attempt.raw_output);
                attempt.error = sanitized_observation_text(&attempt.error);
                attempt
            });
        let execution_input = StageExecutionInput {
            anticipatory,
            handoff: Some(handoff),
            job_id: job.id.clone(),
            pulsation_id: current.id.clone(),
            project_id: current.project_id.clone(),
            feature_name: current.feature_name.clone(),
            stage_name: stage.as_db_value().to_string(),
            workspace_path: project.workspace_path.clone(),
            runtime_db_path: self.db_path.to_string_lossy().to_string(),
            previous_attempt,
            agent_run_id: Some(agent_run_id.to_string()),
        };

        tokio::select! {
            _ = cancellation.cancelled() => {
                let trace = ContextRetrievalTrace::empty(runtime_config, "not_started");
                let connection = self.connection()?;
                Self::persist_context_retrieval_trace(&connection, agent_run_id, job, &trace)?;
                return Err(PersistenceServiceError::Cancelled(
                    "Run cancelled before the agent call.".to_string()
                ));
            }
            _ = sleep(Duration::from_millis(160)) => {}
        }

        // Never abandon an owned filesystem worker; the router bounds network attempts.
        let terminal_service = self
            .terminal_service
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let execution = match execute_stage_with_cli(
            self.embedding_gateway.as_ref(),
            self.llm_router.as_ref(),
            terminal_service.as_ref(),
            &self.document_cache,
            runtime_config,
            &execution_input,
            cancellation,
        )
        .await
        {
            Ok(execution) => execution,
            Err(failure) => {
                if let Some(trace) = failure.context_trace.as_ref() {
                    let connection = self.connection()?;
                    Self::persist_context_retrieval_trace(&connection, agent_run_id, job, trace)?;
                }
                let error = PersistenceServiceError::from(failure.error);
                if let (PersistenceServiceError::ModelOutputInvalid(message), Some(output)) =
                    (&error, failure.failed_output.as_ref())
                {
                    let connection = self.connection()?;
                    Self::persist_failed_llm_output(
                        &connection,
                        agent_run_id,
                        job,
                        output,
                        message,
                    )?;
                }
                return Err(error);
            }
        };

        if cancellation.is_cancelled() {
            let connection = self.connection()?;
            Self::persist_context_retrieval_trace(
                &connection,
                agent_run_id,
                job,
                &execution.context_trace,
            )?;
            return Err(PersistenceServiceError::Cancelled(
                "Run cancelled after the agent answered.".into(),
            ));
        }
        if let Err(error) = self.accept_stage_result(
            job,
            agent_run_id,
            execution_input.handoff.as_ref().expect("worker handoff"),
            &execution,
            Path::new(&project.workspace_path),
        ) {
            let connection = self.connection()?;
            Self::persist_context_retrieval_trace(
                &connection,
                agent_run_id,
                job,
                &execution.context_trace,
            )?;
            if let PersistenceServiceError::ModelOutputInvalid(message) = &error {
                Self::persist_agent_run_output(
                    &connection,
                    agent_run_id,
                    job,
                    &execution,
                    Some(message.as_str()),
                )?;
            }
            return Err(error);
        }
        self.wake_scheduler();
        Ok(())
    }

    async fn process_claimed_job(
        &self,
        job: &Job,
        cancellation: &CancellationToken,
    ) -> Result<(), PersistenceServiceError> {
        let runtime_config = self.load_runtime_config()?;
        let agent_run_id = self.start_agent_run_for_job(job, &runtime_config)?;
        match self.job_cancellation_requested(&job.id) {
            Ok(true) => cancellation.cancel(),
            Ok(false) => {}
            Err(error) => {
                self.unregister_job_cancellation(&job.id);
                return Err(error);
            }
        }

        // A gated CLI (Gemini) that updated itself past its validated ceiling parks the job on
        // a fallback checkpoint before anything is launched, on both the legacy and the
        // composed (v4) paths: the developer validates the CLI, switches provider or aborts.
        if let Some(unvalidated) = self.cli_validation_gate(job).await? {
            if let Some(pulsation_id) = job.pulsation_id.as_deref() {
                self.begin_stage_job_async(job, pulsation_id).await?;
            }
            self.open_cli_fallback(
                job,
                &agent_run_id,
                CLI_VERSION_UNVALIDATED,
                &unvalidated.diagnostic(),
            )?;
            self.unregister_job_cancellation(&job.id);
            self.wake_scheduler();
            return Ok(());
        }

        // ADR-028: a CLI that is low on its plan window asks before the stage runs. Advisory -
        // the developer decides on the same checkpoint; nothing switches provider by itself.
        if let Some(finding) = self.quota_preflight_gate(job).await? {
            if let Some(pulsation_id) = job.pulsation_id.as_deref() {
                self.begin_stage_job_async(job, pulsation_id).await?;
            }
            self.open_cli_fallback(job, &agent_run_id, CLI_QUOTA_LOW, &finding.message())?;
            self.unregister_job_cancellation(&job.id);
            self.wake_scheduler();
            return Ok(());
        }

        // Context filesystem workers are bounded and awaited, never abandoned on timeout.
        let execution_result = self
            .execute_claimed_job(job, &agent_run_id, &runtime_config, cancellation)
            .await;

        self.unregister_job_cancellation(&job.id);

        match execution_result {
            Ok(()) => {}
            Err(error) => {
                // An accepted result is durable even if publication/ACK failed afterward.
                let accepted: bool = self.connection()?.query_row(
                    "SELECT EXISTS(SELECT 1 FROM stage_acceptances WHERE job_id=?1)",
                    [&job.id],
                    |r| r.get(0),
                )?;
                if accepted {
                    return Err(error);
                }
                let reason: Option<String> = self.connection()?.query_row(
                    "SELECT cancellation_reason FROM jobs WHERE id=?1",
                    [&job.id],
                    |r| r.get(0),
                )?;
                if reason.as_deref() == Some("user_cancel") {
                    self.finish_agent_run(&agent_run_id, "interrupted")?;
                    return Err(PersistenceServiceError::Cancelled(
                        "Pulsation cancelled by the user.".into(),
                    ));
                }
                if cancellation.is_cancelled()
                    || matches!(
                        reason.as_deref(),
                        Some("shutdown" | "preempted" | "superseded")
                    )
                {
                    self.finish_agent_run(&agent_run_id, "interrupted")?;
                    self.connection()?.execute("UPDATE jobs SET status='interrupted',cancellation_reason=COALESCE(cancellation_reason,'shutdown'),updated_at=?2 WHERE id=?1 AND status='running'",params![job.id,Self::now_iso()])?;
                    return Err(error);
                }
                let current_execution:bool=self.connection()?.query_row("SELECT j.status='running' AND j.attempts=?2 AND j.state_version=p.state_version AND j.input_revision=p.input_revision FROM jobs j JOIN pulsations p ON p.id=j.pulsation_id WHERE j.id=?1",params![job.id,job.attempts],|r|r.get(0))?;
                if !current_execution {
                    self.finish_agent_run(&agent_run_id, "interrupted")?;
                    self.connection()?.execute("UPDATE jobs SET status='failed',cancellation_reason='superseded' WHERE id=?1 AND status='running' AND attempts=?2",params![job.id,job.attempts])?;
                    return Err(error);
                }
                let can_retry = self.should_retry(&error, job.attempts, &runtime_config);
                if can_retry {
                    self.transition_for_failed_job(job, &error, true)?;
                    self.finish_agent_run(&agent_run_id, "retrying")?;
                    self.requeue_job(&job.id, job.attempts, &error.to_string())?;
                    return Ok(());
                }

                // A truncated answer arrives as a CLI fault but is not a
                // transport problem, so it must not be offered the CLI
                // fallback's "retry the CLI": the same prompt against the same
                // budget stops at the same place. It falls through to the
                // stage-failure checkpoint, which offers a revision instead.
                // A run the host stopped for spending past its output ceiling
                // is the same: another backend would be billed for the same
                // overrun.
                let truncated = matches!(
                    &error,
                    PersistenceServiceError::Cli { code, .. }
                        if matches!(code.as_str(), "cli_output_truncated" | "cli_output_budget_exceeded")
                );
                if !truncated && let PersistenceServiceError::Cli { code, message } = &error {
                    // Ask the human only once the host has run out of backends
                    // to try on its own.
                    if self.elect_next_backend(job, code, message).await? {
                        self.finish_agent_run(&agent_run_id, "retrying")?;
                        return Ok(());
                    }
                    self.open_cli_fallback(job, &agent_run_id, code, message)?;
                    self.wake_scheduler();
                    return Ok(());
                }

                // The automatic budget is spent, but the job and its fence are intact.
                // Park on a decision instead of burning the whole pipeline. Falls
                // through to the terminal path when the class offers nothing but abort.
                if let Some(class) = crate::orchestration::classify_stage_failure(&error)
                    && self.open_stage_failure(job, Some(&agent_run_id), class, &error)?
                {
                    self.wake_scheduler();
                    return Ok(());
                }

                self.transition_for_failed_job(job, &error, false)?;

                let run_status = if matches!(error, PersistenceServiceError::Cancelled(_)) {
                    "interrupted"
                } else {
                    "failed"
                };
                let job_status = if matches!(error, PersistenceServiceError::Cancelled(_)) {
                    JobStatus::Interrupted
                } else {
                    JobStatus::Failed
                };

                self.finish_agent_run(&agent_run_id, run_status)?;
                self.finalize_job(&job.id, job_status, Some(error.to_string()))?;
                return Err(error);
            }
        }

        Ok(())
    }
}

fn strip_frontmatter_for_embedding(content: &str) -> String {
    let normalized = content.replace('\r', "");
    if !normalized.starts_with("---\n") {
        return normalized;
    }

    let mut lines = normalized.lines();
    let Some(first) = lines.next() else {
        return normalized;
    };
    if first.trim() != "---" {
        return normalized;
    }

    let mut body_started = false;
    let mut body_lines = Vec::new();
    for line in lines {
        if !body_started {
            if line.trim() == "---" {
                body_started = true;
            }
            continue;
        }
        body_lines.push(line);
    }

    if body_lines.is_empty() {
        normalized
    } else {
        body_lines.join("\n")
    }
}

fn is_unique_constraint(error: &SqlError) -> bool {
    match error {
        SqlError::SqliteFailure(result, _) => result.code == ErrorCode::ConstraintViolation,
        _ => false,
    }
}

fn brownfield_consent_key(project_id: &str) -> String {
    format!("brownfield.onboarding_consent.{project_id}")
}

#[cfg(test)]
#[path = "sqlite_runtime_tests.rs"]
mod sqlite_runtime_tests;

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

    use super::*;
    use crate::embedding_service::{
        EmbeddingError, EmbeddingFuture, ModelListFuture, ModelPullFuture,
    };
    use crate::llm::LlmRepository;

    fn prepare_service() -> (tempfile::TempDir, PersistenceService) {
        let root = tempfile::tempdir().expect("tempdir should be created");
        let service = PersistenceService::initialize(root.path())
            .expect("persistence service should initialize");
        (root, service)
    }

    /// A model-output validation failure (parse/schema/evidence) is worth a
    /// corrective retry - unlike other `Runtime`/`Validation` errors, which
    /// cover unrelated host-side checks a fresh model attempt could never
    /// fix. The router's own `OutputSchema` rejection on the API-direct path
    /// is the same category of failure and rides the same budget, blind
    /// (no raw output is persisted for that specific case).
    #[test]
    fn should_retry_covers_model_output_failures_within_the_retry_budget() {
        let (_root, service) = prepare_service();
        let config = AgentRuntimeConfig::default();
        assert_eq!(config.max_retries, 2);

        let model_output_invalid =
            PersistenceServiceError::ModelOutputInvalid("bad json".to_string());
        assert!(service.should_retry(&model_output_invalid, 1, &config));
        assert!(service.should_retry(&model_output_invalid, 2, &config));
        assert!(!service.should_retry(&model_output_invalid, 3, &config));

        let output_schema = PersistenceServiceError::Llm(LlmError::new(
            LlmErrorKind::OutputSchema,
            "router rejected output",
        ));
        assert!(service.should_retry(&output_schema, 1, &config));
        assert!(!service.should_retry(&output_schema, 3, &config));

        // A host-side error must not be silently made retryable by this change.
        let host_side = PersistenceServiceError::Validation("Sem pulsação".to_string());
        assert!(!service.should_retry(&host_side, 1, &config));
    }

    #[derive(Default)]
    struct FakeReadinessGateway {
        models: Mutex<Vec<String>>,
        list_error: Mutex<Option<String>>,
        embed_error: Mutex<Option<String>>,
    }

    impl EmbeddingGateway for FakeReadinessGateway {
        fn embed<'a>(&'a self, _model: &'a str, _prompt: &'a str) -> EmbeddingFuture<'a> {
            Box::pin(async move {
                match self.embed_error.lock().unwrap().clone() {
                    Some(error) => Err(EmbeddingError(error)),
                    None => Ok(vec![0.1, 0.2]),
                }
            })
        }

        fn list_models<'a>(&'a self) -> ModelListFuture<'a> {
            Box::pin(async move {
                match self.list_error.lock().unwrap().clone() {
                    Some(error) => Err(EmbeddingError(error)),
                    None => Ok(self.models.lock().unwrap().clone()),
                }
            })
        }

        fn pull_model<'a>(
            &'a self,
            model: &'a str,
            on_event: &'a (dyn Fn(ModelPullEvent) + Send + Sync),
        ) -> ModelPullFuture<'a> {
            Box::pin(async move {
                on_event(ModelPullEvent::Started {
                    model: model.to_string(),
                });
                on_event(ModelPullEvent::Completed {
                    model: model.to_string(),
                });
                Ok(())
            })
        }
    }

    #[derive(Default)]
    struct RecordingModelPullSink(Mutex<Vec<ModelPullEvent>>);

    impl ModelPullEventSink for RecordingModelPullSink {
        fn emit(&self, event: &ModelPullEvent) {
            self.0.lock().unwrap().push(event.clone());
        }
    }

    #[tokio::test]
    async fn pull_embedding_model_forwards_gateway_events_to_the_attached_sink() {
        let (_root, mut service) = prepare_service();
        service.embedding_gateway = Arc::new(FakeReadinessGateway::default());
        let sink = Arc::new(RecordingModelPullSink::default());
        service.attach_model_pull_event_sink(sink.clone());

        service
            .pull_embedding_model(Some("nomic-embed-text".into()))
            .await
            .unwrap();

        let events = sink.0.lock().unwrap();
        assert!(matches!(events[0], ModelPullEvent::Started { .. }));
        assert!(matches!(events[1], ModelPullEvent::Completed { .. }));
    }

    #[tokio::test]
    async fn readiness_report_flags_unreachable_ollama() {
        let (_root, mut service) = prepare_service();
        let gateway = Arc::new(FakeReadinessGateway::default());
        *gateway.list_error.lock().unwrap() = Some("connection refused".into());
        service.embedding_gateway = gateway;
        let report = service.check_embedding_readiness().await.unwrap();
        assert!(!report.ollama_reachable);
        assert!(!report.model_installed);
        assert!(!report.embedding_call_ok);
        assert!(report.error.is_some());
    }

    #[tokio::test]
    async fn readiness_report_flags_missing_model_without_calling_embed() {
        let (_root, mut service) = prepare_service();
        let gateway = Arc::new(FakeReadinessGateway::default());
        *gateway.models.lock().unwrap() = vec!["llama3:8b".into()];
        *gateway.embed_error.lock().unwrap() = Some("must not be reached".into());
        service.embedding_gateway = gateway;
        let report = service.check_embedding_readiness().await.unwrap();
        assert!(report.ollama_reachable);
        assert!(!report.model_installed);
        assert!(!report.embedding_call_ok);
        assert!(report.error.is_none());
    }

    #[tokio::test]
    async fn readiness_report_surfaces_embed_failure_once_model_is_installed() {
        let (_root, mut service) = prepare_service();
        let gateway = Arc::new(FakeReadinessGateway::default());
        *gateway.models.lock().unwrap() = vec!["nomic-embed-text:latest".into()];
        *gateway.embed_error.lock().unwrap() =
            Some("Ollama embedding falhou com status HTTP 500.".into());
        service.embedding_gateway = gateway;
        let report = service.check_embedding_readiness().await.unwrap();
        assert!(report.ollama_reachable);
        assert!(report.model_installed);
        assert!(!report.embedding_call_ok);
        assert_eq!(
            report.error.as_deref(),
            Some("Ollama embedding falhou com status HTTP 500.")
        );
    }

    #[tokio::test]
    async fn readiness_report_is_all_clear_once_installed_model_embeds() {
        let (_root, mut service) = prepare_service();
        let gateway = Arc::new(FakeReadinessGateway::default());
        *gateway.models.lock().unwrap() = vec!["nomic-embed-text:latest".into()];
        service.embedding_gateway = gateway;
        let report = service.check_embedding_readiness().await.unwrap();
        assert!(report.ollama_reachable);
        assert!(report.model_installed);
        assert!(report.embedding_call_ok);
        assert!(report.error.is_none());
    }

    fn spawn_http_response(
        status: &str,
        extra_headers: &str,
        body: &str,
    ) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("fake provider should bind");
        let address = listener
            .local_addr()
            .expect("fake provider address should resolve");
        let status = status.to_string();
        let headers = extra_headers.to_string();
        let body = body.to_string();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("fake provider should accept");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("read timeout should set");
            let mut request = [0_u8; 16 * 1024];
            let _ = stream.read(&mut request);
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .expect("fake response should write");
            stream.flush().expect("fake response should flush");
        });
        (format!("http://{address}"), handle)
    }

    fn spawn_delayed_sse() -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("fake provider should bind");
        let address = listener
            .local_addr()
            .expect("fake provider address should resolve");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("fake provider should accept");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("read timeout should set");
            let mut request = [0_u8; 16 * 1024];
            let _ = stream.read(&mut request);
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
                )
                .expect("stream headers should write");
            let event = "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n";
            let chunk = format!("{:X}\r\n{}\r\n", event.len(), event);
            stream
                .write_all(chunk.as_bytes())
                .expect("first SSE chunk should write");
            stream.flush().expect("first SSE chunk should flush");
            thread::sleep(Duration::from_millis(1_200));
        });
        (format!("http://{address}"), handle)
    }

    #[test]
    fn migrations_are_idempotent() {
        let (root, service) = prepare_service();
        let db_path = service.db_path.clone();
        assert!(db_path.exists());

        PersistenceService::initialize(root.path()).expect("migrations should rerun safely");
        let connection =
            Connection::open(db_path).expect("db should open after repeated migration");
        let tables: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'projects'",
                [],
                |row| row.get(0),
            )
            .expect("table query should work");

        assert_eq!(tables, 1);
    }

    #[test]
    fn v33_to_v34_preserves_canonical_executions_and_bindings() {
        let root = tempfile::tempdir().expect("tempdir should be created");
        let db_path = root.path().join("hard-vibe-code.db");
        let mut connection = Connection::open(&db_path).expect("v33 db should open");
        embedded::migrations::runner()
            .set_target(refinery::Target::Version(33))
            .run(&mut connection)
            .expect("v1-v33 should apply");
        let now = Utc::now().to_rfc3339();
        connection
            .execute(
                "INSERT INTO projects(id,name,workspace_path,created_at,updated_at)
             VALUES('project-v33','Project V33',?1,?2,?2)",
                params![root.path().to_string_lossy(), now],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO provider_routes(id,name,strategy,max_attempts,attempt_timeout_ms,
                    route_timeout_ms,enabled,version,created_at,updated_at)
             VALUES('route-v33','Route V33','priority',3,30000,45000,1,1,?1,?1)",
                params![now],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO provider_route_bindings(project_id,agent_role,stage,route_id,
                    enabled,version,created_at,updated_at)
             VALUES('project-v33','backend_agent','Code','route-v33',1,4,?1,?1)",
                params![now],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO llm_route_executions(id,generation_id,project_id,route_id,
                    agent_role,stage,strategy,mode,status,decision_json,started_at)
             VALUES('execution-v33','generation-v33','project-v33','route-v33',
                    'backend_agent','Code','priority','on','completed','{}',?1)",
                params![now],
            )
            .unwrap();
        drop(connection);

        let service = PersistenceService::initialize(root.path()).expect("v34 should upgrade");
        let db = service.connection().unwrap();
        let identity: (Option<String>, Option<String>) = db
            .query_row(
                "SELECT agent_role,free_form_slot_id FROM llm_route_executions
             WHERE id='execution-v33'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(identity, (Some("backend_agent".into()), None));
        let binding_version: u64 = db
            .query_row(
                "SELECT version FROM provider_route_bindings
             WHERE project_id='project-v33' AND agent_role='backend_agent' AND stage='Code'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(binding_version, 4);
        let violations: i64 = db
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(violations, 0);
    }

    #[test]
    fn free_form_route_bindings_are_project_scoped_versioned_and_fail_closed() {
        let (root, service) = prepare_service();
        let now = Utc::now().to_rfc3339();
        let db = service.connection().unwrap();
        for (id, path) in [
            ("project-a", root.path().join("a")),
            ("project-b", root.path().join("b")),
        ] {
            db.execute(
                "INSERT INTO projects(id,name,workspace_path,created_at,updated_at)
                 VALUES(?1,?1,?2,?3,?3)",
                params![id, path.to_string_lossy(), now],
            )
            .unwrap();
        }
        drop(db);
        let terminals = crate::terminals::TerminalRepository::new(service.db_path.clone());
        let slot_a = terminals
            .create_free_form_slot(crate::terminals::CreateFreeFormSlotRequest {
                project_id: "project-a".into(),
                label: "API A".into(),
            })
            .unwrap();
        let slot_b = terminals
            .create_free_form_slot(crate::terminals::CreateFreeFormSlotRequest {
                project_id: "project-b".into(),
                label: "API B".into(),
            })
            .unwrap();
        let target_a = AgentExecutionTarget::free_form(&slot_a.id);
        assert!(
            service
                .llm_repository
                .resolve_route_for_target("project-a", &target_a, "FreeForm")
                .is_err()
        );
        assert!(
            service
                .llm_repository
                .upsert_free_form_binding(UpsertFreeFormProviderRouteBindingRequest {
                    project_id: "project-a".into(),
                    slot_id: slot_b.id.clone(),
                    route_id: "legacy-default".into(),
                    enabled: Some(true),
                    version: None,
                })
                .is_err()
        );

        let binding = service
            .llm_repository
            .upsert_free_form_binding(UpsertFreeFormProviderRouteBindingRequest {
                project_id: "project-a".into(),
                slot_id: slot_a.id.clone(),
                route_id: "legacy-default".into(),
                enabled: Some(true),
                version: None,
            })
            .unwrap();
        assert_eq!(binding.version, 1);
        assert_eq!(
            service
                .llm_repository
                .list_free_form_bindings("project-a")
                .unwrap()
                .len(),
            1
        );
        assert!(
            service
                .llm_repository
                .list_free_form_bindings("project-b")
                .unwrap()
                .is_empty()
        );
        assert!(
            service
                .llm_repository
                .resolve_route_for_target("project-a", &target_a, "FreeForm")
                .is_ok()
        );

        let stale = service
            .llm_repository
            .upsert_free_form_binding(UpsertFreeFormProviderRouteBindingRequest {
                project_id: "project-a".into(),
                slot_id: slot_a.id,
                route_id: "legacy-default".into(),
                enabled: Some(false),
                version: Some(99),
            })
            .unwrap_err();
        assert_eq!(stale.kind, LlmErrorKind::RouteConflict);
    }

    #[test]
    fn v18_to_v19_is_additive_preserves_v1_learning_and_does_not_backfill_telemetry() {
        let root = tempfile::tempdir().expect("tempdir should be created");
        let db_path = root.path().join("hard-vibe-code.db");
        let mut connection = Connection::open(&db_path).expect("v18 db should open");
        embedded::migrations::runner()
            .set_target(refinery::Target::Version(18))
            .run(&mut connection)
            .expect("v1-v18 should apply");
        let now = Utc::now().to_rfc3339();
        connection
            .execute(
                "INSERT INTO projects(id,name,workspace_path,created_at,updated_at) VALUES('project','Project',?1,?2,?2)",
                params![root.path().to_string_lossy(),now],
            )
            .unwrap();
        connection.execute("INSERT INTO pulsations(id,project_id,feature_name,stage,status,created_at,updated_at,queue_sequence,pipeline_version) VALUES('pulse','project','Feature','Integrator','done',?1,?1,1,2)",[&now]).unwrap();
        connection.execute("INSERT INTO jobs(id,pulsation_id,kind,status,created_at,updated_at,project_id,idempotency_key) VALUES('learning-v1','pulse','learning_report','done',?1,?1,'project','learning-v1')",[&now]).unwrap();
        let v1 = r#"{"schema_version":1,"observations":[],"recommendations":[],"limitations":[]}"#;
        connection.execute("INSERT INTO learning_runs(job_id,pulsation_id,integration_epoch,report_version,state,report_json,created_at,updated_at) VALUES('learning-v1','pulse',1,1,'published',?1,?2,?2)",params![v1,now]).unwrap();
        drop(connection);

        let service = PersistenceService::initialize(root.path()).expect("v19 should upgrade");
        let db = service.connection().unwrap();
        assert_eq!(
            db.query_row(
                "SELECT report_json FROM learning_runs WHERE job_id='learning-v1'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            v1
        );
        for table in [
            "context_retrieval_runs",
            "context_retrieval_items",
            "learning_context_assessments",
            "learning_graph_proposals",
        ] {
            let count: i64 = db
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, 0, "V19 não faz backfill de {table}");
        }
        assert_eq!(
            db.query_row("SELECT value FROM runtime_settings WHERE key='orchestration.learning_refinement_mode'", [], |row| row.get::<_,String>(0)).unwrap(),
            "observe"
        );
    }

    /// V27 rebuilds `checkpoints` to widen both CHECK constraints, and SQLite cannot
    /// alter a CHECK in place. Four tables reference this one, so the rebuild has to
    /// survive DROP+RENAME with their foreign keys intact and no rows lost.
    #[test]
    fn v26_to_v27_rebuilds_checkpoints_without_losing_rows_or_references() {
        let root = tempfile::tempdir().expect("tempdir should be created");
        let db_path = root.path().join("hard-vibe-code.db");
        let mut connection = Connection::open(&db_path).expect("v26 db should open");
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .expect("foreign keys should enable");
        embedded::migrations::runner()
            .set_target(refinery::Target::Version(26))
            .run(&mut connection)
            .expect("v1-v26 should apply");
        let now = Utc::now().to_rfc3339();
        connection.execute("INSERT INTO projects(id,name,workspace_path,created_at,updated_at) VALUES('project-v26','Project V26',?1,?2,?2)",params![root.path().to_string_lossy(),now]).unwrap();
        connection.execute("INSERT INTO pulsations(id,project_id,feature_name,stage,status,created_at,updated_at,queue_sequence,pipeline_version,state_version) VALUES('pulse-v26','project-v26','Feature','Integrator','checkpoint',?1,?1,1,2,7)",[&now]).unwrap();
        connection.execute("INSERT INTO jobs(id,pulsation_id,kind,status,created_at,updated_at,project_id,input_revision,generation,state_version,idempotency_key) VALUES('job-v26','pulse-v26','run_stage::Integrator','interrupted',?1,?1,'project-v26',1,3,7,'job-v26')",[&now]).unwrap();
        // One checkpoint of each purpose V26 knows about. The cli_fallback row carries
        // the mismatched kind V20 forced on it, which V27 realigns.
        connection.execute("INSERT INTO checkpoints(id,pulsation_id,stage,status,created_at,updated_at,kind,input_revision,state_version,producer_job_id,generation,purpose) VALUES('review-v26','pulse-v26','Integrator','approved',?1,?1,'artifact_review',1,7,'job-v26',3,'artifact_review')",[&now]).unwrap();
        connection.execute("INSERT INTO checkpoints(id,pulsation_id,stage,status,created_at,updated_at,kind,input_revision,state_version,producer_job_id,generation,purpose) VALUES('clarify-v26','pulse-v26','SPECIFICATION','approved',?1,?1,'clarification',1,7,'job-v26',4,'clarification')",[&now]).unwrap();
        connection.execute("INSERT INTO checkpoints(id,pulsation_id,stage,status,created_at,updated_at,kind,input_revision,state_version,producer_job_id,generation,purpose) VALUES('fallback-v26','pulse-v26','Integrator','waiting',?1,?1,'artifact_review',1,7,'job-v26',5,'cli_fallback')",[&now]).unwrap();
        connection.execute("INSERT INTO cli_fallback_requests(checkpoint_id,pulsation_id,agent_role,error_code,diagnostic,created_at,updated_at) VALUES('fallback-v26','pulse-v26','integrator','cli_exit','boom',?1,?1)",[&now]).unwrap();
        connection.execute("INSERT INTO pulse_execution_overrides(pulsation_id,agent_role,execution_mode,source_checkpoint_id,created_at) VALUES('pulse-v26','integrator','api_direct','fallback-v26',?1)",[&now]).unwrap();
        connection.execute("INSERT INTO specification_question_sets(id,checkpoint_id,pulsation_id,input_revision,generation,state_version,conversation_id,questions_json) VALUES('questions-v26','clarify-v26','pulse-v26',1,4,7,'conversation-v26','[]')",[]).unwrap();
        connection.execute("UPDATE pulsations SET integration_checkpoint_id='review-v26',integration_state_version=7 WHERE id='pulse-v26'",[]).unwrap();
        drop(connection);

        let service = PersistenceService::initialize(root.path()).expect("v27 should upgrade");
        let db = service.connection().expect("migrated db should open");

        let violations = db
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query_map([], |_| Ok(()))
            .unwrap()
            .count();
        assert_eq!(violations, 0, "the rebuild broke a foreign key");
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM checkpoints", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            3,
            "the rebuild lost checkpoint rows"
        );
        for (table, column, id, expected) in [
            (
                "pulsations",
                "integration_checkpoint_id",
                "pulse-v26",
                "review-v26",
            ),
            (
                "specification_question_sets",
                "checkpoint_id",
                "questions-v26",
                "clarify-v26",
            ),
            (
                "pulse_execution_overrides",
                "source_checkpoint_id",
                "pulse-v26",
                "fallback-v26",
            ),
        ] {
            let key = if table == "pulse_execution_overrides" {
                "pulsation_id"
            } else {
                "id"
            };
            assert_eq!(
                db.query_row(
                    &format!("SELECT {column} FROM {table} WHERE {key}=?1"),
                    [id],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
                expected,
                "{table}.{column} lost its reference"
            );
        }

        // V20 left `kind` behind `purpose`, forcing open_cli_fallback to write a false
        // kind. V27 realigns them so idx_checkpoint_producer separates the types again.
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM checkpoints WHERE kind<>purpose",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0,
            "kind and purpose are still out of step"
        );

        for index in [
            "idx_checkpoint_producer",
            "idx_checkpoints_purpose",
            "idx_checkpoints_pulsation_id",
        ] {
            assert!(
                db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='index' AND name=?1)",
                    [index],
                    |r| r.get::<_, bool>(0)
                )
                .unwrap(),
                "{index} did not survive the rebuild"
            );
        }
        // Still UNIQUE: two checkpoints of one kind for the same job generation.
        db.execute("INSERT INTO checkpoints(id,pulsation_id,stage,status,created_at,updated_at,kind,input_revision,state_version,producer_job_id,generation,purpose) VALUES('dup-v27','pulse-v26','Integrator','waiting',?1,?1,'artifact_review',1,7,'job-v26',3,'artifact_review')",[&now]).expect_err("idx_checkpoint_producer stopped being unique");

        db.execute("INSERT INTO checkpoints(id,pulsation_id,stage,status,created_at,updated_at,kind,input_revision,state_version,producer_job_id,generation,purpose) VALUES('failure-v27','pulse-v26','Integrator','waiting',?1,?1,'stage_failure',1,7,'job-v26',9,'stage_failure')",[&now]).expect("V27 should accept the stage_failure purpose and kind");
        // A checkpoint offering only Abort is a dead end; the schema refuses to store one.
        db.execute("INSERT INTO stage_failure_requests(checkpoint_id,pulsation_id,job_id,stage,failure_class,allow_retry,allow_revision,error_code,diagnostic,created_at,updated_at) VALUES('failure-v27','pulse-v26','job-v26','Integrator','model_reported_failure',0,0,'gave_up','boom',?1,?1)",[&now]).expect_err("a decision set of abort-only must be rejected");
        db.execute("INSERT INTO stage_failure_requests(checkpoint_id,pulsation_id,job_id,stage,failure_class,allow_retry,allow_revision,revision_stage,error_code,diagnostic,created_at,updated_at) VALUES('failure-v27','pulse-v26','job-v26','Integrator','model_reported_failure',0,1,'Code','gave_up','boom',?1,?1)",[&now]).expect("a revisable failure should be storable");
    }

    #[test]
    fn v19_to_v20_preserves_checkpoint_foreign_keys_and_extends_checkpoint_kind() {
        let root = tempfile::tempdir().expect("tempdir should be created");
        let db_path = root.path().join("hard-vibe-code.db");
        let mut connection = Connection::open(&db_path).expect("v19 db should open");
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .expect("foreign keys should enable");
        embedded::migrations::runner()
            .set_target(refinery::Target::Version(19))
            .run(&mut connection)
            .expect("v1-v19 should apply");
        let now = Utc::now().to_rfc3339();
        connection.execute("INSERT INTO projects(id,name,workspace_path,created_at,updated_at) VALUES('project-v19','Project V19',?1,?2,?2)",params![root.path().to_string_lossy(),now]).unwrap();
        connection.execute("INSERT INTO pulsations(id,project_id,feature_name,stage,status,created_at,updated_at,queue_sequence,pipeline_version,state_version) VALUES('pulse-v19','project-v19','Feature','Integrator','checkpoint',?1,?1,1,2,7)",[&now]).unwrap();
        connection.execute("INSERT INTO jobs(id,pulsation_id,kind,status,created_at,updated_at,project_id,input_revision,generation,state_version,idempotency_key) VALUES('job-v19','pulse-v19','run_stage::Integrator','interrupted',?1,?1,'project-v19',1,3,7,'job-v19')",[&now]).unwrap();
        connection.execute("INSERT INTO checkpoints(id,pulsation_id,stage,status,created_at,updated_at,kind,input_revision,state_version,producer_job_id,generation) VALUES('checkpoint-v19','pulse-v19','Integrator','waiting',?1,?1,'artifact_review',1,7,'job-v19',3)",[&now]).unwrap();
        connection.execute("INSERT INTO specification_question_sets(id,checkpoint_id,pulsation_id,input_revision,generation,state_version,conversation_id,questions_json) VALUES('questions-v19','checkpoint-v19','pulse-v19',1,3,7,'conversation-v19','[]')",[]).unwrap();
        connection.execute("UPDATE pulsations SET integration_checkpoint_id='checkpoint-v19',integration_state_version=7 WHERE id='pulse-v19'",[]).unwrap();
        drop(connection);

        let service = PersistenceService::initialize(root.path()).expect("v20 should upgrade");
        let db = service.connection().expect("migrated db should open");
        let foreign_key_violations: i64 = db
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query_map([], |_| Ok(()))
            .unwrap()
            .count() as i64;
        assert_eq!(foreign_key_violations, 0);
        assert_eq!(
            db.query_row(
                "SELECT integration_checkpoint_id FROM pulsations WHERE id='pulse-v19'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            "checkpoint-v19"
        );
        assert_eq!(
            db.query_row(
                "SELECT checkpoint_id FROM specification_question_sets WHERE id='questions-v19'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            "checkpoint-v19"
        );
        db.execute(
            "INSERT INTO checkpoints(id,pulsation_id,stage,status,created_at,updated_at,kind,input_revision,state_version,purpose)
             VALUES('cli-fallback-v20','pulse-v19','Integrator','waiting',?1,?1,'artifact_review',1,7,'cli_fallback')",
            [&now],
        )
        .expect("V20 should accept cli_fallback checkpoint purpose");
    }

    #[test]
    fn project_crud_flow_works() {
        let (_root, service) = prepare_service();
        let created = service
            .create_project(CreateProjectRequest {
                workspace_path: "C:/workspace/project-a".to_string(),
                name: Some("Project A".to_string()),
            })
            .expect("create project should work");

        let listed = service
            .list_projects(ListProjectsRequest {
                include_archived: false,
            })
            .expect("list projects should work");
        assert_eq!(listed.projects.len(), 1);
        assert_eq!(listed.projects[0].id, created.project.id);

        let opened = service
            .open_project(OpenProjectRequest {
                project_id: created.project.id.clone(),
            })
            .expect("open project should work");
        assert!(opened.project.last_opened_at.is_some());

        let archived = service
            .archive_project(ArchiveProjectRequest {
                project_id: created.project.id.clone(),
            })
            .expect("archive project should work");
        assert!(archived.project.is_archived);

        let listed_after_archive = service
            .list_projects(ListProjectsRequest {
                include_archived: false,
            })
            .expect("list active projects should work");
        assert_eq!(listed_after_archive.projects.len(), 0);

        let listed_with_archived = service
            .list_projects(ListProjectsRequest {
                include_archived: true,
            })
            .expect("list archived projects should work");
        assert_eq!(listed_with_archived.projects.len(), 1);
    }

    #[test]
    fn brownfield_consent_is_project_scoped_revocable_and_validated() {
        let (_root, service) = prepare_service();
        let first = service
            .create_project(CreateProjectRequest {
                workspace_path: "C:/workspace/brownfield-a".into(),
                name: None,
            })
            .unwrap()
            .project;
        let second = service
            .create_project(CreateProjectRequest {
                workspace_path: "C:/workspace/brownfield-b".into(),
                name: None,
            })
            .unwrap()
            .project;

        assert_eq!(
            service
                .get_brownfield_onboarding(&first.id)
                .unwrap()
                .consent,
            BrownfieldConsentState::NotDecided
        );
        service
            .set_brownfield_onboarding_consent(&first.id, true)
            .unwrap();
        assert_eq!(
            service
                .get_brownfield_onboarding(&first.id)
                .unwrap()
                .consent,
            BrownfieldConsentState::Granted
        );
        assert_eq!(
            service
                .get_brownfield_onboarding(&second.id)
                .unwrap()
                .consent,
            BrownfieldConsentState::NotDecided
        );
        assert_eq!(
            service
                .set_brownfield_onboarding_consent(&first.id, false)
                .unwrap()
                .consent,
            BrownfieldConsentState::Declined
        );
        assert!(matches!(
            service.set_brownfield_onboarding_consent("missing-project", true),
            Err(PersistenceServiceError::NotFound(_))
        ));
    }

    #[test]
    fn create_project_rejects_duplicate_workspace_path() {
        let (_root, service) = prepare_service();
        service
            .create_project(CreateProjectRequest {
                workspace_path: "C:/workspace/dupe".to_string(),
                name: Some("First".to_string()),
            })
            .expect("first project should be created");

        let duplicate = service.create_project(CreateProjectRequest {
            workspace_path: "C:/workspace/dupe".to_string(),
            name: Some("Second".to_string()),
        });

        assert!(matches!(
            duplicate,
            Err(PersistenceServiceError::Validation(_))
        ));
    }

    #[test]
    fn ensure_project_reactivates_archived_workspace_without_duplication() {
        let (_root, service) = prepare_service();
        let created = service
            .create_project(CreateProjectRequest {
                workspace_path: "C:/workspace/reactivate".to_string(),
                name: Some("Reactivator".to_string()),
            })
            .expect("project should be created")
            .project;

        service
            .archive_project(ArchiveProjectRequest {
                project_id: created.id.clone(),
            })
            .expect("project should be archived");

        let ensured_after_archive = service
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: "C:/workspace/reactivate".to_string(),
                preferred_name: Some("Should not create another".to_string()),
            })
            .expect("project should be reactivated")
            .project;
        assert_eq!(ensured_after_archive.id, created.id);
        assert!(!ensured_after_archive.is_archived);

        let ensured_again = service
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: "C:/workspace/reactivate".to_string(),
                preferred_name: Some("Still same".to_string()),
            })
            .expect("project should remain unique")
            .project;
        assert_eq!(ensured_again.id, created.id);

        let listed = service
            .list_projects(ListProjectsRequest {
                include_archived: true,
            })
            .expect("listing should work");
        assert_eq!(listed.projects.len(), 1);
    }

    #[test]
    fn get_last_active_project_prefers_latest_non_archived_project() {
        let (_root, service) = prepare_service();
        let project_a = service
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: "C:/workspace/last-a".to_string(),
                preferred_name: Some("Project A".to_string()),
            })
            .expect("project A should be created")
            .project;

        thread::sleep(Duration::from_millis(2));

        let project_b = service
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: "C:/workspace/last-b".to_string(),
                preferred_name: Some("Project B".to_string()),
            })
            .expect("project B should be created")
            .project;

        let last_before_archive = service
            .get_last_active_project()
            .expect("last active project should be returned")
            .project
            .expect("one active project should exist");
        assert_eq!(last_before_archive.id, project_b.id);

        service
            .archive_project(ArchiveProjectRequest {
                project_id: project_b.id.clone(),
            })
            .expect("project B should be archived");

        let last_after_archive = service
            .get_last_active_project()
            .expect("last active project should be returned")
            .project
            .expect("one active project should exist");
        assert_eq!(last_after_archive.id, project_a.id);
    }

    #[test]
    fn save_ui_session_rejects_invalid_workspace_view_mode() {
        let (_root, service) = prepare_service();
        let project = service
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: "C:/workspace/view-mode".to_string(),
                preferred_name: Some("View Mode".to_string()),
            })
            .expect("project should be created")
            .project;

        let invalid_mode_result = service.save_ui_session(SaveUiSessionRequest {
            auto_open_review: false,
            project_id: project.id,
            workspace_root: "C:/workspace/view-mode".to_string(),
            active_tab_path: None,
            workspace_view_mode: "split".to_string(),
            active_view: "workspace".to_string(),
            sidebar_collapsed: false,
            context_drawer_open: true,
            active_pulsation_id: None,
            selected_vault_path: None,
            active_chat_conversation_id: None,
            scroll_top: None,
            tabs: vec![],
        });

        assert!(matches!(
            invalid_mode_result,
            Err(PersistenceServiceError::Validation(_))
        ));
    }

    #[test]
    fn save_ui_session_rejects_invalid_active_view() {
        let (_root, service) = prepare_service();
        let project = service
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: "C:/workspace/active-view".to_string(),
                preferred_name: Some("Active View".to_string()),
            })
            .expect("project should be created")
            .project;

        let result = service.save_ui_session(SaveUiSessionRequest {
            auto_open_review: false,
            project_id: project.id,
            workspace_root: "C:/workspace/active-view".to_string(),
            active_tab_path: None,
            workspace_view_mode: "editor".to_string(),
            active_view: "unknown".to_string(),
            sidebar_collapsed: false,
            context_drawer_open: true,
            active_pulsation_id: None,
            selected_vault_path: None,
            active_chat_conversation_id: None,
            scroll_top: None,
            tabs: vec![],
        });

        assert!(matches!(
            result,
            Err(PersistenceServiceError::Validation(_))
        ));
    }

    /// ADR-029: the Notes view is restored like every other workbench view.
    #[test]
    fn save_ui_session_accepts_the_notes_and_canvas_views() {
        let (_root, service) = prepare_service();
        let project = service
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: "C:/workspace/notes-view".to_string(),
                preferred_name: Some("Notes View".to_string()),
            })
            .expect("project should be created")
            .project;

        for view in ["notes", "canvas"] {
            service
                .save_ui_session(SaveUiSessionRequest {
                    auto_open_review: false,
                    project_id: project.id.clone(),
                    workspace_root: "C:/workspace/notes-view".to_string(),
                    active_tab_path: None,
                    workspace_view_mode: "editor".to_string(),
                    active_view: view.to_string(),
                    sidebar_collapsed: false,
                    context_drawer_open: true,
                    active_pulsation_id: None,
                    selected_vault_path: None,
                    active_chat_conversation_id: None,
                    scroll_top: None,
                    tabs: vec![],
                })
                .expect("the notes view is a known view");
            let session = service
                .load_ui_session(LoadUiSessionRequest {
                    project_id: project.id.clone(),
                })
                .expect("session loads")
                .session
                .expect("session exists");
            assert_eq!(session.active_view, view);
        }
    }

    #[test]
    fn save_ui_session_rejects_unknown_project() {
        let (_root, service) = prepare_service();
        let response = service.save_ui_session(SaveUiSessionRequest {
            auto_open_review: false,
            project_id: "missing-project".to_string(),
            workspace_root: "C:/workspace/missing".to_string(),
            active_tab_path: None,
            workspace_view_mode: "editor".to_string(),
            active_view: "workspace".to_string(),
            sidebar_collapsed: false,
            context_drawer_open: true,
            active_pulsation_id: None,
            selected_vault_path: None,
            active_chat_conversation_id: None,
            scroll_top: None,
            tabs: vec![],
        });

        assert!(matches!(
            response,
            Err(PersistenceServiceError::NotFound(_))
        ));
    }

    #[test]
    fn save_ui_session_overwrites_previous_tabs_and_skips_empty_relative_paths() {
        let (_root, service) = prepare_service();
        let project = service
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: "C:/workspace/overwrite".to_string(),
                preferred_name: Some("Overwrite".to_string()),
            })
            .expect("project should be created")
            .project;

        service
            .save_ui_session(SaveUiSessionRequest {
                auto_open_review: false,
                project_id: project.id.clone(),
                workspace_root: project.workspace_path.clone(),
                active_tab_path: Some("first.md".to_string()),
                workspace_view_mode: "editor".to_string(),
                active_view: "workspace".to_string(),
                sidebar_collapsed: false,
                context_drawer_open: true,
                active_pulsation_id: None,
                selected_vault_path: None,
                active_chat_conversation_id: None,
                scroll_top: Some(10.0),
                tabs: vec![
                    UiTabSnapshot {
                        relative_path: "first.md".to_string(),
                        draft_content: Some("draft 1".to_string()),
                        base_revision: Some("1:1".to_string()),
                        cursor_line: Some(1),
                        cursor_column: Some(1),
                        scroll_top: Some(1.0),
                    },
                    UiTabSnapshot {
                        relative_path: "second.md".to_string(),
                        draft_content: None,
                        base_revision: Some("1:2".to_string()),
                        cursor_line: Some(2),
                        cursor_column: Some(2),
                        scroll_top: Some(2.0),
                    },
                ],
            })
            .expect("first session save should work");

        service
            .save_ui_session(SaveUiSessionRequest {
                auto_open_review: false,
                project_id: project.id.clone(),
                workspace_root: project.workspace_path.clone(),
                active_tab_path: Some("third.md".to_string()),
                workspace_view_mode: "hitl".to_string(),
                active_view: "vault".to_string(),
                sidebar_collapsed: true,
                context_drawer_open: false,
                active_pulsation_id: Some("pulse-three".to_string()),
                selected_vault_path: Some("third.md".to_string()),
                active_chat_conversation_id: Some("chat-three".to_string()),
                scroll_top: Some(20.0),
                tabs: vec![
                    UiTabSnapshot {
                        relative_path: "   ".to_string(),
                        draft_content: Some("should skip".to_string()),
                        base_revision: Some("2:1".to_string()),
                        cursor_line: Some(3),
                        cursor_column: Some(3),
                        scroll_top: Some(3.0),
                    },
                    UiTabSnapshot {
                        relative_path: "third.md".to_string(),
                        draft_content: Some("draft 3".to_string()),
                        base_revision: Some("2:3".to_string()),
                        cursor_line: Some(4),
                        cursor_column: Some(4),
                        scroll_top: Some(4.0),
                    },
                ],
            })
            .expect("second session save should work");

        let loaded = service
            .load_ui_session(LoadUiSessionRequest {
                project_id: project.id.clone(),
            })
            .expect("session should load")
            .session
            .expect("session should exist");

        assert_eq!(loaded.workspace_view_mode, "hitl");
        assert_eq!(loaded.active_view, "vault");
        assert!(loaded.sidebar_collapsed);

        // The shell's Home view persists like any other; unknown ids are still rejected.
        service
            .save_ui_session(SaveUiSessionRequest {
                auto_open_review: false,
                project_id: project.id.clone(),
                workspace_root: project.workspace_path.clone(),
                active_tab_path: None,
                workspace_view_mode: "editor".to_string(),
                active_view: "home".to_string(),
                sidebar_collapsed: false,
                context_drawer_open: false,
                active_pulsation_id: None,
                selected_vault_path: None,
                active_chat_conversation_id: None,
                scroll_top: None,
                tabs: vec![],
            })
            .expect("home is a valid active view");
        assert_eq!(
            service
                .load_ui_session(LoadUiSessionRequest {
                    project_id: project.id.clone(),
                })
                .unwrap()
                .session
                .unwrap()
                .active_view,
            "home"
        );
        let rejected = service.save_ui_session(SaveUiSessionRequest {
            auto_open_review: false,
            project_id: project.id.clone(),
            workspace_root: project.workspace_path.clone(),
            active_tab_path: None,
            workspace_view_mode: "editor".to_string(),
            active_view: "dashboard".to_string(),
            sidebar_collapsed: false,
            context_drawer_open: false,
            active_pulsation_id: None,
            selected_vault_path: None,
            active_chat_conversation_id: None,
            scroll_top: None,
            tabs: vec![],
        });
        assert!(matches!(
            rejected,
            Err(PersistenceServiceError::Validation(_))
        ));
        assert!(!loaded.context_drawer_open);
        assert_eq!(loaded.active_pulsation_id.as_deref(), Some("pulse-three"));
        assert_eq!(loaded.selected_vault_path.as_deref(), Some("third.md"));
        assert_eq!(
            loaded.active_chat_conversation_id.as_deref(),
            Some("chat-three")
        );
        assert_eq!(loaded.active_tab_path.as_deref(), Some("third.md"));
        assert_eq!(loaded.tabs.len(), 1);
        assert_eq!(loaded.tabs[0].relative_path, "third.md");
        assert_eq!(loaded.tabs[0].draft_content.as_deref(), Some("draft 3"));
    }

    #[test]
    fn ui_session_round_trip_preserves_tab_order_and_draft() {
        let (_root, service) = prepare_service();
        let project = service
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: "C:/workspace/session-project".to_string(),
                preferred_name: Some("Session Project".to_string()),
            })
            .expect("project should be ensured")
            .project;

        service
            .save_ui_session(SaveUiSessionRequest {
                auto_open_review: false,
                project_id: project.id.clone(),
                workspace_root: project.workspace_path.clone(),
                active_tab_path: Some("notes/todo.md".to_string()),
                workspace_view_mode: "hitl".to_string(),
                active_view: "chat".to_string(),
                sidebar_collapsed: true,
                context_drawer_open: false,
                active_pulsation_id: Some("pulse-observe".to_string()),
                selected_vault_path: Some("notes/todo.md".to_string()),
                active_chat_conversation_id: Some("chat-observe".to_string()),
                scroll_top: Some(112.5),
                tabs: vec![
                    UiTabSnapshot {
                        relative_path: "notes/todo.md".to_string(),
                        draft_content: Some("# Draft".to_string()),
                        base_revision: Some("1:8".to_string()),
                        cursor_line: Some(3),
                        cursor_column: Some(5),
                        scroll_top: Some(90.0),
                    },
                    UiTabSnapshot {
                        relative_path: "README.md".to_string(),
                        draft_content: None,
                        base_revision: Some("2:12".to_string()),
                        cursor_line: Some(1),
                        cursor_column: Some(1),
                        scroll_top: None,
                    },
                ],
            })
            .expect("ui session should save");

        let loaded = service
            .load_ui_session(LoadUiSessionRequest {
                project_id: project.id,
            })
            .expect("ui session should load");
        let session = loaded.session.expect("session should exist");

        assert_eq!(session.workspace_view_mode, "hitl");
        assert_eq!(session.active_view, "chat");
        assert!(session.sidebar_collapsed);
        assert!(!session.context_drawer_open);
        assert_eq!(
            session.active_pulsation_id.as_deref(),
            Some("pulse-observe")
        );
        assert_eq!(
            session.selected_vault_path.as_deref(),
            Some("notes/todo.md")
        );
        assert_eq!(
            session.active_chat_conversation_id.as_deref(),
            Some("chat-observe")
        );
        assert_eq!(session.active_tab_path.as_deref(), Some("notes/todo.md"));
        assert_eq!(session.tabs.len(), 2);
        assert_eq!(session.tabs[0].relative_path, "notes/todo.md");
        assert_eq!(session.tabs[0].draft_content.as_deref(), Some("# Draft"));
        assert_eq!(session.tabs[1].relative_path, "README.md");
    }

    #[test]
    fn failed_ui_session_save_rolls_back_layout_and_tabs_atomically() {
        let (_root, service) = prepare_service();
        let project = service
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: "C:/workspace/atomic-session".to_string(),
                preferred_name: Some("Atomic Session".to_string()),
            })
            .expect("project should be created")
            .project;

        service
            .save_ui_session(SaveUiSessionRequest {
                auto_open_review: false,
                project_id: project.id.clone(),
                workspace_root: project.workspace_path.clone(),
                active_tab_path: Some("stable.md".to_string()),
                workspace_view_mode: "editor".to_string(),
                active_view: "workspace".to_string(),
                sidebar_collapsed: false,
                context_drawer_open: true,
                active_pulsation_id: Some("stable-pulse".to_string()),
                selected_vault_path: Some("stable.md".to_string()),
                active_chat_conversation_id: Some("stable-chat".to_string()),
                scroll_top: Some(12.0),
                tabs: vec![UiTabSnapshot {
                    relative_path: "stable.md".to_string(),
                    draft_content: Some("stable draft".to_string()),
                    base_revision: Some("1:1".to_string()),
                    cursor_line: Some(2),
                    cursor_column: Some(3),
                    scroll_top: Some(12.0),
                }],
            })
            .expect("initial session should save");

        let failed = service.save_ui_session(SaveUiSessionRequest {
            auto_open_review: false,
            project_id: project.id.clone(),
            workspace_root: project.workspace_path,
            active_tab_path: Some("duplicate.md".to_string()),
            workspace_view_mode: "hitl".to_string(),
            active_view: "chat".to_string(),
            sidebar_collapsed: true,
            context_drawer_open: false,
            active_pulsation_id: Some("replacement-pulse".to_string()),
            selected_vault_path: Some("duplicate.md".to_string()),
            active_chat_conversation_id: Some("replacement-chat".to_string()),
            scroll_top: Some(99.0),
            tabs: vec![
                UiTabSnapshot {
                    relative_path: "duplicate.md".to_string(),
                    draft_content: Some("first".to_string()),
                    base_revision: None,
                    cursor_line: None,
                    cursor_column: None,
                    scroll_top: None,
                },
                UiTabSnapshot {
                    relative_path: "duplicate.md".to_string(),
                    draft_content: Some("second".to_string()),
                    base_revision: None,
                    cursor_line: None,
                    cursor_column: None,
                    scroll_top: None,
                },
            ],
        });
        assert!(matches!(failed, Err(PersistenceServiceError::Db(_))));

        let preserved = service
            .load_ui_session(LoadUiSessionRequest {
                project_id: project.id,
            })
            .expect("previous session should remain readable")
            .session
            .expect("previous session should remain present");
        assert_eq!(preserved.active_view, "workspace");
        assert!(!preserved.sidebar_collapsed);
        assert!(preserved.context_drawer_open);
        assert_eq!(
            preserved.active_pulsation_id.as_deref(),
            Some("stable-pulse")
        );
        assert_eq!(
            preserved.active_chat_conversation_id.as_deref(),
            Some("stable-chat")
        );
        assert_eq!(preserved.tabs.len(), 1);
        assert_eq!(preserved.tabs[0].relative_path, "stable.md");
        assert_eq!(
            preserved.tabs[0].draft_content.as_deref(),
            Some("stable draft")
        );
    }

    #[test]
    fn ui_sessions_remain_isolated_between_projects() {
        let (_root, service) = prepare_service();
        let project_a = service
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: "C:/workspace/session-a".to_string(),
                preferred_name: Some("Session A".to_string()),
            })
            .unwrap()
            .project;
        let project_b = service
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: "C:/workspace/session-b".to_string(),
                preferred_name: Some("Session B".to_string()),
            })
            .unwrap()
            .project;

        for (project, active_view, active_chat) in [
            (&project_a, "workspace", "chat-a"),
            (&project_b, "chat", "chat-b"),
        ] {
            service
                .save_ui_session(SaveUiSessionRequest {
                    auto_open_review: false,
                    project_id: project.id.clone(),
                    workspace_root: project.workspace_path.clone(),
                    active_tab_path: None,
                    workspace_view_mode: "editor".to_string(),
                    active_view: active_view.to_string(),
                    sidebar_collapsed: active_view == "chat",
                    context_drawer_open: active_view != "chat",
                    active_pulsation_id: None,
                    selected_vault_path: None,
                    active_chat_conversation_id: Some(active_chat.to_string()),
                    scroll_top: None,
                    tabs: vec![],
                })
                .unwrap();
        }

        let session_a = service
            .load_ui_session(LoadUiSessionRequest {
                project_id: project_a.id,
            })
            .unwrap()
            .session
            .unwrap();
        let session_b = service
            .load_ui_session(LoadUiSessionRequest {
                project_id: project_b.id,
            })
            .unwrap()
            .session
            .unwrap();
        assert_eq!(session_a.active_view, "workspace");
        assert_eq!(
            session_a.active_chat_conversation_id.as_deref(),
            Some("chat-a")
        );
        assert!(!session_a.sidebar_collapsed);
        assert_eq!(session_b.active_view, "chat");
        assert_eq!(
            session_b.active_chat_conversation_id.as_deref(),
            Some("chat-b")
        );
        assert!(session_b.sidebar_collapsed);
    }

    #[test]
    fn worker_advances_pulsation_until_first_checkpoint() {
        let (_root, service) = prepare_service();
        let project = service
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: "C:/workspace/phase-4-worker".to_string(),
                preferred_name: Some("Phase4 Worker".to_string()),
            })
            .expect("project should be created")
            .project;

        let created = service
            .create_pulsation(CreatePulsationRequest {
                project_id: project.id.clone(),
                feature_name: "Feat: Worker Flow".to_string(),
            })
            .expect("pulsation should be created")
            .pulsation;
        assert_eq!(created.status, PulsationStatus::Planning);
        assert_eq!(created.stage, PulsationStage::Intake);

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("tokio runtime should build");

        runtime
            .block_on(service.process_next_pending_job())
            .expect("first job should run");
        runtime
            .block_on(service.process_next_pending_job())
            .expect("second job should run");

        let snapshot = service
            .get_pulsation_runtime_snapshot(GetPulsationRuntimeSnapshotRequest {
                project_id: project.id,
            })
            .expect("snapshot should load")
            .snapshot;

        let pulse = snapshot
            .pulsations
            .iter()
            .find(|item| item.id == created.id)
            .expect("pulsation should exist");
        assert_eq!(pulse.stage, PulsationStage::Specification);
        assert_eq!(pulse.status, PulsationStatus::Checkpoint);
        assert_eq!(snapshot.hitl_waiting_count, 1);
    }

    #[test]
    fn selected_runtime_snapshot_keeps_project_summary_and_limits_rich_details() {
        let (_root, service) = prepare_service();
        let project = service
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: "C:/workspace/runtime-summary".to_string(),
                preferred_name: Some("Runtime Summary".to_string()),
            })
            .unwrap()
            .project;
        let first = service
            .create_pulsation(CreatePulsationRequest {
                project_id: project.id.clone(),
                feature_name: "First runtime item".to_string(),
            })
            .unwrap()
            .pulsation;
        let second = service
            .create_pulsation(CreatePulsationRequest {
                project_id: project.id.clone(),
                feature_name: "Second runtime item".to_string(),
            })
            .unwrap()
            .pulsation;

        let selected = service
            .get_pulsation_runtime_snapshot_for_detail(
                GetPulsationRuntimeSnapshotRequest {
                    project_id: project.id.clone(),
                },
                Some(&first.id),
            )
            .unwrap()
            .snapshot;
        assert_eq!(selected.pulsations.len(), 2);
        assert_eq!(selected.details.len(), 1);
        assert_eq!(selected.details[0].pulsation_id, first.id);

        let complete = service
            .get_pulsation_runtime_snapshot(GetPulsationRuntimeSnapshotRequest {
                project_id: project.id,
            })
            .unwrap()
            .snapshot;
        assert_eq!(complete.details.len(), 2);
        assert!(
            complete
                .pulsations
                .iter()
                .any(|pulsation| pulsation.id == second.id)
        );
    }

    #[test]
    fn hitl_decision_approve_then_reject_updates_state_machine() {
        let (_root, service) = prepare_service();
        let project = service
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: "C:/workspace/phase-4-hitl".to_string(),
                preferred_name: Some("Phase4 HITL".to_string()),
            })
            .expect("project should be created")
            .project;

        let created = service
            .create_pulsation(CreatePulsationRequest {
                project_id: project.id.clone(),
                feature_name: "Feat: HITL Decision".to_string(),
            })
            .expect("pulsation should be created")
            .pulsation;

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("tokio runtime should build");
        runtime
            .block_on(service.process_next_pending_job())
            .expect("first job should run");
        runtime
            .block_on(service.process_next_pending_job())
            .expect("second job should run");

        let approved = service
            .apply_hitl_decision(ApplyHitlDecisionRequest {
                checkpoint_id: service
                    .get_hitl_checkpoint(&created.id)
                    .unwrap()
                    .checkpoint_id,
                expected_state_version: service
                    .get_hitl_checkpoint(&created.id)
                    .unwrap()
                    .state_version,
                feedback: None,
                pulsation_id: created.id.clone(),
                decision: HitlDecision::Approve,
            })
            .expect("approve should work")
            .pulsation;
        assert_eq!(approved.stage, PulsationStage::Contract);
        assert_eq!(approved.status, PulsationStatus::Planning);

        runtime
            .block_on(service.process_next_pending_job())
            .expect("third job should run");

        let contract_checkpoint = service
            .list_pulsations(ListPulsationsRequest {
                project_id: project.id.clone(),
            })
            .expect("pulsations should list")
            .pulsations
            .into_iter()
            .find(|item| item.id == created.id)
            .expect("pulsation should exist");
        assert_eq!(contract_checkpoint.stage, PulsationStage::Contract);
        assert_eq!(contract_checkpoint.status, PulsationStatus::Checkpoint);

        let rejected = service
            .apply_hitl_decision(ApplyHitlDecisionRequest {
                checkpoint_id: service
                    .get_hitl_checkpoint(&created.id)
                    .unwrap()
                    .checkpoint_id,
                expected_state_version: service
                    .get_hitl_checkpoint(&created.id)
                    .unwrap()
                    .state_version,
                feedback: None,
                pulsation_id: created.id,
                decision: HitlDecision::Reject,
            })
            .expect("reject should work")
            .pulsation;
        assert_eq!(rejected.stage, PulsationStage::Contract);
        assert_eq!(rejected.status, PulsationStatus::Failed);
    }

    #[test]
    fn phase5_runtime_config_update_round_trip() {
        let (_root, service) = prepare_service();

        let default_config = service
            .get_agent_runtime_config()
            .expect("default runtime config should load")
            .config;
        assert_eq!(default_config.provider, LlmProvider::Openai);
        assert_eq!(default_config.routing_v2_mode, RoutingMode::On);

        let updated = service
            .update_agent_runtime_config(UpdateAgentRuntimeConfigRequest {
                provider: Some(LlmProvider::Anthropic),
                model: Some("claude-test-model".to_string()),
                temperature: Some(0.7),
                max_output_tokens: Some(1200),
                timeout_ms: Some(20_000),
                max_retries: Some(1),
                context_chars_limit: Some(9_000),
                max_context_files: Some(3),
                context_strategy: Some(ContextStrategy::YamlGraphRag),
                embedding_enabled: Some(true),
                embedding_provider: Some(EmbeddingProvider::Ollama),
                embedding_model: Some("nomic-embed-text".to_string()),
                embedding_top_k: Some(7),
                routing_v2_mode: Some(RoutingMode::Shadow),
                adaptive_context_mode: Some(AdaptiveContextMode::On),
                dynamic_context_policy: Some(DynamicContextPolicy {
                    max_k: 2,
                    ..DynamicContextPolicy::default()
                }),
            })
            .expect("runtime config should update")
            .config;
        assert_eq!(updated.provider, LlmProvider::Anthropic);
        assert_eq!(updated.model, "claude-test-model");
        assert_eq!(updated.max_retries, 1);
        assert_eq!(updated.context_strategy, ContextStrategy::YamlGraphRag);
        assert!(updated.embedding_enabled);
        assert_eq!(updated.embedding_provider, EmbeddingProvider::Ollama);
        assert_eq!(updated.embedding_top_k, 7);
        assert_eq!(updated.routing_v2_mode, RoutingMode::Shadow);
        assert_eq!(updated.adaptive_context_mode, AdaptiveContextMode::On);
        assert_eq!(updated.dynamic_context_policy.max_k, 2);

        let reloaded = service
            .get_agent_runtime_config()
            .expect("runtime config should persist")
            .config;
        assert_eq!(reloaded.provider, LlmProvider::Anthropic);
        assert_eq!(reloaded.model, "claude-test-model");
        assert_eq!(reloaded.context_strategy, ContextStrategy::YamlGraphRag);
        assert!(reloaded.embedding_enabled);
    }

    #[test]
    fn observational_agent_outputs_are_sanitized_without_mutating_machine_artifacts() {
        let fixture = serial::tests::Fixture::new(PulsationStage::Code);
        let secret = "sk-proj-syntheticabcdefghijklmnop";
        let mut output = fixture.output("advance", Some("src/fixture.rs"), secret);
        output.request_payload = Value::Object(serde_json::Map::from_iter([(
            secret.to_owned(),
            Value::String("source metadata".into()),
        )]));
        output.response_payload =
            serde_json::json!({"nested":serde_json::json!({"key":secret}).to_string()});
        let db = fixture.service.connection().unwrap();
        PersistenceService::persist_agent_run_output(
            &db,
            &fixture.run,
            &fixture.job,
            &output,
            Some(secret),
        )
        .unwrap();
        let observed:String=db.query_row("SELECT request_payload||response_payload||raw_output||structured_output||validation_error FROM agent_run_outputs WHERE agent_run_id=?1",[&fixture.run],|row|row.get(0)).unwrap();
        assert!(!observed.contains(secret));
        assert_eq!(output.artifacts[0].content, secret);
        // Simulate a database from before this policy, without rewriting it.
        db.execute("UPDATE agent_run_outputs SET raw_output=?1,structured_output=?2,tool_trace=?2 WHERE agent_run_id=?3",params![secret,serde_json::json!({"legacy":secret}).to_string(),fixture.run]).unwrap();
        let listed = fixture
            .service
            .list_agent_runs(ListAgentRunsRequest {
                pulsation_id: Some(fixture.pulse.id.clone()),
                project_id: Some(fixture.pulse.project_id.clone()),
            })
            .unwrap();
        assert!(!serde_json::to_string(&listed).unwrap().contains(secret));
        let original: String = db
            .query_row(
                "SELECT raw_output FROM agent_run_outputs WHERE agent_run_id=?1",
                [&fixture.run],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(original, secret);
    }

    #[test]
    fn phase5_agent_run_persists_structured_output() {
        let root = tempfile::tempdir().expect("workspace tempdir should be created");
        fs::write(
            root.path().join("feature-auth.md"),
            "# Feature Auth\nRules and notes for login flow.",
        )
        .expect("workspace file should write");
        fs::write(
            root.path().join("contract-auth.txt"),
            "POST /api/auth/login request and response contract",
        )
        .expect("workspace file should write");

        let service_root = tempfile::tempdir().expect("service tempdir should be created");
        let service = PersistenceService::initialize(service_root.path())
            .expect("persistence service should initialize");
        let project = service
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: root.path().to_string_lossy().to_string(),
                preferred_name: Some("Phase5 Outputs".to_string()),
            })
            .expect("project should be created")
            .project;

        let created = service
            .create_pulsation(CreatePulsationRequest {
                project_id: project.id.clone(),
                feature_name: "Feat: Runtime Output".to_string(),
            })
            .expect("pulsation should be created")
            .pulsation;

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("tokio runtime should build");
        runtime
            .block_on(service.process_next_pending_job())
            .expect("first job should run");
        runtime
            .block_on(service.process_next_pending_job())
            .expect("second job should run");

        let runs = service
            .list_agent_runs(ListAgentRunsRequest {
                project_id: Some(project.id.clone()),
                pulsation_id: Some(created.id.clone()),
            })
            .expect("agent runs should load")
            .runs;

        assert!(runs.len() >= 2);
        assert!(runs.iter().any(|run| run.structured_output.is_some()));
        assert!(runs.iter().any(|run| run.raw_output.is_some()));
        assert!(
            runs.iter()
                .all(|run| run.pulsation_id.as_deref() == Some(&created.id))
        );
        assert!(
            runs.iter()
                .all(|run| run.pulsation_name.as_deref() == Some("Feat: Runtime Output"))
        );
        assert!(runs.iter().all(|run| run.job_kind.is_some()));
        let trace_count: i64 = service
            .connection()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM context_retrieval_runs WHERE pulsation_id=?1",
                [&created.id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            trace_count >= 2,
            "cada resultado de agente preserva seu trace tipado"
        );
    }

    #[test]
    fn phase5_cancel_running_job_marks_run_interrupted() {
        let root = tempfile::tempdir().expect("workspace tempdir should be created");
        fs::write(
            root.path().join("feature-cancel.md"),
            "# Cancellation\nLong running stage smoke test.",
        )
        .expect("workspace file should write");

        let service_root = tempfile::tempdir().expect("service tempdir should be created");
        let service = PersistenceService::initialize(service_root.path())
            .expect("persistence service should initialize");
        let project = service
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: root.path().to_string_lossy().to_string(),
                preferred_name: Some("Phase5 Cancel".to_string()),
            })
            .expect("project should be created")
            .project;

        let created = service
            .create_pulsation(CreatePulsationRequest {
                project_id: project.id.clone(),
                feature_name: "Feat: Cancel Runtime".to_string(),
            })
            .expect("pulsation should be created")
            .pulsation;

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_time()
            .build()
            .expect("tokio runtime should build");

        let worker_service = service.clone();
        let handle = runtime.spawn(async move { worker_service.process_next_pending_job().await });

        let mut running_seen = false;
        for _ in 0..50 {
            let snapshot = service
                .get_pulsation_runtime_snapshot(GetPulsationRuntimeSnapshotRequest {
                    project_id: project.id.clone(),
                })
                .expect("snapshot should load")
                .snapshot;
            if snapshot.main_agents_count + snapshot.background_agents_count > 0 {
                running_seen = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            running_seen,
            "running job should be visible before cancellation"
        );

        let cancelled = service
            .cancel_pulsation_jobs(CancelPulsationJobsRequest {
                pulsation_id: created.id.clone(),
            })
            .expect("cancel command should run");
        assert!(cancelled.cancelled_jobs >= 1);

        let result = runtime.block_on(async { handle.await.expect("worker join should succeed") });
        assert!(
            matches!(result, Err(PersistenceServiceError::Cancelled(_))),
            "{result:?}"
        );

        let runs = service
            .list_agent_runs(ListAgentRunsRequest {
                pulsation_id: Some(created.id),
                project_id: None,
            })
            .expect("agent runs should load")
            .runs;
        assert!(runs.iter().any(|run| run.status == "interrupted"));
    }

    #[test]
    fn shutdown_cancels_every_registered_job_token() {
        let service_root = tempfile::tempdir().expect("service tempdir should be created");
        let service = PersistenceService::initialize(service_root.path())
            .expect("persistence service should initialize");
        let first = CancellationToken::new();
        let second = CancellationToken::new();
        service.register_job_cancellation("job-a", first.clone());
        service.register_job_cancellation("job-b", second.clone());

        assert_eq!(service.cancel_all_active_jobs(), 2);
        assert!(first.is_cancelled());
        assert!(second.is_cancelled());

        service.unregister_job_cancellation("job-a");
        service.unregister_job_cancellation("job-b");
        assert_eq!(service.cancel_all_active_jobs(), 0);
    }

    fn claimed_cli_job_fixture(
        service: &PersistenceService,
        workspace_path: &str,
    ) -> (Project, Pulsation, Job, String) {
        let project = service
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: workspace_path.to_string(),
                preferred_name: Some("CLI fallback fixture".to_string()),
            })
            .expect("project should be created")
            .project;
        let pulse = service
            .create_pulsation(CreatePulsationRequest {
                project_id: project.id.clone(),
                feature_name: "CLI fallback".to_string(),
            })
            .expect("pulse should be created")
            .pulsation;
        let (job, _cancellation) = service
            .claim_next_pending_job()
            .expect("claim should succeed")
            .expect("stage job should be available");
        let mut connection = service.connection().expect("database should open");
        let tx = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .expect("transaction should start");
        let current = PersistenceService::find_pulsation_by_id(&tx, &pulse.id)
            .expect("pulse should load")
            .expect("pulse should exist");
        PersistenceService::transition_pulsation(
            &tx,
            &current,
            PulsationStatus::Running,
            current.stage,
            "test_cli_started",
            None,
        )
        .expect("pulse should enter running");
        tx.execute(
            "UPDATE jobs SET state_version=(SELECT state_version FROM pulsations WHERE id=?2)
             WHERE id=?1",
            params![job.id, pulse.id],
        )
        .expect("job fence should follow pulse");
        tx.commit().expect("transaction should commit");
        let agent_run_id = service
            .start_agent_run_for_job(&job, &service.load_runtime_config().unwrap())
            .expect("agent run should start");
        (project, pulse, job, agent_run_id)
    }

    /// Regression for the incident where a pulsation stayed stuck in
    /// `Planning` forever after the `OpenCode` CLI failed and Codex was
    /// elected in its place: `elect_next_backend` bumped the pulsation's
    /// `state_version` via `transition_pulsation` but the job UPDATE right
    /// after it never carried the new version along, fencing the job out of
    /// `claim_next_pending_job` (`j.state_version=p.state_version`) for
    /// good. `elect_next_backend` itself cannot be driven end-to-end here --
    /// it depends on `TerminalService::available_backends()`, a real
    /// environment probe with no discovery seam to inject from a test -- so
    /// this reproduces its two writes (the pulsation transition, then the
    /// job UPDATE with the corrected `state_version` subquery) verbatim and
    /// proves the requeued job survives the claim fence. Written against
    /// the unfixed UPDATE, this test fails at the final claim.
    #[test]
    fn backend_ladder_keeps_the_requeued_job_claimable_after_the_pulsation_bumps_version() {
        let (root, service) = prepare_service();
        let workspace = root.path().join("workspace-version-fence");
        fs::create_dir_all(&workspace).unwrap();
        let (_project, pulse, job, _agent_run_id) =
            claimed_cli_job_fixture(&service, workspace.to_string_lossy().as_ref());

        let mut connection = service.connection().expect("database should open");
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .expect("transaction should start");
        let current = PersistenceService::find_pulsation_by_id(&transaction, &pulse.id)
            .expect("pulse should load")
            .expect("pulse should exist");
        PersistenceService::transition_pulsation(
            &transaction,
            &current,
            PulsationStatus::Planning,
            current.stage,
            "cli_backend_elected",
            Some("OpenCode falhou (cli_not_found); a etapa será repetida em Codex."),
        )
        .expect("election should transition the pulsation");
        let changed = transaction
            .execute(
                "UPDATE jobs SET status='pending',generation=generation+1,available_at=NULL,last_error=?2,updated_at=?3,\
                 state_version=(SELECT state_version FROM pulsations WHERE id=?4) WHERE id=?1 AND status='running'",
                params![
                    job.id,
                    "execução CLI falhou: OpenCode não está pronto",
                    PersistenceService::now_iso(),
                    pulse.id
                ],
            )
            .expect("job requeue should apply");
        assert_eq!(changed, 1);
        transaction.commit().expect("transaction should commit");
        // The real failure path releases the job's runtime slot before
        // electing a new backend (the job stopped executing); the fixture's
        // initial claim reserved one and nothing has freed it since.
        service
            .connection()
            .expect("database should open")
            .execute("DELETE FROM runtime_slots WHERE job_id=?1", [&job.id])
            .expect("runtime slot should release");

        let (reclaimed, _cancellation) = service
            .claim_next_pending_job()
            .expect("claim should succeed")
            .expect("a job requeued after a backend switch must be reclaimable");
        assert_eq!(reclaimed.id, job.id);
    }

    /// The other half of the same fix: the corrected UPDATE also checks
    /// `rows_affected`, so a race where the job stopped being `running`
    /// between the failure being reported and this transaction running
    /// (e.g. a concurrent cancellation) is caught instead of silently
    /// reporting success while touching nothing.
    #[test]
    fn backend_switch_update_matches_nothing_once_the_job_left_running() {
        let (root, service) = prepare_service();
        let workspace = root.path().join("workspace-version-fence-race");
        fs::create_dir_all(&workspace).unwrap();
        let (_project, pulse, job, _agent_run_id) =
            claimed_cli_job_fixture(&service, workspace.to_string_lossy().as_ref());
        service
            .connection()
            .expect("database should open")
            .execute("UPDATE jobs SET status='failed' WHERE id=?1", [&job.id])
            .expect("job status should update");

        let mut connection = service.connection().expect("database should open");
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .expect("transaction should start");
        let current = PersistenceService::find_pulsation_by_id(&transaction, &pulse.id)
            .expect("pulse should load")
            .expect("pulse should exist");
        PersistenceService::transition_pulsation(
            &transaction,
            &current,
            PulsationStatus::Planning,
            current.stage,
            "cli_backend_elected",
            Some("OpenCode falhou (cli_not_found); a etapa será repetida em Codex."),
        )
        .expect("election should transition the pulsation");
        let changed = transaction
            .execute(
                "UPDATE jobs SET status='pending',generation=generation+1,available_at=NULL,last_error=?2,updated_at=?3,\
                 state_version=(SELECT state_version FROM pulsations WHERE id=?4) WHERE id=?1 AND status='running'",
                params![
                    job.id,
                    "execução CLI falhou: OpenCode não está pronto",
                    PersistenceService::now_iso(),
                    pulse.id
                ],
            )
            .expect("update should run even though it matches nothing");
        assert_eq!(
            changed, 0,
            "a job that already left 'running' must not be silently requeued"
        );
    }

    #[test]
    fn repair_state_version_drift_makes_a_stranded_pending_job_claimable_again() {
        let (root, service) = prepare_service();
        let workspace = root.path().join("workspace-drift");
        fs::create_dir_all(&workspace).unwrap();
        let (_project, pulse, job, _agent_run_id) =
            claimed_cli_job_fixture(&service, workspace.to_string_lossy().as_ref());

        // Reproduce the exact state the unfixed elect_next_backend left
        // behind: the pulsation moves to a new state_version, but the job
        // is put back to 'pending' still carrying the old one.
        let connection = service.connection().expect("database should open");
        connection
            .execute(
                "UPDATE pulsations SET state_version = state_version + 1 WHERE id = ?1",
                [&pulse.id],
            )
            .expect("pulsation version should bump");
        connection
            .execute(
                "UPDATE jobs SET status='pending', available_at=NULL WHERE id=?1",
                [&job.id],
            )
            .expect("job should be stranded pending");
        assert!(
            service
                .claim_next_pending_job()
                .expect("claim should succeed")
                .is_none(),
            "precondition: the drifted job must be stranded before the repair runs"
        );
        // The real failure path releases the job's runtime slot before the
        // job ever goes back to 'pending'; the fixture's initial claim
        // reserved one and nothing has freed it since.
        connection
            .execute("DELETE FROM runtime_slots WHERE job_id=?1", [&job.id])
            .expect("runtime slot should release");

        let repaired = service
            .repair_state_version_drift()
            .expect("repair should succeed");
        assert_eq!(repaired, vec![pulse.id.clone()]);

        let (reclaimed, _cancellation) = service
            .claim_next_pending_job()
            .expect("claim should succeed")
            .expect("the repaired job must be claimable again");
        assert_eq!(reclaimed.id, job.id);

        let event_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM pulse_events WHERE pulsation_id=?1 AND event_type='state_version_drift_repaired'",
                [&pulse.id],
                |row| row.get(0),
            )
            .expect("event count should query");
        assert_eq!(event_count, 1);
    }

    #[test]
    fn repair_state_version_drift_is_a_no_op_on_a_healthy_database() {
        let (_root, service) = prepare_service();
        assert_eq!(
            service.repair_state_version_drift().unwrap(),
            Vec::<String>::new()
        );
    }

    fn pulsation_with_override_fixture(
        service: &PersistenceService,
        workspace_path: &str,
    ) -> (Pulsation, AgentRole) {
        let project = service
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: workspace_path.to_string(),
                preferred_name: Some("Clear override fixture".to_string()),
            })
            .expect("project should be created")
            .project;
        let pulse = service
            .create_pulsation(CreatePulsationRequest {
                project_id: project.id.clone(),
                feature_name: "Clear override".to_string(),
            })
            .expect("pulse should be created")
            .pulsation;
        let role = AgentRole::for_stage(pulse.stage.as_db_value());
        service
            .connection()
            .expect("database should open")
            .execute(
                "INSERT INTO pulse_execution_overrides(pulsation_id,agent_role,execution_mode,backend,source_checkpoint_id,created_at) \
                 VALUES(?1,?2,'cli_process','codex',NULL,?3)",
                params![pulse.id, role.as_str(), PersistenceService::now_iso()],
            )
            .expect("override should be seeded");
        (pulse, role)
    }

    fn count_overrides(service: &PersistenceService, pulsation_id: &str) -> i64 {
        service
            .connection()
            .expect("database should open")
            .query_row(
                "SELECT COUNT(*) FROM pulse_execution_overrides WHERE pulsation_id=?1",
                [pulsation_id],
                |row| row.get(0),
            )
            .expect("override count should query")
    }

    #[test]
    fn clear_execution_override_and_retry_wakes_a_pending_job_on_the_default_backend() {
        let (root, service) = prepare_service();
        let workspace = root.path().join("workspace-clear-override-pending");
        fs::create_dir_all(&workspace).unwrap();
        let (pulse, role) =
            pulsation_with_override_fixture(&service, workspace.to_string_lossy().as_ref());

        let response = service
            .clear_execution_override_and_retry(ClearExecutionOverrideRequest {
                pulsation_id: pulse.id.clone(),
                agent_role: role,
            })
            .expect("clearing a pending-job override should succeed");
        assert!(response.cleared);
        assert!(response.retried);
        assert_eq!(count_overrides(&service, &pulse.id), 0);

        let (reclaimed, _cancellation) = service
            .claim_next_pending_job()
            .expect("claim should succeed")
            .expect("the woken job must still be claimable on the default backend");
        assert_eq!(reclaimed.pulsation_id.as_deref(), Some(pulse.id.as_str()));
    }

    #[test]
    fn clear_execution_override_and_retry_refuses_while_the_job_is_running() {
        let (root, service) = prepare_service();
        let workspace = root.path().join("workspace-clear-override-running");
        fs::create_dir_all(&workspace).unwrap();
        let (pulse, role) =
            pulsation_with_override_fixture(&service, workspace.to_string_lossy().as_ref());
        service
            .claim_next_pending_job()
            .expect("claim should succeed")
            .expect("the stage job should be claimable");

        let error = service
            .clear_execution_override_and_retry(ClearExecutionOverrideRequest {
                pulsation_id: pulse.id.clone(),
                agent_role: role,
            })
            .expect_err("clearing while the job is running must be refused");
        assert!(matches!(error, PersistenceServiceError::Validation(_)));
        assert_eq!(
            count_overrides(&service, &pulse.id),
            1,
            "a refused clear must leave the override untouched"
        );
    }

    #[test]
    fn clear_execution_override_and_retry_is_a_harmless_no_op_when_nothing_to_clear() {
        let (root, service) = prepare_service();
        let workspace = root.path().join("workspace-clear-override-none");
        fs::create_dir_all(&workspace).unwrap();
        let project = service
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: workspace.to_string_lossy().into_owned(),
                preferred_name: Some("No override fixture".to_string()),
            })
            .unwrap()
            .project;
        let pulse = service
            .create_pulsation(CreatePulsationRequest {
                project_id: project.id.clone(),
                feature_name: "No override".to_string(),
            })
            .unwrap()
            .pulsation;
        let role = AgentRole::for_stage(pulse.stage.as_db_value());

        let response = service
            .clear_execution_override_and_retry(ClearExecutionOverrideRequest {
                pulsation_id: pulse.id,
                agent_role: role,
            })
            .expect("a no-op clear must not error");
        assert!(!response.cleared);
        assert!(!response.retried);
    }

    /// Field incident, 2026-09-10: the Integrator failed five times on the same
    /// backend across three human checkpoints, and what finally worked was the
    /// developer switching backend by hand in project settings, mid-run. The
    /// host has the same information and can make that move itself; the human
    /// checkpoint belongs at the end of the ladder, not at its first rung.
    #[test]
    fn the_backend_ladder_skips_what_a_pulsation_already_burned_and_then_gives_up() {
        let available = [CliBackend::Codex, CliBackend::Claude, CliBackend::Gemini];

        // Nothing burned yet: the first healthy backend wins.
        assert_eq!(
            PersistenceService::elect_backend(&available, &[]),
            Some(CliBackend::Codex)
        );
        // A burned backend is never re-elected, whatever its position.
        assert_eq!(
            PersistenceService::elect_backend(&available, &["codex".to_string()]),
            Some(CliBackend::Claude)
        );
        assert_eq!(
            PersistenceService::elect_backend(
                &available,
                &["codex".to_string(), "claude".to_string()]
            ),
            Some(CliBackend::Gemini)
        );
        // Exhausted: this is where the human checkpoint is the right answer.
        assert_eq!(
            PersistenceService::elect_backend(
                &available,
                &[
                    "codex".to_string(),
                    "claude".to_string(),
                    "gemini".to_string()
                ]
            ),
            None
        );
        // Nothing installed is also exhausted, not a panic.
        assert_eq!(PersistenceService::elect_backend(&[], &[]), None);
    }

    #[test]
    fn only_faults_a_different_cli_could_survive_trigger_a_backend_switch() {
        for code in [
            "cli_process_failed",
            "cli_timeout",
            "cli_inactivity_timeout",
            "cli_auth_required",
            "cli_version_unsupported",
            "cli_rate_limited",
            "cli_not_found",
            "cli_output_limit",
            "cli_output_invalid",
        ] {
            assert!(
                PersistenceService::backend_switch_can_help(code),
                "{code} is a transport fault another CLI could survive"
            );
        }
        // These travel with the host: another CLI spends a call to fail the
        // same way.
        for code in [
            "cli_cancelled",
            "cli_configuration_invalid",
            "cli_security_failed",
            "cli_persistence_failed",
            "cli_redaction_failed",
            "cli_invalid_state_transition",
            "cli_output_truncated",
            "cli_output_budget_exceeded",
        ] {
            assert!(
                !PersistenceService::backend_switch_can_help(code),
                "{code} must not spend another backend"
            );
        }
    }

    /// The ladder writes a CLI backend into the same row the API fallback uses.
    /// Choosing "use the API" afterwards has to clear it, or the row claims to
    /// be `api_direct` while still naming a backend and the write is rejected.
    #[test]
    fn authorizing_the_api_clears_a_backend_the_ladder_had_elected() {
        let (root, service) = prepare_service();
        let workspace = root.path().join("workspace-override-swap");
        fs::create_dir_all(&workspace).unwrap();
        let (_project, pulse, _job, _agent_run_id) =
            claimed_cli_job_fixture(&service, workspace.to_string_lossy().as_ref());
        let connection = service.connection().unwrap();
        connection
            .execute(
                "INSERT INTO pulse_execution_overrides(pulsation_id,agent_role,execution_mode,backend,source_checkpoint_id,created_at) VALUES(?1,'integrator','cli_process','gemini',NULL,?2)",
                params![pulse.id, PersistenceService::now_iso()],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO pulse_execution_overrides(pulsation_id,agent_role,execution_mode,source_checkpoint_id,created_at) VALUES(?1,'integrator','api_direct',NULL,?2) ON CONFLICT(pulsation_id,agent_role) DO UPDATE SET execution_mode='api_direct',backend=NULL,source_checkpoint_id=NULL,created_at=excluded.created_at",
                params![pulse.id, PersistenceService::now_iso()],
            )
            .expect("switching an elected backend to the API must be a legal write");
        let (mode, backend): (String, Option<String>) = connection
            .query_row(
                "SELECT execution_mode,backend FROM pulse_execution_overrides WHERE pulsation_id=?1 AND agent_role='integrator'",
                params![pulse.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(mode, "api_direct");
        assert_eq!(backend, None);
    }

    #[test]
    fn cli_failure_requires_explicit_api_authorization_and_requeues_same_job() {
        let (root, service) = prepare_service();
        let workspace = root.path().join("workspace-api-fallback");
        fs::create_dir_all(&workspace).unwrap();
        let (project, pulse, job, agent_run_id) =
            claimed_cli_job_fixture(&service, workspace.to_string_lossy().as_ref());

        service
            .open_cli_fallback(
                &job,
                &agent_run_id,
                "cli_process_failed",
                "Codex encerrou com código 1.",
            )
            .expect("fallback checkpoint should open");
        service
            .release_runtime_slot(&PersistenceService::job_slot_token(&job))
            .unwrap();

        let fallbacks = service
            .list_cli_fallbacks(&project.id)
            .expect("fallbacks should load");
        assert_eq!(fallbacks.len(), 1);
        assert_eq!(fallbacks[0].pulsation_id, pulse.id);
        assert_eq!(fallbacks[0].agent_role, AgentRole::IntakeRouter);
        let checkpoint = fallbacks[0].clone();

        let response = service
            .resolve_cli_fallback(ResolveCliFallbackRequest {
                checkpoint_id: checkpoint.checkpoint_id.clone(),
                expected_state_version: checkpoint.state_version,
                decision: CliFallbackDecision::UseApi,
            })
            .expect("API authorization should resolve fallback");
        assert_eq!(response.pulsation.status, PulsationStatus::Planning);
        let receipt = service
            .checkpoint_receipt(&checkpoint.checkpoint_id)
            .unwrap()
            .expect("the fallback decision has a receipt");
        assert_eq!(receipt.purpose, "cli_fallback");
        assert_eq!(receipt.decision, "use_api");
        assert_eq!(receipt.state_version, checkpoint.state_version);
        let connection = service.connection().unwrap();
        let (job_status, cancellation_reason): (String, Option<String>) = connection
            .query_row(
                "SELECT status,cancellation_reason FROM jobs WHERE id=?1",
                [&job.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(job_status, "pending");
        assert!(cancellation_reason.is_none());
        let override_exists: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM pulse_execution_overrides
                 WHERE pulsation_id=?1 AND agent_role='intake_router')",
                [&pulse.id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(override_exists);
        assert!(service.list_cli_fallbacks(&project.id).unwrap().is_empty());
        assert!(matches!(
            service.resolve_cli_fallback(ResolveCliFallbackRequest {
                checkpoint_id: checkpoint.checkpoint_id,
                expected_state_version: checkpoint.state_version,
                decision: CliFallbackDecision::RetryCli,
            }),
            Err(PersistenceServiceError::Validation(_))
        ));
    }

    #[test]
    fn cancelling_cli_fallback_terminates_the_pulse() {
        let (root, service) = prepare_service();
        let workspace = root.path().join("workspace-cancel-fallback");
        fs::create_dir_all(&workspace).unwrap();
        let (project, pulse, job, agent_run_id) =
            claimed_cli_job_fixture(&service, workspace.to_string_lossy().as_ref());
        service
            .open_cli_fallback(
                &job,
                &agent_run_id,
                "cli_not_found",
                "Executável não encontrado.",
            )
            .unwrap();
        service
            .release_runtime_slot(&PersistenceService::job_slot_token(&job))
            .unwrap();
        let checkpoint = service.list_cli_fallbacks(&project.id).unwrap().remove(0);

        let response = service
            .resolve_cli_fallback(ResolveCliFallbackRequest {
                checkpoint_id: checkpoint.checkpoint_id,
                expected_state_version: checkpoint.state_version,
                decision: CliFallbackDecision::Cancel,
            })
            .expect("cancel decision should resolve fallback");
        assert_eq!(response.pulsation.status, PulsationStatus::Failed);
        assert_eq!(
            response.pulsation.terminal_reason.as_deref(),
            Some("user_cancel")
        );
        let connection = service.connection().unwrap();
        let terminal_reason: String = connection
            .query_row(
                "SELECT terminal_reason FROM pulsations WHERE id=?1",
                [&pulse.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(terminal_reason, "user_cancel");
        let status: String = connection
            .query_row("SELECT status FROM jobs WHERE id=?1", [&job.id], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(status, "failed");
    }

    #[test]
    fn migrates_v4_database_to_v9_without_losing_legacy_settings() {
        let root = tempfile::tempdir().expect("tempdir should be created");
        let db_path = root.path().join("hard-vibe-code.db");
        let mut connection = Connection::open(&db_path).expect("v4 db should open");
        embedded::migrations::runner()
            .set_target(refinery::Target::Version(4))
            .run(&mut connection)
            .expect("v1-v4 should apply");
        connection
            .execute(
                "INSERT INTO runtime_settings(key,value,updated_at) VALUES ('runtime.provider','anthropic',?1)",
                params![Utc::now().to_rfc3339()],
            )
            .expect("legacy setting should insert");
        drop(connection);

        let service = PersistenceService::initialize(root.path()).expect("v5-v9 should apply");
        let connection = service.connection().expect("migrated db should open");
        for table in [
            "provider_connections",
            "provider_routes",
            "provider_route_targets",
            "provider_route_bindings",
            "provider_circuit_breakers",
            "provider_model_lockouts",
            "llm_route_executions",
            "llm_attempts",
            "llm_route_affinity",
            "llm_compression_metrics",
            "chat_conversations",
            "chat_messages",
            "chat_model_calls",
            "chat_tool_calls",
        ] {
            let exists: bool = connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                    params![table],
                    |row| row.get(0),
                )
                .expect("schema lookup should work");
            assert!(exists, "table {table} should exist after V9");
        }
        let active_view_default: String = connection
            .query_row(
                "SELECT dflt_value FROM pragma_table_info('ui_sessions') WHERE name='active_view'",
                [],
                |row| row.get(0),
            )
            .expect("active_view should be added by V8");
        assert_eq!(active_view_default, "'workspace'");
        let provider: String = connection
            .query_row(
                "SELECT value FROM runtime_settings WHERE key='runtime.provider'",
                [],
                |row| row.get(0),
            )
            .expect("legacy provider should remain");
        assert_eq!(provider, "anthropic");
        let route_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM provider_routes WHERE id='legacy-default'",
                [],
                |row| row.get(0),
            )
            .expect("legacy route should exist");
        assert_eq!(route_count, 1);
    }

    #[test]
    fn migrates_v1_database_to_v9_and_round_trips_legacy_session() {
        let root = tempfile::tempdir().expect("tempdir should be created");
        let db_path = root.path().join("hard-vibe-code.db");
        let mut connection = Connection::open(&db_path).expect("v1 db should open");
        embedded::migrations::runner()
            .set_target(refinery::Target::Version(1))
            .run(&mut connection)
            .expect("v1 should apply");
        let now = Utc::now().to_rfc3339();
        connection
            .execute(
                "INSERT INTO projects (id,name,workspace_path,is_archived,created_at,updated_at)
                 VALUES ('project-v1','Projeto V1','C:/workspace/v1',0,?1,?1)",
                params![now],
            )
            .expect("v1 project should insert");
        connection
            .execute(
                "INSERT INTO ui_sessions (
                   project_id,workspace_root,active_tab_path,workspace_view_mode,scroll_top,updated_at
                 ) VALUES (
                   'project-v1','C:/workspace/v1','notes/legacy.md','editor',33.0,?1
                 )",
                params![now],
            )
            .expect("v1 session should insert");
        connection
            .execute(
                "INSERT INTO ui_session_tabs (
                   project_id,tab_order,relative_path,draft_content,base_revision,
                   cursor_line,cursor_column,scroll_top,created_at,updated_at
                 ) VALUES (
                   'project-v1',0,'notes/legacy.md','# legado','1:1',3,7,31.0,?1,?1
                 )",
                params![now],
            )
            .expect("v1 tab should insert");
        drop(connection);

        let service = PersistenceService::initialize(root.path()).expect("v2-v9 should apply");
        let migrated = service
            .load_ui_session(LoadUiSessionRequest {
                project_id: "project-v1".into(),
            })
            .expect("migrated session should load")
            .session
            .expect("legacy session should remain");

        assert_eq!(migrated.active_view, "workspace");
        assert_eq!(migrated.workspace_view_mode, "editor");
        assert!(!migrated.sidebar_collapsed);
        assert!(migrated.context_drawer_open);
        assert_eq!(migrated.active_tab_path.as_deref(), Some("notes/legacy.md"));
        assert_eq!(migrated.scroll_top, Some(33.0));
        assert_eq!(migrated.tabs.len(), 1);
        assert_eq!(migrated.tabs[0].draft_content.as_deref(), Some("# legado"));
        assert_eq!(migrated.tabs[0].cursor_line, Some(3));
        assert_eq!(migrated.tabs[0].cursor_column, Some(7));

        let chat_table_exists: bool = service
            .connection()
            .expect("migrated db should open")
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='chat_messages')",
                [],
                |row| row.get(0),
            )
            .expect("chat schema lookup should work");
        assert!(chat_table_exists);
    }

    #[test]
    fn migrates_v8_database_to_v9_without_losing_ui_session() {
        let root = tempfile::tempdir().expect("tempdir should be created");
        let db_path = root.path().join("hard-vibe-code.db");
        let mut connection = Connection::open(&db_path).expect("v8 db should open");
        embedded::migrations::runner()
            .set_target(refinery::Target::Version(8))
            .run(&mut connection)
            .expect("v1-v8 should apply");
        let now = Utc::now().to_rfc3339();
        connection
            .execute(
                "INSERT INTO projects (id,name,workspace_path,is_archived,created_at,updated_at)
                 VALUES ('project-v8','Projeto V8','C:/workspace/v8',0,?1,?1)",
                params![now],
            )
            .expect("v8 project should insert");
        connection
            .execute(
                "INSERT INTO ui_sessions (
                   project_id,workspace_root,active_tab_path,workspace_view_mode,active_view,
                   sidebar_collapsed,context_drawer_open,active_pulsation_id,selected_vault_path,
                   scroll_top,updated_at
                 ) VALUES (
                   'project-v8','C:/workspace/v8','notes/draft.md','hitl','vault',1,0,
                   'pulse-v8','notes/draft.md',81.5,?1
                 )",
                params![now],
            )
            .expect("v8 session should insert");
        connection
            .execute(
                "INSERT INTO ui_session_tabs (
                   project_id,tab_order,relative_path,draft_content,base_revision,
                   cursor_line,cursor_column,scroll_top,created_at,updated_at
                 ) VALUES (
                   'project-v8',0,'notes/draft.md','# V8 draft','8:1',9,4,75.0,?1,?1
                 )",
                params![now],
            )
            .expect("v8 tab should insert");
        drop(connection);

        let service = PersistenceService::initialize(root.path()).expect("v9 should apply");
        let migrated = service
            .load_ui_session(LoadUiSessionRequest {
                project_id: "project-v8".into(),
            })
            .expect("session should load")
            .session
            .expect("session should remain");
        assert_eq!(migrated.active_view, "vault");
        assert_eq!(migrated.workspace_view_mode, "hitl");
        assert!(migrated.sidebar_collapsed);
        assert!(!migrated.context_drawer_open);
        assert_eq!(migrated.active_tab_path.as_deref(), Some("notes/draft.md"));
        assert_eq!(migrated.active_pulsation_id.as_deref(), Some("pulse-v8"));
        assert_eq!(
            migrated.selected_vault_path.as_deref(),
            Some("notes/draft.md")
        );
        assert_eq!(migrated.scroll_top, Some(81.5));
        assert!(migrated.active_chat_conversation_id.is_none());
        assert_eq!(migrated.tabs.len(), 1);
        assert_eq!(migrated.tabs[0].relative_path, "notes/draft.md");
        assert_eq!(
            migrated.tabs[0].draft_content.as_deref(),
            Some("# V8 draft")
        );
        assert_eq!(migrated.tabs[0].base_revision.as_deref(), Some("8:1"));
        assert_eq!(migrated.tabs[0].cursor_line, Some(9));
        assert_eq!(migrated.tabs[0].cursor_column, Some(4));
        assert_eq!(migrated.tabs[0].scroll_top, Some(75.0));
        let chat_table_exists: bool = service
            .connection()
            .unwrap()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='chat_conversations')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(chat_table_exists);
    }

    #[test]
    fn provider_configuration_never_persists_api_key_and_restricts_referenced_delete() {
        let (_root, service) = prepare_service();
        let created = service
            .upsert_provider_connection(UpsertProviderConnectionRequest {
                id: Some("ollama-test".to_string()),
                provider_id: "ollama".to_string(),
                display_name: "Ollama Test".to_string(),
                endpoint_url: Some("http://127.0.0.1:11434".to_string()),
                default_model: "fixture-model".to_string(),
                enabled: Some(true),
                max_concurrency: Some(1),
                api_key: None,
                version: None,
            })
            .expect("ollama connection should save")
            .connection;
        service
            .upsert_provider_route(UpsertProviderRouteRequest {
                id: Some("route-test".to_string()),
                name: "Route Test".to_string(),
                strategy: "priority".to_string(),
                max_attempts: Some(3),
                attempt_timeout_ms: Some(30_000),
                route_timeout_ms: Some(45_000),
                enabled: Some(true),
                targets: vec![crate::llm::RouteTarget {
                    position: 0,
                    connection_id: created.id.clone(),
                    model_id: created.default_model.clone(),
                    enabled: true,
                }],
                version: None,
            })
            .expect("route should save");
        let error = service
            .delete_provider_connection(DeleteProviderConnectionRequest {
                connection_id: created.id.clone(),
                version: created.version,
            })
            .expect_err("referenced connection should be restricted");
        assert!(
            matches!(error, PersistenceServiceError::Llm(ref error) if error.kind == LlmErrorKind::RouteConflict)
        );

        let connection = service.connection().expect("db should open");
        let secret_columns: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('provider_connections') WHERE lower(name) LIKE '%api_key%' OR lower(name) LIKE '%secret%'",
                [],
                |row| row.get(0),
            )
            .expect("schema should inspect");
        assert_eq!(secret_columns, 0);
        let serialized = serde_json::to_string(&created).expect("connection should serialize");
        assert!(!serialized.contains("credential_ref"));
        assert!(!serialized.contains("credentialRef"));
    }

    #[test]
    fn exact_binding_resolves_only_matching_project_role_and_stage() {
        let (workspace, service) = prepare_service();
        let project = service
            .create_project(CreateProjectRequest {
                workspace_path: workspace
                    .path()
                    .join("workspace")
                    .to_string_lossy()
                    .to_string(),
                name: Some("Bindings".to_string()),
            })
            .expect("project should save")
            .project;
        service
            .upsert_provider_route_binding(UpsertProviderRouteBindingRequest {
                project_id: project.id.clone(),
                agent_role: "backend_agent".to_string(),
                stage: "Code".to_string(),
                route_id: "legacy-default".to_string(),
                enabled: Some(true),
                version: None,
            })
            .expect("binding should save");
        let bound = service
            .llm_repository
            .resolve_route(&project.id, crate::llm::AgentRole::BackendAgent, "Code")
            .expect("exact route should resolve");
        assert_eq!(bound.id, "legacy-default");
        let fallback = service
            .llm_repository
            .resolve_route(&project.id, crate::llm::AgentRole::TestAgent, "Test")
            .expect("missing exact binding should fallback");
        assert_eq!(fallback.id, "legacy-default");
        assert_eq!(
            service
                .list_provider_route_bindings()
                .unwrap()
                .bindings
                .len(),
            1
        );
    }

    #[test]
    fn worker_does_not_claim_job_before_available_at() {
        let (root, service) = prepare_service();
        let project = service
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: root.path().to_string_lossy().into_owned(),
                preferred_name: None,
            })
            .unwrap()
            .project;
        let pulse = service
            .create_pulsation(CreatePulsationRequest {
                project_id: project.id,
                feature_name: "Future job".into(),
            })
            .unwrap()
            .pulsation;
        let connection = service.connection().expect("db should open");
        let now = Utc::now();
        connection
            .execute(
                "UPDATE jobs SET id='future-job',available_at=?2 WHERE pulsation_id=?1",
                params![pulse.id, (now + chrono::Duration::minutes(5)).to_rfc3339()],
            )
            .expect("future job should insert");
        assert!(service.claim_next_pending_job().unwrap().is_none());
        connection
            .execute(
                "UPDATE jobs SET available_at=?1 WHERE id='future-job'",
                params![(now - chrono::Duration::seconds(1)).to_rfc3339()],
            )
            .expect("job should become available");
        let (job, _token) = service
            .claim_next_pending_job()
            .expect("claim should run")
            .expect("job should be claimable");
        assert_eq!(job.id, "future-job");
        assert!(
            service
                .runtime_control
                .cancellations
                .lock()
                .unwrap()
                .contains_key("future-job")
        );
        service.unregister_job_cancellation("future-job");
    }

    #[test]
    fn api_export_distinguishes_original_evidence_from_the_sanitized_envelope() {
        let (workspace, service) = prepare_service();
        let project = service
            .create_project(CreateProjectRequest {
                workspace_path: workspace
                    .path()
                    .join("api-export")
                    .to_string_lossy()
                    .into_owned(),
                name: Some("API export".into()),
            })
            .unwrap()
            .project;
        let original = r#"{"password":"synthetic multi word secret","note":"legacy"}"#;
        let db = service.connection().unwrap();
        db.execute(
            "INSERT INTO llm_route_executions(id,generation_id,project_id,route_id,agent_role,stage,strategy,mode,status,decision_json,visible_output,visible_output_complete,started_at)
             VALUES ('export-execution','generation',?1,'legacy-default','backend_agent','Code','priority','on','completed','{}',?2,1,?3)",
            params![project.id, original, Utc::now().to_rfc3339()],
        ).unwrap();
        let (_, evidence_sha) = service
            .llm_repository
            .execution_snapshot_with_evidence("export-execution")
            .unwrap();
        let request = GetAgentApiExecutionSnapshotRequest {
            project_id: project.id.clone(),
            target: crate::llm::AgentExecutionTarget::canonical(
                crate::llm::AgentRole::BackendAgent,
            ),
            execution_id: "export-execution".into(),
        };
        let path = service.export_agent_api_execution(request.clone()).unwrap();
        let bytes = fs::read_to_string(&path).unwrap();
        assert!(!bytes.contains("synthetic multi word secret"));
        assert!(bytes.contains("<REDACTED_SECRET:"));
        let mut export: serde_json::Value = serde_json::from_str(&bytes).unwrap();
        let metadata = export
            .as_object_mut()
            .unwrap()
            .remove("sanitization")
            .unwrap();
        assert_eq!(metadata["originalEvidenceSha256"], evidence_sha);
        assert_eq!(
            metadata["sanitizedContentSha256"],
            crate::secret_redactor::text_sha256(
                &crate::llm::repository::canonical_evidence_json(&export).unwrap()
            )
        );
        assert_ne!(
            metadata["originalEvidenceSha256"],
            metadata["sanitizedContentSha256"]
        );
        service.export_agent_api_execution(request).unwrap();
        assert_eq!(fs::read_to_string(path).unwrap(), bytes);
        let persisted: String = db
            .query_row(
                "SELECT visible_output FROM llm_route_executions WHERE id='export-execution'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(persisted, original);
    }

    #[test]
    fn finishing_cancelled_stream_withholds_unqualified_partial_output() {
        let (workspace, service) = prepare_service();
        let project = service
            .create_project(CreateProjectRequest {
                workspace_path: workspace
                    .path()
                    .join("cancelled-stream")
                    .to_string_lossy()
                    .to_string(),
                name: Some("Cancelled stream".to_string()),
            })
            .unwrap()
            .project;
        service
            .connection()
            .unwrap()
            .execute(
                "INSERT INTO llm_route_executions(id,generation_id,project_id,route_id,agent_role,stage,strategy,mode,status,decision_json,visible_output_complete,started_at)
                 VALUES ('cancelled-execution','generation',?1,'legacy-default','backend_agent','Code','priority','on','running','{}',0,?2)",
                params![project.id, Utc::now().to_rfc3339()],
            )
            .unwrap();
        service
            .llm_repository
            .update_visible_output("cancelled-execution", "partial")
            .unwrap();
        service
            .llm_repository
            .finish_execution("cancelled-execution", "cancelled", None, None, false)
            .unwrap();
        let snapshot = service
            .llm_repository
            .execution_snapshot("cancelled-execution")
            .unwrap();
        assert_eq!(snapshot.status, "cancelled");
        assert!(snapshot.visible_output.is_none());
        assert!(!snapshot.visible_output_complete);
    }

    #[test]
    fn half_open_breaker_allows_only_one_probe() {
        let (_root, service) = prepare_service();
        let connection = service.connection().expect("db should open");
        let past = (Utc::now() - chrono::Duration::seconds(1)).to_rfc3339();
        connection
            .execute(
                "INSERT INTO provider_circuit_breakers(provider_id,endpoint_origin,state,failure_count,probe_after,probe_in_flight,version,updated_at)
                 VALUES ('openai','https://api.openai.com','open',5,?1,0,1,?2)",
                params![past, Utc::now().to_rfc3339()],
            )
            .expect("breaker should insert");
        assert!(
            service
                .llm_repository
                .claim_breaker_probe("openai", "https://api.openai.com/v1/responses", Utc::now())
                .unwrap()
        );
        assert!(
            !service
                .llm_repository
                .claim_breaker_probe("openai", "https://api.openai.com/v1/responses", Utc::now())
                .unwrap()
        );
    }

    #[test]
    fn breaker_thresholds_use_the_injected_clock_value() {
        let (_root, service) = prepare_service();
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-18T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        for _ in 0..3 {
            service
                .llm_repository
                .record_health_failure("openai", "https://api.openai.com", false, now)
                .unwrap();
        }
        assert_eq!(
            service
                .llm_repository
                .breaker_state("openai", "https://api.openai.com")
                .unwrap()
                .as_deref(),
            Some("degraded")
        );
        for _ in 0..2 {
            service
                .llm_repository
                .record_health_failure("openai", "https://api.openai.com", false, now)
                .unwrap();
        }
        assert_eq!(
            service
                .llm_repository
                .breaker_state("openai", "https://api.openai.com")
                .unwrap()
                .as_deref(),
            Some("open")
        );
        let probe_after: String = service
            .connection()
            .unwrap()
            .query_row(
                "SELECT probe_after FROM provider_circuit_breakers WHERE provider_id='openai' AND endpoint_origin='https://api.openai.com'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            chrono::DateTime::parse_from_rfc3339(&probe_after)
                .unwrap()
                .with_timezone(&Utc),
            now + chrono::Duration::seconds(30)
        );
    }

    #[test]
    fn router_falls_back_to_second_target_after_429() {
        use crate::llm::{
            AgentRole, ExecutionId, GenerationId, LlmGateway, LlmRequest, ResponseFormat,
        };
        use std::time::Instant;

        let (first_endpoint, first_server) = spawn_http_response(
            "429 Too Many Requests",
            "Retry-After: 2\r\n",
            r#"{"error":{"message":"rate limited","code":"rate_limit"}}"#,
        );
        let (second_endpoint, second_server) = spawn_http_response(
            "200 OK",
            "",
            r#"{"model":"fixture","choices":[{"message":{"content":"OK"},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":1}}"#,
        );
        let (workspace, service) = prepare_service();
        let project = service
            .create_project(CreateProjectRequest {
                workspace_path: workspace
                    .path()
                    .join("routing-e2e")
                    .to_string_lossy()
                    .to_string(),
                name: Some("Routing E2E".to_string()),
            })
            .expect("project should save")
            .project;
        let first = service
            .upsert_provider_connection(UpsertProviderConnectionRequest {
                id: Some("ollama-rate-limited".to_string()),
                provider_id: "ollama".to_string(),
                display_name: "Rate limited".to_string(),
                endpoint_url: Some(first_endpoint),
                default_model: "fixture".to_string(),
                enabled: Some(true),
                max_concurrency: Some(1),
                api_key: None,
                version: None,
            })
            .unwrap()
            .connection;
        let second = service
            .upsert_provider_connection(UpsertProviderConnectionRequest {
                id: Some("ollama-success".to_string()),
                provider_id: "ollama".to_string(),
                display_name: "Success".to_string(),
                endpoint_url: Some(second_endpoint),
                default_model: "fixture".to_string(),
                enabled: Some(true),
                max_concurrency: Some(1),
                api_key: None,
                version: None,
            })
            .unwrap()
            .connection;
        service
            .upsert_provider_route(UpsertProviderRouteRequest {
                id: Some("fallback-e2e".to_string()),
                name: "Fallback E2E".to_string(),
                strategy: "priority".to_string(),
                max_attempts: Some(3),
                attempt_timeout_ms: Some(5_000),
                route_timeout_ms: Some(10_000),
                enabled: Some(true),
                targets: vec![
                    crate::llm::RouteTarget {
                        position: 0,
                        connection_id: first.id.clone(),
                        model_id: "fixture".to_string(),
                        enabled: true,
                    },
                    crate::llm::RouteTarget {
                        position: 1,
                        connection_id: second.id.clone(),
                        model_id: "fixture".to_string(),
                        enabled: true,
                    },
                ],
                version: None,
            })
            .unwrap();
        service
            .upsert_provider_route_binding(UpsertProviderRouteBindingRequest {
                project_id: project.id.clone(),
                agent_role: "backend_agent".to_string(),
                stage: "Code".to_string(),
                route_id: "fallback-e2e".to_string(),
                enabled: Some(true),
                version: None,
            })
            .unwrap();
        service.llm_repository.set_routing_mode("on").unwrap();
        let request = LlmRequest {
            execution_id: ExecutionId::new(Uuid::new_v4().to_string()),
            generation_id: GenerationId::new(Uuid::new_v4().to_string()),
            project_id: project.id,
            pulsation_id: None,
            job_id: None,
            agent_run_id: None,
            job_chain: "fallback-e2e".to_string(),
            target: crate::llm::AgentExecutionTarget::canonical(AgentRole::BackendAgent),
            stage: "Code".to_string(),
            system_prompt: "test".to_string(),
            user_prompt: "test".to_string(),
            response_format: ResponseFormat::Text,
            max_output_tokens: 8,
            temperature: 0.0,
            deadline: Instant::now() + Duration::from_secs(10),
            stream: false,
            legacy_provider: "openai".to_string(),
            legacy_model: "unused".to_string(),
            dispatch: crate::llm::DispatchPolicy::default(),
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let response = runtime
            .block_on(
                service
                    .llm_router
                    .execute(&request, &CancellationToken::new()),
            )
            .expect("second target should succeed");
        assert_eq!(response.connection_id.as_str(), second.id);
        assert_eq!(response.content, "OK");
        let attempts = service
            .llm_repository
            .execution_snapshot(request.execution_id.as_str())
            .unwrap()
            .attempts;
        assert_eq!(attempts.len(), 2);
        let first_after = service
            .llm_repository
            .find_resolved_connection(&first.id)
            .unwrap();
        assert!(first_after.cooldown_until.is_some());
        first_server.join().unwrap();
        second_server.join().unwrap();
    }

    #[test]
    fn router_withholds_incomplete_stream_and_can_try_the_next_target() {
        use crate::llm::{
            AgentRole, ExecutionId, GenerationId, LlmErrorKind, LlmGateway, LlmRequest,
            ResponseFormat,
        };
        use std::time::Instant;

        let (stream_endpoint, stream_server) = spawn_delayed_sse();
        let unused_listener =
            TcpListener::bind("127.0.0.1:0").expect("unused endpoint should bind");
        let unused_endpoint = format!(
            "http://{}",
            unused_listener
                .local_addr()
                .expect("unused endpoint should resolve")
        );
        drop(unused_listener);
        let (workspace, service) = prepare_service();
        let project = service
            .create_project(CreateProjectRequest {
                workspace_path: workspace
                    .path()
                    .join("stream-no-fallback")
                    .to_string_lossy()
                    .to_string(),
                name: Some("Stream no fallback".to_string()),
            })
            .expect("project should save")
            .project;
        let first = service
            .upsert_provider_connection(UpsertProviderConnectionRequest {
                id: Some("ollama-stream-partial".to_string()),
                provider_id: "ollama".to_string(),
                display_name: "Partial stream".to_string(),
                endpoint_url: Some(stream_endpoint),
                default_model: "fixture".to_string(),
                enabled: Some(true),
                max_concurrency: Some(1),
                api_key: None,
                version: None,
            })
            .unwrap()
            .connection;
        let second = service
            .upsert_provider_connection(UpsertProviderConnectionRequest {
                id: Some("ollama-must-not-run".to_string()),
                provider_id: "ollama".to_string(),
                display_name: "Must not run".to_string(),
                endpoint_url: Some(unused_endpoint),
                default_model: "fixture".to_string(),
                enabled: Some(true),
                max_concurrency: Some(1),
                api_key: None,
                version: None,
            })
            .unwrap()
            .connection;
        service
            .upsert_provider_route(UpsertProviderRouteRequest {
                id: Some("stream-no-fallback".to_string()),
                name: "Stream no fallback".to_string(),
                strategy: "priority".to_string(),
                max_attempts: Some(3),
                attempt_timeout_ms: Some(100),
                route_timeout_ms: Some(2_000),
                enabled: Some(true),
                targets: vec![
                    crate::llm::RouteTarget {
                        position: 0,
                        connection_id: first.id,
                        model_id: "fixture".to_string(),
                        enabled: true,
                    },
                    crate::llm::RouteTarget {
                        position: 1,
                        connection_id: second.id,
                        model_id: "fixture".to_string(),
                        enabled: true,
                    },
                ],
                version: None,
            })
            .unwrap();
        service
            .upsert_provider_route_binding(UpsertProviderRouteBindingRequest {
                project_id: project.id.clone(),
                agent_role: "backend_agent".to_string(),
                stage: "Code".to_string(),
                route_id: "stream-no-fallback".to_string(),
                enabled: Some(true),
                version: None,
            })
            .unwrap();
        service.llm_repository.set_routing_mode("on").unwrap();
        let request = LlmRequest {
            execution_id: ExecutionId::new(Uuid::new_v4().to_string()),
            generation_id: GenerationId::new(Uuid::new_v4().to_string()),
            project_id: project.id,
            pulsation_id: None,
            job_id: None,
            agent_run_id: None,
            job_chain: "stream-no-fallback".to_string(),
            target: crate::llm::AgentExecutionTarget::canonical(AgentRole::BackendAgent),
            stage: "Code".to_string(),
            system_prompt: "test".to_string(),
            user_prompt: "test".to_string(),
            response_format: ResponseFormat::Text,
            max_output_tokens: 8,
            temperature: 0.0,
            deadline: Instant::now() + Duration::from_secs(2),
            stream: true,
            legacy_provider: "openai".to_string(),
            legacy_model: "unused".to_string(),
            dispatch: crate::llm::DispatchPolicy::default(),
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let error = runtime
            .block_on(
                service
                    .llm_router
                    .execute(&request, &CancellationToken::new()),
            )
            .expect_err("both targets are unavailable");
        assert_eq!(error.kind, LlmErrorKind::AllTargetsExhausted);
        assert!(error.visible_partial.is_none());
        let snapshot = service
            .llm_repository
            .execution_snapshot(request.execution_id.as_str())
            .unwrap();
        assert_eq!(snapshot.status, "failed");
        assert!(snapshot.visible_output.is_none());
        assert!(!snapshot.visible_output_complete);
        assert_eq!(snapshot.attempts.len(), 2);
        stream_server.join().unwrap();
    }

    #[test]
    fn router_blocks_connection_on_401_and_falls_back_to_second_target() {
        use crate::llm::{
            AgentRole, ExecutionId, GenerationId, LlmGateway, LlmRequest, ResponseFormat,
        };
        use std::time::Instant;

        let (first_endpoint, first_server) = spawn_http_response(
            "401 Unauthorized",
            "",
            r#"{"error":{"message":"Invalid API key","code":"invalid_api_key"}}"#,
        );
        let (second_endpoint, second_server) = spawn_http_response(
            "200 OK",
            "",
            r#"{"model":"fixture","choices":[{"message":{"content":"Auth fallback OK"},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":3}}"#,
        );
        let (workspace, service) = prepare_service();
        let project = service
            .create_project(CreateProjectRequest {
                workspace_path: workspace
                    .path()
                    .join("routing-401")
                    .to_string_lossy()
                    .to_string(),
                name: Some("Routing 401".to_string()),
            })
            .expect("project should save")
            .project;
        let first = service
            .upsert_provider_connection(UpsertProviderConnectionRequest {
                id: Some("ollama-unauthorized".to_string()),
                provider_id: "ollama".to_string(),
                display_name: "Unauthorized".to_string(),
                endpoint_url: Some(first_endpoint),
                default_model: "fixture".to_string(),
                enabled: Some(true),
                max_concurrency: Some(1),
                api_key: None,
                version: None,
            })
            .unwrap()
            .connection;
        let second = service
            .upsert_provider_connection(UpsertProviderConnectionRequest {
                id: Some("ollama-auth-fallback".to_string()),
                provider_id: "ollama".to_string(),
                display_name: "Fallback Success".to_string(),
                endpoint_url: Some(second_endpoint),
                default_model: "fixture".to_string(),
                enabled: Some(true),
                max_concurrency: Some(1),
                api_key: None,
                version: None,
            })
            .unwrap()
            .connection;
        service
            .upsert_provider_route(UpsertProviderRouteRequest {
                id: Some("fallback-401".to_string()),
                name: "Fallback 401".to_string(),
                strategy: "priority".to_string(),
                max_attempts: Some(3),
                attempt_timeout_ms: Some(5_000),
                route_timeout_ms: Some(10_000),
                enabled: Some(true),
                targets: vec![
                    crate::llm::RouteTarget {
                        position: 0,
                        connection_id: first.id.clone(),
                        model_id: "fixture".to_string(),
                        enabled: true,
                    },
                    crate::llm::RouteTarget {
                        position: 1,
                        connection_id: second.id.clone(),
                        model_id: "fixture".to_string(),
                        enabled: true,
                    },
                ],
                version: None,
            })
            .unwrap();
        service
            .upsert_provider_route_binding(UpsertProviderRouteBindingRequest {
                project_id: project.id.clone(),
                agent_role: "backend_agent".to_string(),
                stage: "Code".to_string(),
                route_id: "fallback-401".to_string(),
                enabled: Some(true),
                version: None,
            })
            .unwrap();
        service.llm_repository.set_routing_mode("on").unwrap();

        let request = LlmRequest {
            execution_id: ExecutionId::new(Uuid::new_v4().to_string()),
            generation_id: GenerationId::new(Uuid::new_v4().to_string()),
            project_id: project.id.clone(),
            pulsation_id: None,
            job_id: None,
            agent_run_id: None,
            job_chain: "fallback-401".to_string(),
            target: crate::llm::AgentExecutionTarget::canonical(AgentRole::BackendAgent),
            stage: "Code".to_string(),
            system_prompt: "test".to_string(),
            user_prompt: "test".to_string(),
            response_format: ResponseFormat::Text,
            max_output_tokens: 8,
            temperature: 0.0,
            deadline: Instant::now() + Duration::from_secs(10),
            stream: false,
            legacy_provider: "openai".to_string(),
            legacy_model: "unused".to_string(),
            dispatch: crate::llm::DispatchPolicy::default(),
        };

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let response = runtime
            .block_on(
                service
                    .llm_router
                    .execute(&request, &CancellationToken::new()),
            )
            .expect("second target should succeed after 401");
        assert_eq!(response.connection_id.as_str(), second.id);
        assert_eq!(response.content, "Auth fallback OK");

        let first_after = service
            .llm_repository
            .find_resolved_connection(&first.id)
            .unwrap();
        assert_eq!(first_after.state, "blocked");

        let attempts = service
            .llm_repository
            .execution_snapshot(request.execution_id.as_str())
            .unwrap()
            .attempts;
        assert_eq!(attempts.len(), 2);
        assert_eq!(
            attempts[0].get("outcome").and_then(Value::as_str),
            Some("failed")
        );
        assert_eq!(
            attempts[0].get("errorKind").and_then(Value::as_str),
            Some("provider_blocked")
        );
        assert_eq!(
            attempts[1].get("outcome").and_then(Value::as_str),
            Some("success")
        );

        first_server.join().unwrap();
        second_server.join().unwrap();
    }

    #[test]
    fn router_locks_out_model_on_model_not_found_and_falls_back() {
        use crate::llm::{
            AgentRole, ExecutionId, GenerationId, LlmGateway, LlmRequest, ResponseFormat,
        };
        use std::time::Instant;

        let (first_endpoint, first_server) = spawn_http_response(
            "404 Not Found",
            "",
            r#"{"error":{"message":"The model bad-model does not exist","code":"model_not_found"}}"#,
        );
        let (second_endpoint, second_server) = spawn_http_response(
            "200 OK",
            "",
            r#"{"model":"bad-model","choices":[{"message":{"content":"Model fallback OK"},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":3}}"#,
        );
        let (workspace, service) = prepare_service();
        let project = service
            .create_project(CreateProjectRequest {
                workspace_path: workspace
                    .path()
                    .join("routing-lockout")
                    .to_string_lossy()
                    .to_string(),
                name: Some("Routing Lockout".to_string()),
            })
            .expect("project should save")
            .project;
        let first = service
            .upsert_provider_connection(UpsertProviderConnectionRequest {
                id: Some("ollama-bad-model".to_string()),
                provider_id: "ollama".to_string(),
                display_name: "Bad Model Conn".to_string(),
                endpoint_url: Some(first_endpoint),
                default_model: "bad-model".to_string(),
                enabled: Some(true),
                max_concurrency: Some(1),
                api_key: None,
                version: None,
            })
            .unwrap()
            .connection;
        let second = service
            .upsert_provider_connection(UpsertProviderConnectionRequest {
                id: Some("ollama-good-model".to_string()),
                provider_id: "ollama".to_string(),
                display_name: "Good Model Conn".to_string(),
                endpoint_url: Some(second_endpoint),
                default_model: "bad-model".to_string(),
                enabled: Some(true),
                max_concurrency: Some(1),
                api_key: None,
                version: None,
            })
            .unwrap()
            .connection;
        service
            .upsert_provider_route(UpsertProviderRouteRequest {
                id: Some("fallback-lockout".to_string()),
                name: "Fallback Lockout".to_string(),
                strategy: "priority".to_string(),
                max_attempts: Some(3),
                attempt_timeout_ms: Some(5_000),
                route_timeout_ms: Some(10_000),
                enabled: Some(true),
                targets: vec![
                    crate::llm::RouteTarget {
                        position: 0,
                        connection_id: first.id.clone(),
                        model_id: "bad-model".to_string(),
                        enabled: true,
                    },
                    crate::llm::RouteTarget {
                        position: 1,
                        connection_id: second.id.clone(),
                        model_id: "bad-model".to_string(),
                        enabled: true,
                    },
                ],
                version: None,
            })
            .unwrap();
        service
            .upsert_provider_route_binding(UpsertProviderRouteBindingRequest {
                project_id: project.id.clone(),
                agent_role: "backend_agent".to_string(),
                stage: "Code".to_string(),
                route_id: "fallback-lockout".to_string(),
                enabled: Some(true),
                version: None,
            })
            .unwrap();
        service.llm_repository.set_routing_mode("on").unwrap();

        let request = LlmRequest {
            execution_id: ExecutionId::new(Uuid::new_v4().to_string()),
            generation_id: GenerationId::new(Uuid::new_v4().to_string()),
            project_id: project.id.clone(),
            pulsation_id: None,
            job_id: None,
            agent_run_id: None,
            job_chain: "fallback-lockout".to_string(),
            target: crate::llm::AgentExecutionTarget::canonical(AgentRole::BackendAgent),
            stage: "Code".to_string(),
            system_prompt: "test".to_string(),
            user_prompt: "test".to_string(),
            response_format: ResponseFormat::Text,
            max_output_tokens: 8,
            temperature: 0.0,
            deadline: Instant::now() + Duration::from_secs(10),
            stream: false,
            legacy_provider: "openai".to_string(),
            legacy_model: "unused".to_string(),
            dispatch: crate::llm::DispatchPolicy::default(),
        };

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let response = runtime
            .block_on(
                service
                    .llm_router
                    .execute(&request, &CancellationToken::new()),
            )
            .expect("second target should succeed after model lockout");
        assert_eq!(response.connection_id.as_str(), second.id);
        assert_eq!(response.content, "Model fallback OK");

        let states = service.llm_repository.resilience_snapshot().unwrap().states;
        assert!(states.iter().any(
            |item| item.scope == "model" && item.scope_id == format!("{}@bad-model", first.id)
        ));

        first_server.join().unwrap();
        second_server.join().unwrap();
    }

    #[test]
    fn router_circuit_breaker_full_cycle_opens_and_lazy_recovers() {
        let (_workspace, service) = prepare_service();
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-18T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        service
            .llm_repository
            .record_health_failure("ollama", "http://127.0.0.1:11434", true, now)
            .unwrap();
        assert_eq!(
            service
                .llm_repository
                .breaker_state("ollama", "http://127.0.0.1:11434")
                .unwrap()
                .as_deref(),
            Some("degraded")
        );

        service
            .llm_repository
            .record_health_failure("ollama", "http://127.0.0.1:11434", true, now)
            .unwrap();
        assert_eq!(
            service
                .llm_repository
                .breaker_state("ollama", "http://127.0.0.1:11434")
                .unwrap()
                .as_deref(),
            Some("open")
        );

        assert!(
            !service
                .llm_repository
                .claim_breaker_probe(
                    "ollama",
                    "http://127.0.0.1:11434",
                    now + chrono::Duration::seconds(5)
                )
                .unwrap()
        );

        let probe_time = now + chrono::Duration::seconds(20);
        assert!(
            service
                .llm_repository
                .claim_breaker_probe("ollama", "http://127.0.0.1:11434", probe_time)
                .unwrap()
        );

        assert!(
            !service
                .llm_repository
                .claim_breaker_probe("ollama", "http://127.0.0.1:11434", probe_time)
                .unwrap()
        );

        service
            .llm_repository
            .record_health_success("ollama", "http://127.0.0.1:11434", probe_time)
            .unwrap();
        assert_eq!(
            service
                .llm_repository
                .breaker_state("ollama", "http://127.0.0.1:11434")
                .unwrap()
                .as_deref(),
            Some("closed")
        );
    }

    #[test]
    fn router_respects_max_attempts_and_stops_without_calling_extra_targets() {
        use crate::llm::{
            AgentRole, ExecutionId, GenerationId, LlmErrorKind, LlmGateway, LlmRequest,
            ResponseFormat,
        };
        use std::time::Instant;

        let (first_endpoint, first_server) = spawn_http_response(
            "500 Internal Server Error",
            "",
            r#"{"error":{"message":"Internal error 1","code":"internal_error"}}"#,
        );
        let (second_endpoint, second_server) = spawn_http_response(
            "500 Internal Server Error",
            "",
            r#"{"error":{"message":"Internal error 2","code":"internal_error"}}"#,
        );
        let unused_listener =
            TcpListener::bind("127.0.0.1:0").expect("unused endpoint should bind");
        let unused_endpoint = format!(
            "http://{}",
            unused_listener
                .local_addr()
                .expect("unused endpoint should resolve")
        );
        drop(unused_listener);

        let (workspace, service) = prepare_service();
        let project = service
            .create_project(CreateProjectRequest {
                workspace_path: workspace
                    .path()
                    .join("routing-max-attempts")
                    .to_string_lossy()
                    .to_string(),
                name: Some("Routing Max Attempts".to_string()),
            })
            .expect("project should save")
            .project;

        let first = service
            .upsert_provider_connection(UpsertProviderConnectionRequest {
                id: Some("ollama-attempt-1".to_string()),
                provider_id: "ollama".to_string(),
                display_name: "Attempt 1".to_string(),
                endpoint_url: Some(first_endpoint),
                default_model: "fixture".to_string(),
                enabled: Some(true),
                max_concurrency: Some(1),
                api_key: None,
                version: None,
            })
            .unwrap()
            .connection;
        let second = service
            .upsert_provider_connection(UpsertProviderConnectionRequest {
                id: Some("ollama-attempt-2".to_string()),
                provider_id: "ollama".to_string(),
                display_name: "Attempt 2".to_string(),
                endpoint_url: Some(second_endpoint),
                default_model: "fixture".to_string(),
                enabled: Some(true),
                max_concurrency: Some(1),
                api_key: None,
                version: None,
            })
            .unwrap()
            .connection;
        let third = service
            .upsert_provider_connection(UpsertProviderConnectionRequest {
                id: Some("ollama-attempt-3-unreached".to_string()),
                provider_id: "ollama".to_string(),
                display_name: "Attempt 3 Unreached".to_string(),
                endpoint_url: Some(unused_endpoint),
                default_model: "fixture".to_string(),
                enabled: Some(true),
                max_concurrency: Some(1),
                api_key: None,
                version: None,
            })
            .unwrap()
            .connection;

        service
            .upsert_provider_route(UpsertProviderRouteRequest {
                id: Some("route-max-2".to_string()),
                name: "Route Max 2".to_string(),
                strategy: "priority".to_string(),
                max_attempts: Some(2),
                attempt_timeout_ms: Some(5_000),
                route_timeout_ms: Some(10_000),
                enabled: Some(true),
                targets: vec![
                    crate::llm::RouteTarget {
                        position: 0,
                        connection_id: first.id,
                        model_id: "fixture".to_string(),
                        enabled: true,
                    },
                    crate::llm::RouteTarget {
                        position: 1,
                        connection_id: second.id,
                        model_id: "fixture".to_string(),
                        enabled: true,
                    },
                    crate::llm::RouteTarget {
                        position: 2,
                        connection_id: third.id,
                        model_id: "fixture".to_string(),
                        enabled: true,
                    },
                ],
                version: None,
            })
            .unwrap();
        service
            .upsert_provider_route_binding(UpsertProviderRouteBindingRequest {
                project_id: project.id.clone(),
                agent_role: "backend_agent".to_string(),
                stage: "Code".to_string(),
                route_id: "route-max-2".to_string(),
                enabled: Some(true),
                version: None,
            })
            .unwrap();
        service.llm_repository.set_routing_mode("on").unwrap();

        let request = LlmRequest {
            execution_id: ExecutionId::new(Uuid::new_v4().to_string()),
            generation_id: GenerationId::new(Uuid::new_v4().to_string()),
            project_id: project.id,
            pulsation_id: None,
            job_id: None,
            agent_run_id: None,
            job_chain: "route-max-2".to_string(),
            target: crate::llm::AgentExecutionTarget::canonical(AgentRole::BackendAgent),
            stage: "Code".to_string(),
            system_prompt: "test".to_string(),
            user_prompt: "test".to_string(),
            response_format: ResponseFormat::Text,
            max_output_tokens: 8,
            temperature: 0.0,
            deadline: Instant::now() + Duration::from_secs(10),
            stream: false,
            legacy_provider: "openai".to_string(),
            legacy_model: "unused".to_string(),
            dispatch: crate::llm::DispatchPolicy::default(),
        };

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let error = runtime
            .block_on(
                service
                    .llm_router
                    .execute(&request, &CancellationToken::new()),
            )
            .expect_err("should exhaust targets after 2 attempts");
        assert_eq!(error.kind, LlmErrorKind::AllTargetsExhausted);

        let snapshot = service
            .llm_repository
            .execution_snapshot(request.execution_id.as_str())
            .unwrap();
        assert_eq!(snapshot.attempts.len(), 2);

        first_server.join().unwrap();
        second_server.join().unwrap();
    }

    #[test]
    fn router_lkgp_promotes_affinity_target_and_respects_cooldown_gates() {
        use crate::llm::{
            AgentRole, ExecutionId, GenerationId, LlmGateway, LlmRequest, ResponseFormat,
        };
        use std::time::Instant;

        let (first_endpoint, first_server) = spawn_http_response(
            "429 Too Many Requests",
            "Retry-After: 1\r\n",
            r#"{"error":{"message":"Rate limit 1","code":"rate_limit"}}"#,
        );
        let (second_endpoint, second_server) = spawn_http_response(
            "200 OK",
            "",
            r#"{"model":"fixture","choices":[{"message":{"content":"LKGP Target 2 OK"},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":4}}"#,
        );

        let (workspace, service) = prepare_service();
        let project = service
            .create_project(CreateProjectRequest {
                workspace_path: workspace
                    .path()
                    .join("routing-lkgp")
                    .to_string_lossy()
                    .to_string(),
                name: Some("Routing LKGP".to_string()),
            })
            .expect("project should save")
            .project;

        let first = service
            .upsert_provider_connection(UpsertProviderConnectionRequest {
                id: Some("ollama-lkgp-1".to_string()),
                provider_id: "ollama".to_string(),
                display_name: "LKGP 1".to_string(),
                endpoint_url: Some(first_endpoint),
                default_model: "fixture".to_string(),
                enabled: Some(true),
                max_concurrency: Some(1),
                api_key: None,
                version: None,
            })
            .unwrap()
            .connection;
        let second = service
            .upsert_provider_connection(UpsertProviderConnectionRequest {
                id: Some("ollama-lkgp-2".to_string()),
                provider_id: "ollama".to_string(),
                display_name: "LKGP 2".to_string(),
                endpoint_url: Some(second_endpoint),
                default_model: "fixture".to_string(),
                enabled: Some(true),
                max_concurrency: Some(1),
                api_key: None,
                version: None,
            })
            .unwrap()
            .connection;

        service
            .upsert_provider_route(UpsertProviderRouteRequest {
                id: Some("route-lkgp".to_string()),
                name: "Route LKGP".to_string(),
                strategy: "lkgp".to_string(),
                max_attempts: Some(3),
                attempt_timeout_ms: Some(5_000),
                route_timeout_ms: Some(10_000),
                enabled: Some(true),
                targets: vec![
                    crate::llm::RouteTarget {
                        position: 0,
                        connection_id: first.id.clone(),
                        model_id: "fixture".to_string(),
                        enabled: true,
                    },
                    crate::llm::RouteTarget {
                        position: 1,
                        connection_id: second.id.clone(),
                        model_id: "fixture".to_string(),
                        enabled: true,
                    },
                ],
                version: None,
            })
            .unwrap();
        service
            .upsert_provider_route_binding(UpsertProviderRouteBindingRequest {
                project_id: project.id.clone(),
                agent_role: "backend_agent".to_string(),
                stage: "Code".to_string(),
                route_id: "route-lkgp".to_string(),
                enabled: Some(true),
                version: None,
            })
            .unwrap();
        service.llm_repository.set_routing_mode("on").unwrap();

        let request = LlmRequest {
            execution_id: ExecutionId::new(Uuid::new_v4().to_string()),
            generation_id: GenerationId::new(Uuid::new_v4().to_string()),
            project_id: project.id.clone(),
            pulsation_id: None,
            job_id: None,
            agent_run_id: None,
            job_chain: "route-lkgp".to_string(),
            target: crate::llm::AgentExecutionTarget::canonical(AgentRole::BackendAgent),
            stage: "Code".to_string(),
            system_prompt: "test".to_string(),
            user_prompt: "test".to_string(),
            response_format: ResponseFormat::Text,
            max_output_tokens: 8,
            temperature: 0.0,
            deadline: Instant::now() + Duration::from_secs(10),
            stream: false,
            legacy_provider: "openai".to_string(),
            legacy_model: "unused".to_string(),
            dispatch: crate::llm::DispatchPolicy::default(),
        };

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let response = runtime
            .block_on(
                service
                    .llm_router
                    .execute(&request, &CancellationToken::new()),
            )
            .expect("fallback to second target should succeed");
        assert_eq!(response.connection_id.as_str(), second.id);

        first_server.join().unwrap();
        second_server.join().unwrap();

        let route = service
            .llm_repository
            .resolve_route(&project.id, AgentRole::BackendAgent, "Code")
            .unwrap();
        let targets = service.llm_repository.resolved_targets(&route.id).unwrap();
        let decision = service
            .llm_repository
            .decide(&route, &targets, "some-affinity-hash", Utc::now())
            .unwrap();
        assert!(
            decision
                .candidates
                .iter()
                .any(|c| c.connection_id == second.id)
        );
    }

    #[test]
    fn user_cancellation_does_not_affect_health_or_cooldown() {
        use crate::llm::{
            AgentRole, ExecutionId, GenerationId, LlmErrorKind, LlmGateway, LlmRequest,
            ResponseFormat,
        };
        use std::time::Instant;

        let (_workspace, service) = prepare_service();
        let token = CancellationToken::new();
        token.cancel();

        let request = LlmRequest {
            execution_id: ExecutionId::new(Uuid::new_v4().to_string()),
            generation_id: GenerationId::new(Uuid::new_v4().to_string()),
            project_id: "cancel-test".to_string(),
            pulsation_id: None,
            job_id: None,
            agent_run_id: None,
            job_chain: "cancel-test".to_string(),
            target: crate::llm::AgentExecutionTarget::canonical(AgentRole::BackendAgent),
            stage: "Code".to_string(),
            system_prompt: "test".to_string(),
            user_prompt: "test".to_string(),
            response_format: ResponseFormat::Text,
            max_output_tokens: 8,
            temperature: 0.0,
            deadline: Instant::now() + Duration::from_secs(10),
            stream: false,
            legacy_provider: "openai".to_string(),
            legacy_model: "unused".to_string(),
            dispatch: crate::llm::DispatchPolicy::default(),
        };

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let error = runtime
            .block_on(service.llm_router.execute(&request, &token))
            .expect_err("cancelled token should return error");
        assert_eq!(error.kind, LlmErrorKind::Cancelled);

        let snapshot = service.llm_repository.resilience_snapshot().unwrap();
        assert!(snapshot.states.is_empty());
    }
}
