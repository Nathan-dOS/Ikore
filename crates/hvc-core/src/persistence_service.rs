use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::agent_runtime::{
    AgentRuntimeConfig, AgentRuntimeError, ContextStrategy, EmbeddingProvider, LlmProvider,
    ProviderKeyStatus, StageExecutionInput, StageExecutionResult, execute_stage,
};
use crate::embedding_service::{EmbeddingGateway, OllamaEmbeddingGateway};
use crate::llm::{
    Clock, CompressionPreview, CompressionPreviewRequest, CredentialStore,
    DeleteProviderConnectionRequest, DeleteProviderRouteRequest, JitterSource,
    KeyringCredentialStore, LlmError, LlmErrorKind, LlmEventSink, LlmExecutionSnapshot,
    LlmResilienceSnapshot, LlmRoutingMetric, NoopEventSink, ProviderConnection, ProviderProfile,
    ProviderRoute, ProviderRouteBinding, ProviderRouter, ResetLlmResilienceStateRequest,
    RoutingMode, SqliteLlmRepository, StableJitter, SystemClock, TestProviderConnectionRequest,
    TestProviderConnectionResponse, UpsertProviderConnectionRequest,
    UpsertProviderRouteBindingRequest, UpsertProviderRouteRequest, preview_compression,
    provider_profiles,
};
use chrono::Utc;
use reqwest::Client;
use rusqlite::{Connection, Error as SqlError, ErrorCode, OptionalExtension, params, types::Type};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[path = "orchestration/coordination.rs"]
mod coordination;
#[path = "orchestration/integration.rs"]
mod integration;
#[path = "orchestration/reconciliation.rs"]
mod reconciliation;
#[path = "orchestration/serial.rs"]
mod serial;
#[path = "orchestration/supervisor.rs"]
mod supervisor;

mod embedded {
    use refinery::embed_migrations;
    embed_migrations!("migrations");
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
            | Self::Cancelled(message) => write!(f, "{message}"),
            Self::Llm(error) => write!(f, "{error}"),
            Self::Db(error) => write!(f, "Falha de banco local: {error}"),
            Self::Migration(error) => write!(f, "Falha de migração: {error}"),
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
                message: format!("Falha de banco local: {error}"),
                details: None,
                remediation: None,
            },
            PersistenceServiceError::Migration(error) => Self {
                code: PersistenceErrorCode::MigrationError,
                message: format!("Falha ao aplicar migrações SQLite: {error}"),
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
                    LlmErrorKind::MissingApiKey => Some("Cadastre uma API key na conexão e teste-a novamente.".to_string()),
                    LlmErrorKind::Authentication | LlmErrorKind::Billing => Some("Atualize a credencial ou regularize a conta do provider.".to_string()),
                    LlmErrorKind::RouteConflict => Some("Recarregue a configuração e reaplique a alteração sobre a versão atual.".to_string()),
                    LlmErrorKind::StreamFailed => Some("Revise o texto parcial e use retry explícito para iniciar uma nova geração.".to_string()),
                    _ => None,
                };
                Self {
                    code,
                    message: error.message,
                    details: Some(serde_json::json!({
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
                Self::Runtime(format!("Falha ao processar JSON do agente: {error}"))
            }
            AgentRuntimeError::Provider(message) => Self::Runtime(message),
            AgentRuntimeError::Schema(message) => Self::Runtime(message),
            AgentRuntimeError::Cancelled => {
                Self::Cancelled("Execução cancelada durante chamada do agente.".to_string())
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
    pub sidebar_collapsed: bool,
    pub context_drawer_open: bool,
    pub active_pulsation_id: Option<String>,
    pub selected_vault_path: Option<String>,
    pub active_chat_conversation_id: Option<String>,
    pub scroll_top: Option<f64>,
    pub tabs: Vec<UiTabSnapshot>,
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
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveUiSessionResponse {
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PulsationStage {
    #[serde(rename = "Intake")]
    Intake,
    #[serde(rename = "SPECIFICATION")]
    Specification,
    #[serde(rename = "Contract")]
    Contract,
    #[serde(rename = "Code")]
    Code,
    #[serde(rename = "Test")]
    Test,
    #[serde(rename = "Integrator")]
    Integrator,
}

impl PulsationStage {
    pub(crate) fn as_db_value(self) -> &'static str {
        crate::orchestration::catalog::STAGES
            .iter()
            .find(|(stage, _, _)| *stage == self)
            .expect("six stages")
            .1
    }

    fn from_db_value(raw: &str) -> Option<Self> {
        crate::orchestration::catalog::parse_stage(raw)
    }

    pub(crate) fn next(self) -> Option<Self> {
        match self {
            Self::Intake => Some(Self::Specification),
            Self::Specification => Some(Self::Contract),
            Self::Contract => Some(Self::Code),
            Self::Code => Some(Self::Test),
            Self::Test => Some(Self::Integrator),
            Self::Integrator => None,
        }
    }

    pub(crate) fn requires_checkpoint(self) -> bool {
        matches!(
            self,
            Self::Specification | Self::Contract | Self::Test | Self::Integrator
        )
    }
}

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
    pub coordination: Option<crate::orchestration::ProjectCoordinationSnapshot>,
    pub main_agents_count: usize,
    pub hitl_checkpoints: Vec<HitlCheckpoint>,
    pub pulsations: Vec<Pulsation>,
    pub hitl_waiting_count: usize,
    pub background_agents_count: usize,
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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HitlCheckpoint {
    pub pulsation_id: String,
    pub checkpoint_id: String,
    pub state_version: u64,
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
    pub workspace_path: String,
    pub model: String,
    pub indexed_count: usize,
    pub skipped_count: usize,
    pub deleted_count: usize,
    pub failed_count: usize,
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListAgentRunsRequest {
    pub pulsation_id: String,
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
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListAgentRunsResponse {
    pub runs: Vec<AgentRunListItem>,
}

#[derive(Debug, Default)]
struct RuntimeControl {
    cancellations: Mutex<HashMap<String, CancellationToken>>,
    wakeup: tokio::sync::Notify,
    pending_user_cancels: Mutex<HashMap<String, usize>>,
    #[cfg(test)]
    publication_gate: Mutex<Option<(Arc<std::sync::Barrier>, Arc<std::sync::Barrier>)>>,
}

#[derive(Clone)]
pub struct PersistenceService {
    db_path: PathBuf,
    runtime_control: Arc<RuntimeControl>,
    embedding_gateway: Arc<dyn EmbeddingGateway>,
    llm_repository: SqliteLlmRepository,
    credentials: Arc<dyn CredentialStore>,
    llm_router: Arc<ProviderRouter>,
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
        embedded::migrations::runner()
            .run(&mut connection)
            .map_err(PersistenceServiceError::Migration)?;
        crate::orchestration::upgrade::adapt_legacy(&mut connection)?;

        let llm_repository = SqliteLlmRepository::new(&db_path);
        llm_repository.ensure_legacy_configuration()?;
        let llm_router = Arc::new(ProviderRouter::new(
            llm_repository.clone(),
            llm_client.clone(),
            credentials.clone(),
            event_sink,
            clock,
            jitter,
        ));
        Ok(Self {
            db_path,
            runtime_control: Arc::new(RuntimeControl::default()),
            embedding_gateway,
            llm_repository,
            credentials,
            llm_router,
        })
    }

    fn connection(&self) -> Result<Connection, PersistenceServiceError> {
        let connection = Connection::open(&self.db_path)?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.busy_timeout(Duration::from_secs(5))?;
        Ok(connection)
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
        let mut config = AgentRuntimeConfig::default();
        let connection = self.connection()?;

        if let Some(raw_provider) =
            Self::read_runtime_setting(&connection, SETTING_RUNTIME_PROVIDER)?
            && let Some(provider) = LlmProvider::parse(raw_provider.trim())
        {
            config.provider = provider;
        }

        if let Some(raw_model) = Self::read_runtime_setting(&connection, SETTING_RUNTIME_MODEL)? {
            config.model = raw_model;
        }

        if let Some(raw_temperature) =
            Self::read_runtime_setting(&connection, SETTING_RUNTIME_TEMPERATURE)?
            && let Ok(parsed) = raw_temperature.parse::<f32>()
        {
            config.temperature = parsed;
        }

        if let Some(raw_max_output_tokens) =
            Self::read_runtime_setting(&connection, SETTING_RUNTIME_MAX_OUTPUT_TOKENS)?
            && let Ok(parsed) = raw_max_output_tokens.parse::<u32>()
        {
            config.max_output_tokens = parsed;
        }

        if let Some(raw_timeout_ms) =
            Self::read_runtime_setting(&connection, SETTING_RUNTIME_TIMEOUT_MS)?
            && let Ok(parsed) = raw_timeout_ms.parse::<u64>()
        {
            config.timeout_ms = parsed;
        }

        if let Some(raw_max_retries) =
            Self::read_runtime_setting(&connection, SETTING_RUNTIME_MAX_RETRIES)?
            && let Ok(parsed) = raw_max_retries.parse::<u8>()
        {
            config.max_retries = parsed;
        }

        if let Some(raw_context_limit) =
            Self::read_runtime_setting(&connection, SETTING_RUNTIME_CONTEXT_LIMIT)?
            && let Ok(parsed) = raw_context_limit.parse::<usize>()
        {
            config.context_chars_limit = parsed;
        }

        if let Some(raw_max_context_files) =
            Self::read_runtime_setting(&connection, SETTING_RUNTIME_MAX_CONTEXT_FILES)?
            && let Ok(parsed) = raw_max_context_files.parse::<usize>()
        {
            config.max_context_files = parsed;
        }

        if let Some(raw_strategy) =
            Self::read_runtime_setting(&connection, SETTING_RUNTIME_CONTEXT_STRATEGY)?
            && let Some(parsed) = ContextStrategy::parse(raw_strategy.trim())
        {
            config.context_strategy = parsed;
        }

        if let Some(raw_embedding_enabled) =
            Self::read_runtime_setting(&connection, SETTING_RUNTIME_EMBEDDING_ENABLED)?
        {
            config.embedding_enabled = matches!(
                raw_embedding_enabled.trim(),
                "1" | "true" | "TRUE" | "yes" | "YES"
            );
        }

        if let Some(raw_embedding_provider) =
            Self::read_runtime_setting(&connection, SETTING_RUNTIME_EMBEDDING_PROVIDER)?
            && let Some(parsed) = EmbeddingProvider::parse(raw_embedding_provider.trim())
        {
            config.embedding_provider = parsed;
        }

        if let Some(raw_embedding_model) =
            Self::read_runtime_setting(&connection, SETTING_RUNTIME_EMBEDDING_MODEL)?
        {
            config.embedding_model = raw_embedding_model;
        }

        if let Some(raw_embedding_top_k) =
            Self::read_runtime_setting(&connection, SETTING_RUNTIME_EMBEDDING_TOP_K)?
            && let Ok(parsed) = raw_embedding_top_k.parse::<usize>()
        {
            config.embedding_top_k = parsed;
        }

        if let Some(raw_mode) =
            Self::read_runtime_setting(&connection, SETTING_RUNTIME_ROUTING_V2_MODE)?
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
        cancellations.len()
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
                "workspacePath não pode ser vazio.".to_string(),
            ));
        }
        Ok(normalized.to_string())
    }

    fn normalize_project_id(raw: &str) -> Result<String, PersistenceServiceError> {
        let normalized = raw.trim();
        if normalized.is_empty() {
            return Err(PersistenceServiceError::Validation(
                "projectId não pode ser vazio.".to_string(),
            ));
        }
        Ok(normalized.to_string())
    }

    fn normalize_workspace_view_mode(raw: &str) -> Result<String, PersistenceServiceError> {
        let normalized = raw.trim();
        if normalized != "editor" && normalized != "hitl" {
            return Err(PersistenceServiceError::Validation(
                "workspaceViewMode deve ser `editor` ou `hitl`.".to_string(),
            ));
        }

        Ok(normalized.to_string())
    }

    fn normalize_active_view(raw: &str) -> Result<String, PersistenceServiceError> {
        let normalized = raw.trim();
        if !matches!(
            normalized,
            "workspace" | "pulsations" | "vault" | "chat" | "settings" | "logs"
        ) {
            return Err(PersistenceServiceError::Validation(
                "activeView deve ser `workspace`, `pulsations`, `vault`, `chat`, `settings` ou `logs`."
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
                "pulsationId não pode ser vazio.".to_string(),
            ));
        }
        Ok(normalized.to_string())
    }

    fn normalize_feature_name(raw: &str) -> Result<String, PersistenceServiceError> {
        let normalized = raw.trim();
        if normalized.is_empty() {
            return Err(PersistenceServiceError::Validation(
                "featureName não pode ser vazio.".to_string(),
            ));
        }

        Ok(normalized.to_string())
    }

    fn invalid_enum_error(column: &str, value: &str) -> SqlError {
        SqlError::FromSqlConversionFailure(
            0,
            Type::Text,
            Box::new(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Valor inválido `{value}` na coluna `{column}`."),
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
            PulsationStatus::Planning => {
                matches!(to, PulsationStatus::Running | PulsationStatus::Failed)
            }
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
                "Transição inválida: {:?} -> {:?}.",
                current.status, next_status
            )));
        }

        let now = Self::now_iso();
        transaction.execute(
            "UPDATE pulsations
             SET status = ?1, stage = ?2, updated_at = ?3, state_version = state_version + 1
             WHERE id = ?4",
            params![
                next_status.as_db_value(),
                next_stage.as_db_value(),
                now,
                current.id.as_str()
            ],
        )?;

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
               updated_at, project_id, input_revision, state_version, idempotency_key
             ) SELECT ?1, ?2, ?3, ?4, 0, ?5, ?5, project_id, input_revision, state_version,
               id || ':' || input_revision || ':' || state_version || ':' || ?3 FROM pulsations WHERE id=?2",
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
                    "Já existe um projeto para este workspace.".to_string(),
                ));
            }
            return Err(PersistenceServiceError::Db(error));
        }

        let project =
            Self::find_project_by_id(&connection, &project_id, true)?.ok_or_else(|| {
                PersistenceServiceError::NotFound(
                    "Projeto recém-criado não foi encontrado.".to_string(),
                )
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
                "Projeto não encontrado ou arquivado.".to_string(),
            ));
        }

        let project =
            Self::find_project_by_id(&connection, &project_id, false)?.ok_or_else(|| {
                PersistenceServiceError::NotFound(
                    "Projeto não encontrado após atualização.".to_string(),
                )
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
                "Projeto não encontrado para arquivamento.".to_string(),
            ));
        }

        let project = Self::find_project_by_id(&connection, &request.project_id, true)?
            .ok_or_else(|| {
                PersistenceServiceError::NotFound("Projeto arquivado não encontrado.".to_string())
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
            connection.execute(
                "UPDATE projects
                 SET is_archived = 0, last_opened_at = ?1, updated_at = ?1
                 WHERE id = ?2",
                params![now, existing.id],
            )?;

            let project =
                Self::find_project_by_id(&connection, &existing.id, true)?.ok_or_else(|| {
                    PersistenceServiceError::NotFound(
                        "Projeto existente não foi encontrado.".to_string(),
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
                    scroll_top, updated_at
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
                "Projeto não encontrado para persistir sessão de UI.".to_string(),
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
               updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
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
                "Projeto não encontrado para criar pulsação.".to_string(),
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
               (SELECT COALESCE(MAX(queue_sequence),0)+1 FROM pulsations WHERE project_id=?2), 2)",
            params![
                pulsation_id.as_str(),
                project_id.as_str(),
                feature_name.as_str(),
                PulsationStage::Intake.as_db_value(),
                PulsationStatus::Idle.as_db_value(),
                now
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
                "Pulsação criada.",
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
            Some("Pulsação enviada para fila."),
        )?;

        Self::enqueue_stage_job(&transaction, &current.id, current.stage)?;
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

    pub fn get_pulsation_runtime_snapshot(
        &self,
        request: GetPulsationRuntimeSnapshotRequest,
    ) -> Result<GetPulsationRuntimeSnapshotResponse, PersistenceServiceError> {
        let project_id = Self::normalize_project_id(&request.project_id)?;
        let db = self.connection()?;
        let connection = db.unchecked_transaction()?;
        let pulsations = connection.prepare("SELECT id,project_id,feature_name,stage,status,created_at,updated_at,terminal_reason FROM pulsations WHERE project_id=?1 ORDER BY queue_sequence")?
            .query_map([&project_id],Self::map_pulsation)?.collect::<Result<Vec<_>,_>>()?;
        let hitl_waiting_count: i64 = connection.query_row(
            "SELECT COUNT(*)
             FROM pulsations
             WHERE project_id = ?1 AND status = 'checkpoint'",
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
                coordination: Some(Self::coordination_snapshot(&connection, &project_id)?),
                main_agents_count: connection.query_row(
                    "SELECT COUNT(*) FROM runtime_slots WHERE project_id=?1 AND job_class='main'",
                    [&project_id],
                    |r| r.get(0),
                )?,
                hitl_checkpoints: {
                    let mut stmt=connection.prepare("SELECT p.id,c.id,p.state_version FROM pulsations p JOIN checkpoints c ON c.pulsation_id=p.id WHERE p.project_id=?1 AND p.status='checkpoint' AND c.status='waiting'")?;
                    stmt.query_map([&project_id], |r| {
                        Ok(HitlCheckpoint {
                            pulsation_id: r.get(0)?,
                            checkpoint_id: r.get(1)?,
                            state_version: r.get(2)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?
                },
                pulsations,
                hitl_waiting_count: hitl_waiting_count.max(0) as usize,
                background_agents_count: background_agents_count.max(0) as usize,
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
        if let Some(routing_v2_mode) = request.routing_v2_mode {
            config.routing_v2_mode = routing_v2_mode;
        }

        let saved = self.persist_runtime_config(&config)?;
        Ok(UpdateAgentRuntimeConfigResponse { config: saved })
    }

    pub async fn reindex_workspace_embeddings_for_workspace(
        &self,
        workspace_path: &str,
        request: ReindexWorkspaceEmbeddingsRequest,
    ) -> Result<ReindexWorkspaceEmbeddingsResponse, PersistenceServiceError> {
        let normalized_workspace_path = Self::normalize_workspace_path(workspace_path)?;
        let runtime_config = self.load_runtime_config()?;
        let model = request
            .model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(runtime_config.embedding_model.as_str())
            .to_string();
        let workspace_root = PathBuf::from(&normalized_workspace_path);
        let notes_root = workspace_root.join("knowledge/obsidian");

        if !notes_root.exists() || !notes_root.is_dir() {
            return Ok(ReindexWorkspaceEmbeddingsResponse {
                workspace_path: normalized_workspace_path,
                model,
                indexed_count: 0,
                skipped_count: 0,
                deleted_count: 0,
                failed_count: 0,
            });
        }

        let connection = self.connection()?;
        let mut indexed_count = 0usize;
        let mut skipped_count = 0usize;
        let mut failed_count = 0usize;
        let mut deleted_count = 0usize;
        let mut seen_paths = HashSet::new();

        for entry in walkdir::WalkDir::new(&notes_root)
            .min_depth(1)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            if path
                .extension()
                .and_then(|value| value.to_str())
                .map(|value| value.to_ascii_lowercase())
                != Some("md".to_string())
            {
                continue;
            }

            let relative_path = match path.strip_prefix(&workspace_root) {
                Ok(value) => value.to_string_lossy().replace('\\', "/"),
                Err(_) => {
                    failed_count += 1;
                    continue;
                }
            };
            seen_paths.insert(relative_path.clone());

            let raw = match fs::read_to_string(path) {
                Ok(content) => content,
                Err(_) => {
                    failed_count += 1;
                    continue;
                }
            };
            let revision_hash = sha256_hex(raw.as_bytes());

            let stored_hash = connection
                .query_row(
                    "SELECT revision_hash
                     FROM workspace_note_embeddings
                     WHERE workspace_path = ?1 AND relative_path = ?2 AND embedding_model = ?3
                     LIMIT 1",
                    params![
                        normalized_workspace_path.as_str(),
                        relative_path.as_str(),
                        model.as_str()
                    ],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            if stored_hash.as_deref() == Some(revision_hash.as_str()) {
                skipped_count += 1;
                continue;
            }

            let embedding_source = strip_frontmatter_for_embedding(&raw);
            let embedding = match self
                .embedding_gateway
                .embed(model.as_str(), embedding_source.as_str())
                .await
            {
                Ok(value) => value,
                Err(_) => {
                    failed_count += 1;
                    continue;
                }
            };
            let embedding_json = serde_json::to_string(&embedding).map_err(|error| {
                PersistenceServiceError::Runtime(format!("Falha ao serializar embedding: {error}"))
            })?;

            connection.execute(
                "INSERT INTO workspace_note_embeddings (
                   id,
                   workspace_path,
                   relative_path,
                   revision_hash,
                   embedding_model,
                   embedding_vector_json,
                   updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(workspace_path, relative_path, embedding_model)
                 DO UPDATE SET
                   revision_hash = excluded.revision_hash,
                   embedding_vector_json = excluded.embedding_vector_json,
                   updated_at = excluded.updated_at",
                params![
                    Uuid::new_v4().to_string(),
                    normalized_workspace_path.as_str(),
                    relative_path.as_str(),
                    revision_hash.as_str(),
                    model.as_str(),
                    embedding_json.as_str(),
                    Self::now_iso()
                ],
            )?;
            indexed_count += 1;
        }

        let mut statement = connection.prepare(
            "SELECT relative_path
             FROM workspace_note_embeddings
             WHERE workspace_path = ?1 AND embedding_model = ?2",
        )?;
        let stale_rows = statement.query_map(
            params![normalized_workspace_path.as_str(), model.as_str()],
            |row| row.get::<_, String>(0),
        )?;
        let mut stale_paths = Vec::new();
        for row in stale_rows {
            let relative_path = row?;
            if !seen_paths.contains(&relative_path) {
                stale_paths.push(relative_path);
            }
        }
        for stale_relative_path in stale_paths {
            connection.execute(
                "DELETE FROM workspace_note_embeddings
                 WHERE workspace_path = ?1 AND relative_path = ?2 AND embedding_model = ?3",
                params![
                    normalized_workspace_path.as_str(),
                    stale_relative_path.as_str(),
                    model.as_str()
                ],
            )?;
            deleted_count += 1;
        }

        Ok(ReindexWorkspaceEmbeddingsResponse {
            workspace_path: normalized_workspace_path,
            model,
            indexed_count,
            skipped_count,
            deleted_count,
            failed_count,
        })
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
            .ok_or_else(|| PersistenceServiceError::NotFound("Pulsação não encontrada.".into()))?;
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
            Some("Pulsação cancelada pelo usuário; arquivos já publicados não foram desfeitos."),
        )?;
        tx.execute(
            "UPDATE pulsations SET terminal_reason='user_cancel' WHERE id=?1",
            [&pulse],
        )?;
        Self::mark_effects_for_review(&tx, &pulse, "user_cancel_with_effects")?;
        tx.execute("UPDATE artifact_publications SET state='cancelled' WHERE pulsation_id=?1 AND state IN ('pending','applying')",[&pulse])?;
        tx.execute("UPDATE checkpoints SET status='rejected',updated_at=?2 WHERE pulsation_id=?1 AND status='waiting'",params![pulse,now])?;
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
        let pulsation_id = Self::normalize_pulsation_id(&request.pulsation_id)?;
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
               aro.raw_output AS raw_output,
               aro.structured_output AS structured_output
             FROM agent_runs ar
             INNER JOIN jobs j ON j.id = ar.job_id
             LEFT JOIN agent_run_outputs aro ON aro.agent_run_id = ar.id
             WHERE j.pulsation_id = ?1
             ORDER BY ar.created_at DESC",
        )?;

        let rows = statement.query_map(params![pulsation_id.as_str()], |row| {
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
            })
        })?;

        let mut runs = Vec::new();
        for row in rows {
            runs.push(row?);
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
                .ok_or_else(|| PersistenceServiceError::NotFound("Pulsação ausente".into()))?;
            Self::transition_pulsation(
                &tx,
                &current,
                PulsationStatus::Failed,
                current.stage,
                "recovered_user_cancel",
                Some("Cancelamento do usuário preservado durante recovery."),
            )?;
            tx.execute(
                "UPDATE pulsations SET terminal_reason='user_cancel' WHERE id=?1",
                [&id],
            )?;
            Self::mark_effects_for_review(&tx, &id, "recovered_user_cancel")?;
        }
        tx.execute("UPDATE project_pipeline_state SET base_state='needs_review',blocking_reason='stale_publication_uncertain' WHERE project_id IN (SELECT o.project_id FROM artifact_publications o JOIN artifacts a ON a.id=o.artifact_id JOIN pulsations p ON p.id=o.pulsation_id JOIN jobs j ON j.id=a.producer_job_id WHERE o.state='applying' AND (a.input_revision<>p.input_revision OR a.generation<>j.generation OR a.acceptance='superseded' OR p.status='failed'))",[])?;
        tx.execute("UPDATE artifact_publications SET state='cancelled' WHERE state IN ('pending','applying') AND artifact_id IN (SELECT a.id FROM artifacts a JOIN pulsations p ON p.id=a.pulsation_id JOIN jobs j ON j.id=a.producer_job_id WHERE a.input_revision<>p.input_revision OR a.generation<>j.generation OR a.acceptance='superseded' OR p.status='failed')",[])?;
        tx.execute("UPDATE jobs SET status='failed',cancellation_reason=COALESCE(cancellation_reason,'superseded'),updated_at=?1 WHERE status IN ('pending','running','interrupted') AND (cancellation_reason IN ('user_cancel','superseded') OR EXISTS(SELECT 1 FROM pulsations p WHERE p.id=jobs.pulsation_id AND (p.status IN ('failed','done') OR p.input_revision<>jobs.input_revision)))",[&now])?;
        tx.execute("UPDATE jobs SET status='pending',state_version=(SELECT state_version FROM pulsations WHERE id=jobs.pulsation_id),generation=generation+1,started_at=NULL,cancel_requested_at=NULL,cancellation_reason=NULL,available_at=?1,updated_at=?1 WHERE status IN ('running','interrupted') AND (cancellation_reason IS NULL OR cancellation_reason IN ('shutdown','preempted')) AND NOT EXISTS(SELECT 1 FROM stage_acceptances a WHERE a.job_id=jobs.id) AND EXISTS(SELECT 1 FROM pulsations p WHERE p.id=jobs.pulsation_id AND p.status IN ('planning','running') AND p.input_revision=jobs.input_revision)",[&now])?;
        tx.execute("UPDATE agent_runs SET status='interrupted',updated_at=?1 WHERE status='running' AND NOT EXISTS(SELECT 1 FROM stage_acceptances a WHERE a.agent_run_id=agent_runs.id)",[&now])?;
        Self::recover_coordination(&tx)?;
        tx.commit()?;
        self.reconcile_publications()
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
            params![
                id,
                job.id.as_str(),
                agent_name,
                runtime_config.provider.as_str(),
                runtime_config.model.as_str(),
                now
            ],
        )?;

        Ok(id)
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
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, ?10, ?11, ?11)
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
                execution.request_payload.to_string(),
                execution.response_payload.to_string(),
                execution.raw_output.as_str(),
                execution.structured_output.to_string(),
                execution.schema_json.to_string(),
                execution.tool_trace.to_string(),
                now
            ],
        )?;
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

        let connection = self.connection()?;
        let transaction = connection.unchecked_transaction()?;
        let current = Self::find_pulsation_by_id(&transaction, pulsation_id)?.ok_or_else(|| {
            PersistenceServiceError::NotFound("Pulsação do job não encontrada.".to_string())
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
                Some("Falha transitória detectada. Job reagendado."),
            )?;
            transaction.commit()?;
            return Ok(());
        }

        let (event_type, message) = match failure {
            PersistenceServiceError::Cancelled(message) => ("job_cancelled", message.as_str()),
            _ => ("job_failed", "Falha durante execução da etapa."),
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
        transaction.commit()?;
        Ok(())
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
                    )
            )
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
            PersistenceServiceError::Validation("Job sem referência de pulsação.".to_string())
        })?;

        {
            let connection = self.connection()?;
            let transaction = connection.unchecked_transaction()?;
            let current =
                Self::find_pulsation_by_id(&transaction, pulsation_id)?.ok_or_else(|| {
                    PersistenceServiceError::NotFound("Pulsação do job não encontrada.".to_string())
                })?;

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
        }

        let current = {
            let connection = self.connection()?;
            let transaction = connection.unchecked_transaction()?;
            let current =
                Self::find_pulsation_by_id(&transaction, pulsation_id)?.ok_or_else(|| {
                    PersistenceServiceError::NotFound("Pulsação do job não encontrada.".to_string())
                })?;

            if current.status != PulsationStatus::Running || current.stage != stage {
                transaction.commit()?;
                return Err(
                    if cancellation.is_cancelled() || self.job_cancellation_requested(&job.id)? {
                        PersistenceServiceError::Cancelled(
                            "Execução cancelada antes de iniciar a etapa.".into(),
                        )
                    } else {
                        PersistenceServiceError::Validation(
                            "Execução obsoleta antes de iniciar a etapa.".into(),
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
                PersistenceServiceError::NotFound("Projeto da pulsação não encontrado.".to_string())
            })?;

        let execution_input = StageExecutionInput {
            handoff: Some(self.load_stage_handoff(
                &Self::execution_fence(&connection, &job.id)?,
                stage,
                runtime_config.context_chars_limit,
            )?),
            job_id: job.id.clone(),
            pulsation_id: current.id.clone(),
            project_id: current.project_id.clone(),
            feature_name: current.feature_name.clone(),
            stage_name: stage.as_db_value().to_string(),
            workspace_path: project.workspace_path.clone(),
            runtime_db_path: self.db_path.to_string_lossy().to_string(),
        };

        if runtime_config.context_strategy == ContextStrategy::YamlGraphRag
            && runtime_config.embedding_enabled
            && runtime_config.embedding_provider == EmbeddingProvider::Ollama
        {
            let _ = self
                .reindex_workspace_embeddings_for_workspace(
                    project.workspace_path.as_str(),
                    ReindexWorkspaceEmbeddingsRequest {
                        model: Some(runtime_config.embedding_model.clone()),
                    },
                )
                .await;
        }

        tokio::select! {
            _ = cancellation.cancelled() => {
                return Err(PersistenceServiceError::Cancelled(
                    "Execução cancelada antes da chamada do agente.".to_string()
                ));
            }
            _ = sleep(Duration::from_millis(160)) => {}
        }

        let execution = execute_stage(
            self.embedding_gateway.as_ref(),
            self.llm_router.as_ref(),
            runtime_config,
            &execution_input,
            cancellation,
        )
        .await
        .map_err(PersistenceServiceError::from)?;

        if cancellation.is_cancelled() {
            return Err(PersistenceServiceError::Cancelled(
                "Execução cancelada após resposta do agente.".into(),
            ));
        }
        self.accept_stage_result(
            job,
            agent_run_id,
            execution_input.handoff.as_ref().expect("worker handoff"),
            &execution,
            Path::new(&project.workspace_path),
        )?;
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

        let execution = timeout(
            Duration::from_millis(runtime_config.timeout_ms),
            self.execute_claimed_job(job, &agent_run_id, &runtime_config, cancellation),
        )
        .await;

        self.unregister_job_cancellation(&job.id);

        let execution_result = match execution {
            Ok(result) => result,
            Err(_) => Err(PersistenceServiceError::Retryable(format!(
                "Tempo limite excedido ({}ms) para job {}.",
                runtime_config.timeout_ms, job.id
            ))),
        };

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
                        "Pulsação cancelada pelo usuário.".into(),
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
                self.transition_for_failed_job(job, &error, can_retry)?;

                if can_retry {
                    self.finish_agent_run(&agent_run_id, "retrying")?;
                    self.requeue_job(&job.id, job.attempts, &error.to_string())?;
                    return Ok(());
                }

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

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
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

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

    use super::*;
    use crate::llm::LlmRepository;

    fn prepare_service() -> (tempfile::TempDir, PersistenceService) {
        let root = tempfile::tempdir().expect("tempdir should be created");
        let service = PersistenceService::initialize(root.path())
            .expect("persistence service should initialize");
        (root, service)
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

    #[test]
    fn save_ui_session_rejects_unknown_project() {
        let (_root, service) = prepare_service();
        let response = service.save_ui_session(SaveUiSessionRequest {
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
                project_id: project.id,
            })
            .expect("session should load")
            .session
            .expect("session should exist");

        assert_eq!(loaded.workspace_view_mode, "hitl");
        assert_eq!(loaded.active_view, "vault");
        assert!(loaded.sidebar_collapsed);
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
                pulsation_id: created.id,
            })
            .expect("agent runs should load")
            .runs;

        assert!(runs.len() >= 2);
        assert!(runs.iter().any(|run| run.structured_output.is_some()));
        assert!(runs.iter().any(|run| run.raw_output.is_some()));
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
                pulsation_id: created.id,
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
    fn finishing_cancelled_stream_preserves_visible_partial_output() {
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
        assert_eq!(snapshot.visible_output.as_deref(), Some("partial"));
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
            r#"{"choices":[{"message":{"content":"OK"},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":1}}"#,
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
            job_chain: "fallback-e2e".to_string(),
            agent_role: AgentRole::BackendAgent,
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
    fn router_never_falls_back_after_a_visible_stream_delta() {
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
            job_chain: "stream-no-fallback".to_string(),
            agent_role: AgentRole::BackendAgent,
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
            .expect_err("visible partial must stop the route");
        assert_eq!(error.kind, LlmErrorKind::Timeout);
        assert_eq!(error.visible_partial.as_deref(), Some("partial"));
        let snapshot = service
            .llm_repository
            .execution_snapshot(request.execution_id.as_str())
            .unwrap();
        assert_eq!(snapshot.status, "stream_failed");
        assert_eq!(snapshot.visible_output.as_deref(), Some("partial"));
        assert!(!snapshot.visible_output_complete);
        assert_eq!(snapshot.attempts.len(), 1);
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
            r#"{"choices":[{"message":{"content":"Auth fallback OK"},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":3}}"#,
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
            job_chain: "fallback-401".to_string(),
            agent_role: AgentRole::BackendAgent,
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
            r#"{"choices":[{"message":{"content":"Model fallback OK"},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":3}}"#,
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
                default_model: "good-model".to_string(),
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
                        model_id: "good-model".to_string(),
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
            job_chain: "fallback-lockout".to_string(),
            agent_role: AgentRole::BackendAgent,
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
            job_chain: "route-max-2".to_string(),
            agent_role: AgentRole::BackendAgent,
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
            r#"{"choices":[{"message":{"content":"LKGP Target 2 OK"},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":4}}"#,
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
            job_chain: "route-lkgp".to_string(),
            agent_role: AgentRole::BackendAgent,
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
            job_chain: "cancel-test".to_string(),
            agent_role: AgentRole::BackendAgent,
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
