use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use futures::{StreamExt, stream};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::runtime::Handle;
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agent_timeout::{AgentTimeoutPolicy, ProgressWatchdog, TimeoutReason};
use crate::llm::{AgentRole, LlmRequest, ResponseFormat};
use crate::repository_context::RepositoryContext;
use crate::secret_redactor::redact_secrets;

mod adapters;
mod model_catalog;

pub(crate) use model_catalog::validate_catalog_model_selection;
pub use model_catalog::{
    CliModelCatalogEntry, CliModelCatalogService, CliModelCatalogSnapshot, CliModelCatalogSource,
    CliModelCatalogState, CliModelSelectionSource,
};

#[cfg(windows)]
struct ProcessTreeJob {
    handle: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
unsafe impl Send for ProcessTreeJob {}

#[cfg(windows)]
impl ProcessTreeJob {
    fn assign(child: &tokio::process::Child) -> std::io::Result<Self> {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };

        let process = child
            .raw_handle()
            .ok_or_else(|| std::io::Error::other("handle do processo CLI indisponível"))?
            as windows_sys::Win32::Foundation::HANDLE;
        // SAFETY: all pointers are either null (unnamed job/default security) or point to a
        // correctly sized initialized Windows structure for the duration of the calls.
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                return Err(std::io::Error::last_os_error());
            }
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) == 0
            {
                let error = std::io::Error::last_os_error();
                CloseHandle(handle);
                return Err(error);
            }
            if AssignProcessToJobObject(handle, process) == 0 {
                let error = std::io::Error::last_os_error();
                CloseHandle(handle);
                return Err(error);
            }
            Ok(Self { handle })
        }
    }

    fn terminate(&self) {
        // SAFETY: `handle` remains owned by this guard until Drop.
        unsafe {
            windows_sys::Win32::System::JobObjects::TerminateJobObject(self.handle, 1);
        }
    }
}

#[cfg(windows)]
impl Drop for ProcessTreeJob {
    fn drop(&mut self) {
        // KILL_ON_JOB_CLOSE makes this a final safety net for grandchildren as well.
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

#[cfg(not(windows))]
struct ProcessTreeJob;

#[cfg(not(windows))]
impl ProcessTreeJob {
    fn assign(_child: &tokio::process::Child) -> std::io::Result<Self> {
        Ok(Self)
    }

    fn terminate(&self) {}
}

const MAX_MODEL_CHARS: usize = 200;
const MAX_CUSTOM_ARGS: usize = 64;
const MAX_CUSTOM_ARG_CHARS: usize = 1_024;
const MAX_EVENT_CHARS: usize = 64 * 1_024;
const MAX_PENDING_STREAM_BYTES: usize = 256 * 1_024;
const MAX_RUN_OUTPUT_BYTES: usize = 32 * 1024 * 1024;
const MAX_RUN_EVENTS: usize = 100_000;
const PROCESS_EVENT_CHANNEL_CAPACITY: usize = 256;
const DEFAULT_MANUAL_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const MAX_SAFE_SNAPSHOT_FILES: usize = 10_000;
const MAX_SAFE_SNAPSHOT_BYTES: u64 = 64 * 1024 * 1024;
const CLI_DISCOVERY_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const CLI_VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const CLI_AUTH_PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_PROBE_OUTPUT_BYTES: usize = 128 * 1024;
const MAX_MODEL_CATALOG_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_COMPANION_CLI_OUTPUT_BYTES: usize = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum TerminalError {
    #[error("configuração inválida de Agents: {0}")]
    Validation(String),
    #[error("registro de Agents não encontrado: {0}")]
    NotFound(String),
    #[error("configuração de Agents foi alterada em outra operação")]
    Conflict,
    #[error("falha no banco local de Agents: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("falha de I/O no runtime CLI: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON inválido no runtime CLI: {0}")]
    Json(#[from] serde_json::Error),
    #[error("execução CLI falhou: {0}")]
    Process(String),
    #[error("execução CLI excedeu o tempo limite")]
    Timeout,
    #[error("execução CLI excedeu o limite de saída")]
    OutputLimit,
    #[error("execução CLI foi cancelada")]
    Cancelled,
    #[error("{0} requer autenticação")]
    AuthRequired(String),
    #[error("versão do {0} incompatível com o contrato CLI")]
    VersionUnsupported(String),
    #[error("{0} atingiu o limite temporário de requisições")]
    RateLimited(String),
    #[error("transição inválida da execução CLI: {from} -> {to}")]
    InvalidTransition { from: String, to: String },
    #[error("falha ao remover segredos da saída CLI: {0}")]
    Redaction(String),
    #[error("falha de segurança no runtime CLI: {0}")]
    Security(String),
}

impl TerminalError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Validation(_) => "cli_configuration_invalid",
            Self::NotFound(_) => "cli_not_found",
            Self::Conflict => "cli_configuration_conflict",
            Self::Database(_) => "cli_persistence_failed",
            Self::Io(_) => "cli_io_failed",
            Self::Json(_) => "cli_output_invalid",
            Self::Process(_) => "cli_process_failed",
            Self::Timeout => "cli_timeout",
            Self::OutputLimit => "cli_output_limit",
            Self::Cancelled => "cli_cancelled",
            Self::AuthRequired(_) => "cli_auth_required",
            Self::VersionUnsupported(_) => "cli_version_unsupported",
            Self::RateLimited(_) => "cli_rate_limited",
            Self::InvalidTransition { .. } => "cli_invalid_state_transition",
            Self::Redaction(_) => "cli_redaction_failed",
            Self::Security(_) => "cli_security_failed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentExecutionMode {
    ApiDirect,
    CliProcess,
}

impl AgentExecutionMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ApiDirect => "api_direct",
            Self::CliProcess => "cli_process",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "api_direct" => Some(Self::ApiDirect),
            "cli_process" => Some(Self::CliProcess),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CliBackend {
    #[default]
    Codex,
    Claude,
    Gemini,
    Opencode,
    Custom,
}

impl CliBackend {
    pub const ALL: [Self; 5] = [
        Self::Codex,
        Self::Claude,
        Self::Gemini,
        Self::Opencode,
        Self::Custom,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Gemini => "gemini",
            Self::Opencode => "opencode",
            Self::Custom => "custom",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude",
            Self::Gemini => "Gemini",
            Self::Opencode => "OpenCode",
            Self::Custom => "Custom",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "codex" => Some(Self::Codex),
            "claude" => Some(Self::Claude),
            "gemini" => Some(Self::Gemini),
            "opencode" => Some(Self::Opencode),
            "custom" => Some(Self::Custom),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingLevel {
    Default,
    Low,
    Medium,
    High,
}

impl ThinkingLevel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "default" => Some(Self::Default),
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionOrigin {
    Feature,
    Pipeline,
    Learning,
    Manual,
}

impl ExecutionOrigin {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Feature => "feature",
            Self::Pipeline => "pipeline",
            Self::Learning => "learning",
            Self::Manual => "manual",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "feature" => Some(Self::Feature),
            "pipeline" => Some(Self::Pipeline),
            "learning" => Some(Self::Learning),
            "manual" => Some(Self::Manual),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalRunStatus {
    Queued,
    Starting,
    Running,
    AwaitingFallback,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

impl TerminalRunStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::AwaitingFallback => "awaiting_fallback",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "queued" => Some(Self::Queued),
            "starting" => Some(Self::Starting),
            "running" => Some(Self::Running),
            "awaiting_fallback" => Some(Self::AwaitingFallback),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            "interrupted" => Some(Self::Interrupted),
            _ => None,
        }
    }

    pub const fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Starting | Self::Running)
    }

    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Interrupted
        )
    }

    const fn can_transition_to(self, next: Self) -> bool {
        if self as u8 == next as u8 {
            return true;
        }
        match self {
            Self::Queued => matches!(
                next,
                Self::Starting | Self::Failed | Self::Cancelled | Self::Interrupted
            ),
            Self::Starting => matches!(
                next,
                Self::Running | Self::Failed | Self::Cancelled | Self::Interrupted
            ),
            Self::Running => matches!(
                next,
                Self::AwaitingFallback
                    | Self::Completed
                    | Self::Failed
                    | Self::Cancelled
                    | Self::Interrupted
            ),
            Self::Failed => matches!(next, Self::AwaitingFallback),
            Self::AwaitingFallback | Self::Completed | Self::Cancelled | Self::Interrupted => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalEventKind {
    System,
    Message,
    ToolUse,
    ToolResult,
    Stdout,
    Stderr,
    Warning,
    Error,
    Completed,
}

impl TerminalEventKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Message => "message",
            Self::ToolUse => "tool_use",
            Self::ToolResult => "tool_result",
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
            Self::Warning => "warning",
            Self::Error => "error",
            Self::Completed => "completed",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "system" => Some(Self::System),
            "message" => Some(Self::Message),
            "tool_use" => Some(Self::ToolUse),
            "tool_result" => Some(Self::ToolResult),
            "stdout" => Some(Self::Stdout),
            "stderr" => Some(Self::Stderr),
            "warning" => Some(Self::Warning),
            "error" => Some(Self::Error),
            "completed" => Some(Self::Completed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanonicalAgentDescriptor {
    pub role: AgentRole,
    pub name: &'static str,
    pub glyph: &'static str,
    pub stage: &'static str,
    pub color_key: &'static str,
}

pub const CANONICAL_AGENTS: [CanonicalAgentDescriptor; 8] = [
    CanonicalAgentDescriptor {
        role: AgentRole::FeatureAgent,
        name: "FeatureAgent",
        glyph: "+",
        stage: "Feature",
        color_key: "cyan",
    },
    CanonicalAgentDescriptor {
        role: AgentRole::IntakeRouter,
        name: "IntakeRouter",
        glyph: ">",
        stage: "Intake",
        color_key: "blue",
    },
    CanonicalAgentDescriptor {
        role: AgentRole::TaskSpecifier,
        name: "TaskSpecifier",
        glyph: "#",
        stage: "SPECIFICATION",
        color_key: "purple",
    },
    CanonicalAgentDescriptor {
        role: AgentRole::ContractAgent,
        name: "ContractAgent",
        glyph: "$",
        stage: "Contract",
        color_key: "gold",
    },
    CanonicalAgentDescriptor {
        role: AgentRole::BackendAgent,
        name: "BackendAgent",
        glyph: "@",
        stage: "Code",
        color_key: "green",
    },
    CanonicalAgentDescriptor {
        role: AgentRole::TestAgent,
        name: "TestAgent",
        glyph: "!",
        stage: "Test",
        color_key: "red",
    },
    CanonicalAgentDescriptor {
        role: AgentRole::Integrator,
        name: "Integrator",
        glyph: "&",
        stage: "Integrator",
        color_key: "cyan",
    },
    CanonicalAgentDescriptor {
        role: AgentRole::LearningAgent,
        name: "LearningAgent",
        glyph: "*",
        stage: "Learning",
        color_key: "magenta",
    },
];

pub fn canonical_agent(role: AgentRole) -> &'static CanonicalAgentDescriptor {
    CANONICAL_AGENTS
        .iter()
        .find(|descriptor| descriptor.role == role)
        .expect("all AgentRole variants must have a canonical descriptor")
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionSlot {
    Canonical { agent_role: AgentRole },
    FreeForm { id: String },
}

impl SessionSlot {
    fn persisted_id(&self, project_id: &str) -> String {
        match self {
            Self::Canonical { agent_role } => canonical_slot_id(project_id, *agent_role),
            Self::FreeForm { id } => id.clone(),
        }
    }

    pub fn agent_role(&self) -> Option<AgentRole> {
        match self {
            Self::Canonical { agent_role } => Some(*agent_role),
            Self::FreeForm { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionPolicy {
    FreshPerRun,
    ResumePerPulsation,
    ResumeExplicit,
}

impl SessionPolicy {
    fn as_str(self) -> &'static str {
        match self {
            Self::FreshPerRun => "fresh_per_run",
            Self::ResumePerPulsation => "resume_per_pulsation",
            Self::ResumeExplicit => "resume_explicit",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "fresh_per_run" => Some(Self::FreshPerRun),
            "resume_per_pulsation" => Some(Self::ResumePerPulsation),
            "resume_explicit" => Some(Self::ResumeExplicit),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSlot {
    pub id: String,
    pub project_id: String,
    pub kind: String,
    pub agent_role: Option<AgentRole>,
    pub label: String,
    pub execution_mode: AgentExecutionMode,
    pub backend: CliBackend,
    pub model: Option<String>,
    pub model_profile: Option<String>,
    pub model_source: CliModelSelectionSource,
    pub thinking_level: ThinkingLevel,
    pub session_policy: SessionPolicy,
    pub custom_executable: Option<String>,
    pub custom_args: Vec<String>,
    pub custom_risk_ack: bool,
    pub position: u32,
    pub archived: bool,
    pub version: u64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateFreeFormSlotRequest {
    pub project_id: String,
    pub label: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateFreeFormSlotRequest {
    pub id: String,
    pub project_id: String,
    pub label: String,
    pub execution_mode: AgentExecutionMode,
    pub backend: CliBackend,
    pub model: Option<String>,
    #[serde(default)]
    pub model_profile: Option<String>,
    #[serde(default)]
    pub model_source: CliModelSelectionSource,
    pub thinking_level: ThinkingLevel,
    pub session_policy: SessionPolicy,
    pub custom_executable: Option<String>,
    #[serde(default)]
    pub custom_args: Vec<String>,
    #[serde(default)]
    pub custom_risk_ack: bool,
    pub position: u32,
    pub archived: bool,
    pub expected_version: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CliAgentConfig {
    pub project_id: String,
    pub agent_role: AgentRole,
    pub execution_mode: AgentExecutionMode,
    pub backend: CliBackend,
    pub model: Option<String>,
    pub model_profile: Option<String>,
    pub model_source: CliModelSelectionSource,
    pub thinking_level: ThinkingLevel,
    pub custom_executable: Option<String>,
    pub custom_args: Vec<String>,
    pub custom_risk_ack: bool,
    pub version: u64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCliAgentConfigRequest {
    pub project_id: String,
    pub agent_role: AgentRole,
    pub execution_mode: AgentExecutionMode,
    pub backend: CliBackend,
    pub model: Option<String>,
    #[serde(default)]
    pub model_profile: Option<String>,
    #[serde(default)]
    pub model_source: CliModelSelectionSource,
    pub thinking_level: ThinkingLevel,
    pub custom_executable: Option<String>,
    #[serde(default)]
    pub custom_args: Vec<String>,
    #[serde(default)]
    pub custom_risk_ack: bool,
    pub expected_version: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalRun {
    pub id: String,
    pub project_id: String,
    pub slot: SessionSlot,
    pub slot_id: String,
    pub agent_role: Option<AgentRole>,
    pub origin: ExecutionOrigin,
    pub execution_mode: AgentExecutionMode,
    pub backend: Option<CliBackend>,
    pub model: Option<String>,
    pub requested_model_profile: Option<String>,
    pub model_source: CliModelSelectionSource,
    pub observed_model: Option<String>,
    pub native_session_id: Option<String>,
    pub pulsation_id: Option<String>,
    pub job_id: Option<String>,
    pub agent_run_id: Option<String>,
    pub llm_execution_id: Option<String>,
    pub status: TerminalRunStatus,
    pub exit_code: Option<i32>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub updated_at: String,
    pub deadline_at: Option<String>,
    pub termination_reason: Option<String>,
    pub output_bytes: u64,
    pub event_count: u64,
    pub workspace_snapshot_hash: Option<String>,
    pub launch_identity_sha256: Option<String>,
    pub detected_cli_version: Option<String>,
    pub adapter_contract_version: u32,
    pub parent_run_id: Option<String>,
    pub continuation_reason: Option<String>,
    pub session_fingerprint: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CreateTerminalRunRequest {
    pub project_id: String,
    pub agent_role: AgentRole,
    pub origin: ExecutionOrigin,
    pub execution_mode: AgentExecutionMode,
    pub backend: Option<CliBackend>,
    pub model: Option<String>,
    pub pulsation_id: Option<String>,
    pub job_id: Option<String>,
    pub agent_run_id: Option<String>,
    pub llm_execution_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CreateSessionRunRequest {
    pub project_id: String,
    pub slot: SessionSlot,
    pub origin: ExecutionOrigin,
    pub execution_mode: AgentExecutionMode,
    pub backend: Option<CliBackend>,
    pub model: Option<String>,
    pub pulsation_id: Option<String>,
    pub job_id: Option<String>,
    pub agent_run_id: Option<String>,
    pub llm_execution_id: Option<String>,
    pub parent_run_id: Option<String>,
    pub continuation_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalEvent {
    pub id: String,
    pub run_id: String,
    pub sequence: u64,
    pub kind: TerminalEventKind,
    pub stream: Option<String>,
    pub content: String,
    pub redaction_count: u64,
    pub redaction_categories: Vec<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalEventEnvelope {
    pub protocol_version: u8,
    pub project_id: String,
    pub agent_role: Option<AgentRole>,
    pub run_id: String,
    pub sequence: u64,
    pub run: TerminalRun,
    pub event: TerminalEvent,
}

impl TerminalEventEnvelope {
    pub fn event_name(&self) -> &'static str {
        match self.event.kind {
            TerminalEventKind::System => "agents://system",
            TerminalEventKind::Message => "agents://message",
            TerminalEventKind::ToolUse => "agents://tool-use",
            TerminalEventKind::ToolResult => "agents://tool-result",
            TerminalEventKind::Stdout => "agents://stdout",
            TerminalEventKind::Stderr => "agents://stderr",
            TerminalEventKind::Warning => "agents://warning",
            TerminalEventKind::Error => "agents://error",
            TerminalEventKind::Completed => "agents://completed",
        }
    }
}

pub trait TerminalEventSink: Send + Sync {
    fn emit(&self, envelope: &TerminalEventEnvelope);
}

#[derive(Debug, Clone)]
pub struct TerminalRepository {
    db_path: PathBuf,
}

impl TerminalRepository {
    pub fn new(db_path: impl Into<PathBuf>) -> Self {
        Self {
            db_path: db_path.into(),
        }
    }

    fn connection(&self) -> Result<Connection, TerminalError> {
        let connection = Connection::open(&self.db_path)?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(connection)
    }

    pub fn ensure_project_configs(
        &self,
        project_id: &str,
        codex_ready: bool,
    ) -> Result<Vec<CliAgentConfig>, TerminalError> {
        let project_id = required(project_id, "projectId")?;
        let mut connection = self.connection()?;
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1 AND is_archived=0)",
            [project_id],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(TerminalError::NotFound("projeto ativo".into()));
        }
        let now = Utc::now().to_rfc3339();
        let default_mode = if codex_ready {
            "cli_process"
        } else {
            "api_direct"
        };
        let transaction = connection.transaction()?;
        for (position, role) in AgentRole::ALL.into_iter().enumerate() {
            transaction.execute(
                "INSERT OR IGNORE INTO cli_agent_configs(
                   project_id,agent_role,execution_mode,backend,thinking_level,
                   custom_args_json,custom_risk_ack,version,created_at,updated_at
                 ) VALUES(?1,?2,?3,'codex','default','[]',0,1,?4,?4)",
                params![project_id, role.as_str(), default_mode, now],
            )?;
            let descriptor = canonical_agent(role);
            transaction.execute(
                "INSERT OR IGNORE INTO terminal_slots(
                   id,project_id,kind,agent_role,label,execution_mode,backend,model,
                   thinking_level,session_policy,custom_executable,custom_args_json,
                   custom_risk_ack,position,archived,version,created_at,updated_at,
                   model_profile,model_source
                 )
                 SELECT ?1,project_id,'canonical',agent_role,?2,execution_mode,backend,
                        model,thinking_level,'fresh_per_run',custom_executable,
                         custom_args_json,custom_risk_ack,?3,0,version,created_at,updated_at,
                         model_profile,model_source
                 FROM cli_agent_configs WHERE project_id=?4 AND agent_role=?5",
                params![
                    canonical_slot_id(project_id, role),
                    descriptor.name,
                    position,
                    project_id,
                    role.as_str()
                ],
            )?;
            transaction.execute(
                "UPDATE terminal_slots SET
                   label=?1,execution_mode=(SELECT execution_mode FROM cli_agent_configs
                     WHERE project_id=?2 AND agent_role=?3),
                   backend=(SELECT backend FROM cli_agent_configs
                     WHERE project_id=?2 AND agent_role=?3),
                   model=(SELECT model FROM cli_agent_configs
                     WHERE project_id=?2 AND agent_role=?3),
                    thinking_level=(SELECT thinking_level FROM cli_agent_configs
                      WHERE project_id=?2 AND agent_role=?3),
                    model_profile=(SELECT model_profile FROM cli_agent_configs
                      WHERE project_id=?2 AND agent_role=?3),
                    model_source=(SELECT model_source FROM cli_agent_configs
                      WHERE project_id=?2 AND agent_role=?3),
                   custom_executable=(SELECT custom_executable FROM cli_agent_configs
                     WHERE project_id=?2 AND agent_role=?3),
                   custom_args_json=(SELECT custom_args_json FROM cli_agent_configs
                     WHERE project_id=?2 AND agent_role=?3),
                   custom_risk_ack=(SELECT custom_risk_ack FROM cli_agent_configs
                     WHERE project_id=?2 AND agent_role=?3),position=?4,updated_at=?5
                 WHERE project_id=?2 AND agent_role=?3 AND kind='canonical'",
                params![descriptor.name, project_id, role.as_str(), position, now],
            )?;
        }
        transaction.commit()?;
        self.list_configs(project_id)
    }

    pub fn list_configs(&self, project_id: &str) -> Result<Vec<CliAgentConfig>, TerminalError> {
        let project_id = required(project_id, "projectId")?;
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT project_id,agent_role,execution_mode,backend,model,thinking_level,
                    custom_executable,custom_args_json,custom_risk_ack,version,created_at,updated_at,
                    model_profile,model_source
             FROM cli_agent_configs WHERE project_id=?1 ORDER BY rowid",
        )?;
        let rows = statement.query_map([project_id], map_config)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(TerminalError::from)
    }

    pub fn update_config(
        &self,
        mut request: UpdateCliAgentConfigRequest,
    ) -> Result<CliAgentConfig, TerminalError> {
        request.project_id = required(&request.project_id, "projectId")?.to_string();
        request.model = normalize_model_id(request.model)?;
        request.model_profile = normalize_model_profile(request.model_profile)?;
        validate_model_selection(
            request.backend,
            request.model.as_deref(),
            request.model_profile.as_deref(),
            request.model_source,
        )?;
        validate_catalog_model_selection(
            &self.connection()?,
            &request.project_id,
            request.backend,
            request.model.as_deref(),
            request.model_profile.as_deref(),
            request.model_source,
        )?;
        let current = self
            .get_config(&request.project_id, request.agent_role)
            .ok();
        let custom_identity_changed = request.backend == CliBackend::Custom
            && current.as_ref().is_none_or(|current| {
                current.backend != CliBackend::Custom
                    || current.custom_executable != request.custom_executable
                    || current.custom_args != request.custom_args
            });
        if custom_identity_changed {
            request.custom_risk_ack = false;
            if request.execution_mode == AgentExecutionMode::CliProcess {
                return Err(TerminalError::Validation(
                    "alterar o executável ou argumentos Custom exige salvar em API Mode e confirmar o novo risco"
                        .into(),
                ));
            }
        }
        validate_custom(&request)?;
        let custom_executable = if request.backend == CliBackend::Custom {
            request.custom_executable.as_deref()
        } else {
            None
        };
        let custom_args = if request.backend == CliBackend::Custom {
            serde_json::to_string(&request.custom_args)?
        } else {
            "[]".to_string()
        };
        let now = Utc::now().to_rfc3339();
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let changed = transaction.execute(
            "UPDATE cli_agent_configs SET
               execution_mode=?1,backend=?2,model=?3,thinking_level=?4,
               custom_executable=?5,custom_args_json=?6,custom_risk_ack=?7,
               version=version+1,updated_at=?8,model_profile=?9,model_source=?10
             WHERE project_id=?11 AND agent_role=?12 AND version=?13",
            params![
                request.execution_mode.as_str(),
                request.backend.as_str(),
                request.model,
                request.thinking_level.as_str(),
                custom_executable,
                custom_args,
                request.backend == CliBackend::Custom && request.custom_risk_ack,
                now,
                request.model_profile,
                request.model_source.as_str(),
                request.project_id,
                request.agent_role.as_str(),
                request.expected_version,
            ],
        )?;
        if changed == 0 {
            let exists: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM cli_agent_configs WHERE project_id=?1 AND agent_role=?2)",
                params![request.project_id, request.agent_role.as_str()],
                |row| row.get(0),
            )?;
            return Err(if exists {
                TerminalError::Conflict
            } else {
                TerminalError::NotFound("configuração do agente".into())
            });
        }
        transaction.execute(
            "UPDATE terminal_slots SET execution_mode=?1,backend=?2,model=?3,
                thinking_level=?4,custom_executable=?5,custom_args_json=?6,
                custom_risk_ack=?7,version=version+1,updated_at=?8,
                model_profile=?9,model_source=?10
              WHERE project_id=?11 AND agent_role=?12 AND kind='canonical'",
            params![
                request.execution_mode.as_str(),
                request.backend.as_str(),
                request.model,
                request.thinking_level.as_str(),
                custom_executable,
                custom_args,
                request.backend == CliBackend::Custom && request.custom_risk_ack,
                now,
                request.model_profile,
                request.model_source.as_str(),
                request.project_id,
                request.agent_role.as_str(),
            ],
        )?;
        transaction.commit()?;
        self.get_config(&request.project_id, request.agent_role)
    }

    pub fn get_config(
        &self,
        project_id: &str,
        role: AgentRole,
    ) -> Result<CliAgentConfig, TerminalError> {
        self.connection()?
            .query_row(
                "SELECT project_id,agent_role,execution_mode,backend,model,thinking_level,
                        custom_executable,custom_args_json,custom_risk_ack,version,created_at,updated_at,
                        model_profile,model_source
                 FROM cli_agent_configs WHERE project_id=?1 AND agent_role=?2",
                params![project_id, role.as_str()],
                map_config,
            )
            .optional()?
            .ok_or_else(|| TerminalError::NotFound("configuração do agente".into()))
    }

    pub fn list_free_form_slots(
        &self,
        project_id: &str,
        include_archived: bool,
    ) -> Result<Vec<TerminalSlot>, TerminalError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id,project_id,kind,agent_role,label,execution_mode,backend,model,
                    thinking_level,session_policy,custom_executable,custom_args_json,
                    custom_risk_ack,position,archived,version,created_at,updated_at,
                    model_profile,model_source
             FROM terminal_slots
             WHERE project_id=?1 AND kind='free_form' AND (?2 OR archived=0)
             ORDER BY archived,position,created_at,id",
        )?;
        let rows = statement.query_map(
            params![required(project_id, "projectId")?, include_archived],
            map_terminal_slot,
        )?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(TerminalError::from)
    }

    pub fn get_slot(
        &self,
        project_id: &str,
        slot: &SessionSlot,
    ) -> Result<TerminalSlot, TerminalError> {
        self.connection()?
            .query_row(
                "SELECT id,project_id,kind,agent_role,label,execution_mode,backend,model,
                        thinking_level,session_policy,custom_executable,custom_args_json,
                         custom_risk_ack,position,archived,version,created_at,updated_at,
                         model_profile,model_source
                 FROM terminal_slots WHERE id=?1 AND project_id=?2",
                params![
                    slot.persisted_id(required(project_id, "projectId")?),
                    project_id
                ],
                map_terminal_slot,
            )
            .optional()?
            .ok_or_else(|| TerminalError::NotFound("slot de terminal".into()))
    }

    pub fn create_free_form_slot(
        &self,
        request: CreateFreeFormSlotRequest,
    ) -> Result<TerminalSlot, TerminalError> {
        let project_id = required(&request.project_id, "projectId")?;
        let label = validate_slot_label(&request.label)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let active_count: u32 = transaction.query_row(
            "SELECT COUNT(*) FROM terminal_slots
             WHERE project_id=?1 AND kind='free_form' AND archived=0",
            [project_id],
            |row| row.get(0),
        )?;
        if active_count >= 8 {
            return Err(TerminalError::Validation(
                "cada projeto aceita no máximo 8 agentes livres ativos".into(),
            ));
        }
        let position: u32 = transaction.query_row(
            "SELECT COALESCE(MAX(position),-1)+1 FROM terminal_slots
             WHERE project_id=?1 AND kind='free_form' AND archived=0",
            [project_id],
            |row| row.get(0),
        )?;
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        transaction.execute(
            "INSERT INTO terminal_slots(
               id,project_id,kind,agent_role,label,execution_mode,backend,model,
               thinking_level,session_policy,custom_executable,custom_args_json,
               custom_risk_ack,position,archived,version,created_at,updated_at
             ) VALUES(?1,?2,'free_form',NULL,?3,'api_direct','codex',NULL,
                      'default','resume_explicit',NULL,'[]',0,?4,0,1,?5,?5)",
            params![id, project_id, label, position, now],
        )?;
        transaction.commit()?;
        self.get_slot(project_id, &SessionSlot::FreeForm { id })
    }

    pub fn update_free_form_slot(
        &self,
        mut request: UpdateFreeFormSlotRequest,
    ) -> Result<TerminalSlot, TerminalError> {
        request.project_id = required(&request.project_id, "projectId")?.to_string();
        request.id = required(&request.id, "slotId")?.to_string();
        request.label = validate_slot_label(&request.label)?.to_string();
        request.model = normalize_model_id(request.model)?;
        request.model_profile = normalize_model_profile(request.model_profile)?;
        validate_model_selection(
            request.backend,
            request.model.as_deref(),
            request.model_profile.as_deref(),
            request.model_source,
        )?;
        validate_catalog_model_selection(
            &self.connection()?,
            &request.project_id,
            request.backend,
            request.model.as_deref(),
            request.model_profile.as_deref(),
            request.model_source,
        )?;
        let current = self.get_slot(
            &request.project_id,
            &SessionSlot::FreeForm {
                id: request.id.clone(),
            },
        )?;
        let custom_identity_changed = request.backend == CliBackend::Custom
            && (current.backend != CliBackend::Custom
                || current.custom_executable != request.custom_executable
                || current.custom_args != request.custom_args);
        if custom_identity_changed {
            request.custom_risk_ack = false;
            if request.execution_mode == AgentExecutionMode::CliProcess {
                return Err(TerminalError::Validation(
                    "alterar o executável ou argumentos Custom exige salvar em API Mode e confirmar o novo risco"
                        .into(),
                ));
            }
        }
        validate_custom_parts(
            request.execution_mode,
            request.backend,
            request.custom_executable.as_deref(),
            &request.custom_args,
            request.custom_risk_ack,
        )?;
        if !current.archived && request.archived {
            let active: bool = self.connection()?.query_row(
                "SELECT EXISTS(SELECT 1 FROM terminal_runs
                 WHERE project_id=?1 AND slot_id=?2
                   AND status IN ('queued','starting','running'))",
                params![request.project_id, request.id],
                |row| row.get(0),
            )?;
            if active {
                return Err(TerminalError::Validation(
                    "cancele a execução ativa antes de arquivar o agente livre".into(),
                ));
            }
        }
        if current.archived && !request.archived {
            let active_count: u32 = self.connection()?.query_row(
                "SELECT COUNT(*) FROM terminal_slots
                 WHERE project_id=?1 AND kind='free_form' AND archived=0",
                [request.project_id.as_str()],
                |row| row.get(0),
            )?;
            if active_count >= 8 {
                return Err(TerminalError::Validation(
                    "cada projeto aceita no máximo 8 agentes livres ativos".into(),
                ));
            }
        }
        let custom_executable = (request.backend == CliBackend::Custom)
            .then_some(request.custom_executable.as_deref())
            .flatten();
        let custom_args = if request.backend == CliBackend::Custom {
            serde_json::to_string(&request.custom_args)?
        } else {
            "[]".into()
        };
        let changed = self.connection()?.execute(
            "UPDATE terminal_slots SET label=?1,execution_mode=?2,backend=?3,model=?4,
               thinking_level=?5,session_policy=?6,custom_executable=?7,
               custom_args_json=?8,custom_risk_ack=?9,position=?10,archived=?11,
               version=version+1,updated_at=?12,model_profile=?13,model_source=?14
             WHERE id=?15 AND project_id=?16 AND kind='free_form' AND version=?17",
            params![
                request.label,
                request.execution_mode.as_str(),
                request.backend.as_str(),
                request.model,
                request.thinking_level.as_str(),
                request.session_policy.as_str(),
                custom_executable,
                custom_args,
                request.backend == CliBackend::Custom && request.custom_risk_ack,
                request.position,
                request.archived,
                Utc::now().to_rfc3339(),
                request.model_profile,
                request.model_source.as_str(),
                request.id,
                request.project_id,
                request.expected_version,
            ],
        )?;
        if changed == 0 {
            return Err(TerminalError::Conflict);
        }
        self.get_slot(
            &request.project_id,
            &SessionSlot::FreeForm { id: request.id },
        )
    }

    pub fn delete_free_form_slot(
        &self,
        project_id: &str,
        slot_id: &str,
        expected_version: u64,
    ) -> Result<(), TerminalError> {
        let project_id = required(project_id, "projectId")?;
        let slot_id = required(slot_id, "slotId")?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let active: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM terminal_runs
             WHERE project_id=?1 AND slot_id=?2
               AND status IN ('queued','starting','running'))",
            params![project_id, slot_id],
            |row| row.get(0),
        )?;
        if active {
            return Err(TerminalError::Validation(
                "cancele a execução ativa antes de excluir o agente livre".into(),
            ));
        }
        transaction.execute(
            "DELETE FROM terminal_events WHERE run_id IN (
               SELECT id FROM terminal_runs WHERE project_id=?1 AND slot_id=?2
             )",
            params![project_id, slot_id],
        )?;
        transaction.execute(
            "DELETE FROM terminal_runs WHERE project_id=?1 AND slot_id=?2",
            params![project_id, slot_id],
        )?;
        let changed = transaction.execute(
            "DELETE FROM terminal_slots
             WHERE id=?1 AND project_id=?2 AND kind='free_form' AND version=?3",
            params![slot_id, project_id, expected_version],
        )?;
        if changed == 0 {
            return Err(TerminalError::Conflict);
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn reorder_free_form_slots(
        &self,
        project_id: &str,
        slot_ids: &[String],
    ) -> Result<(), TerminalError> {
        let project_id = required(project_id, "projectId")?;
        let requested = slot_ids.iter().cloned().collect::<HashSet<_>>();
        if requested.len() != slot_ids.len() {
            return Err(TerminalError::Validation(
                "a ordenação contém slots duplicados".into(),
            ));
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let current = {
            let mut statement = transaction.prepare(
                "SELECT id FROM terminal_slots
                 WHERE project_id=?1 AND kind='free_form' AND archived=0",
            )?;
            let rows = statement.query_map([project_id], |row| row.get::<_, String>(0))?;
            rows.collect::<Result<HashSet<_>, _>>()?
        };
        if current != requested {
            return Err(TerminalError::Conflict);
        }
        let now = Utc::now().to_rfc3339();
        for (position, slot_id) in slot_ids.iter().enumerate() {
            transaction.execute(
                "UPDATE terminal_slots SET position=?1,version=version+1,updated_at=?2
                 WHERE id=?3 AND project_id=?4 AND kind='free_form' AND archived=0",
                params![position, now, slot_id, project_id],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn project_workspace(&self, project_id: &str) -> Result<PathBuf, TerminalError> {
        let path: Option<String> = self
            .connection()?
            .query_row(
                "SELECT workspace_path FROM projects WHERE id=?1 AND is_archived=0",
                [required(project_id, "projectId")?],
                |row| row.get(0),
            )
            .optional()?;
        path.map(PathBuf::from)
            .ok_or_else(|| TerminalError::NotFound("workspace do projeto".into()))
    }

    pub fn create_run(
        &self,
        request: CreateTerminalRunRequest,
    ) -> Result<TerminalRun, TerminalError> {
        self.create_session_run(CreateSessionRunRequest {
            project_id: request.project_id,
            slot: SessionSlot::Canonical {
                agent_role: request.agent_role,
            },
            origin: request.origin,
            execution_mode: request.execution_mode,
            backend: request.backend,
            model: request.model,
            pulsation_id: request.pulsation_id,
            job_id: request.job_id,
            agent_run_id: request.agent_run_id,
            llm_execution_id: request.llm_execution_id,
            parent_run_id: None,
            continuation_reason: None,
        })
    }

    pub fn create_session_run(
        &self,
        request: CreateSessionRunRequest,
    ) -> Result<TerminalRun, TerminalError> {
        required(&request.project_id, "projectId")?;
        if request.execution_mode == AgentExecutionMode::CliProcess && request.backend.is_none() {
            return Err(TerminalError::Validation(
                "backend é obrigatório para uma execução CLI".into(),
            ));
        }
        let slot = self.get_slot(&request.project_id, &request.slot)?;
        if slot.archived {
            return Err(TerminalError::Validation(
                "o slot de terminal está arquivado".into(),
            ));
        }
        if request.origin != ExecutionOrigin::Manual && slot.kind == "free_form" {
            return Err(TerminalError::Validation(
                "agentes livres aceitam somente execuções manuais".into(),
            ));
        }
        let continuation_reason = request
            .continuation_reason
            .as_deref()
            .map(|value| match value {
                "explicit_resume" | "resume_per_pulsation" | "restart_recovery" => Ok(value),
                _ => Err(TerminalError::Validation(
                    "razão de continuidade inválida".into(),
                )),
            })
            .transpose()?;
        if request.parent_run_id.is_some() != continuation_reason.is_some() {
            return Err(TerminalError::Validation(
                "retomada exige run pai e razão de continuidade".into(),
            ));
        }
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let slot_id = request.slot.persisted_id(&request.project_id);
        let agent_role = request.slot.agent_role();
        let (slot_kind, free_form_slot_id, free_form_label) = match &request.slot {
            SessionSlot::Canonical { .. } => ("canonical", None, None),
            SessionSlot::FreeForm { id } => {
                ("free_form", Some(id.as_str()), Some(slot.label.as_str()))
            }
        };
        self.connection()?.execute(
            "INSERT INTO terminal_runs(
               id,project_id,slot_kind,agent_role,free_form_slot_id,free_form_label,
               origin,execution_mode,backend,model,
               pulsation_id,job_id,agent_run_id,llm_execution_id,status,created_at,updated_at,
               slot_id,parent_run_id,continuation_reason,requested_model_profile,model_source
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,
                      'queued',?15,?15,?16,?17,?18,?19,?20)",
            params![
                id,
                request.project_id,
                slot_kind,
                agent_role.map(AgentRole::as_str),
                free_form_slot_id,
                free_form_label,
                request.origin.as_str(),
                request.execution_mode.as_str(),
                request.backend.map(CliBackend::as_str),
                request.model,
                request.pulsation_id,
                request.job_id,
                request.agent_run_id,
                request.llm_execution_id,
                now,
                slot_id,
                request.parent_run_id,
                continuation_reason,
                slot.model_profile,
                slot.model_source.as_str(),
            ],
        )?;
        self.get_run(&id)
    }

    pub fn get_run(&self, run_id: &str) -> Result<TerminalRun, TerminalError> {
        self.connection()?.query_row(
            "SELECT id,project_id,agent_role,origin,execution_mode,backend,model,native_session_id,
                    pulsation_id,job_id,agent_run_id,llm_execution_id,status,exit_code,error_code,
                    error_message,created_at,started_at,finished_at,updated_at,deadline_at,
                    termination_reason,output_bytes,event_count,workspace_snapshot_hash,
                    launch_identity_sha256,detected_cli_version,adapter_contract_version,
                    slot_kind,free_form_slot_id,slot_id,parent_run_id,continuation_reason,
                    session_fingerprint,requested_model_profile,model_source,observed_model
             FROM terminal_runs WHERE id=?1",
            [required(run_id,"runId")?],
            map_run,
        ).optional()?.ok_or_else(|| TerminalError::NotFound("execução".into()))
    }

    pub fn list_runs(
        &self,
        project_id: &str,
        role: AgentRole,
        limit: usize,
    ) -> Result<Vec<TerminalRun>, TerminalError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id,project_id,agent_role,origin,execution_mode,backend,model,native_session_id,
                    pulsation_id,job_id,agent_run_id,llm_execution_id,status,exit_code,error_code,
                    error_message,created_at,started_at,finished_at,updated_at,deadline_at,
                    termination_reason,output_bytes,event_count,workspace_snapshot_hash,
                    launch_identity_sha256,detected_cli_version,adapter_contract_version,
                    slot_kind,free_form_slot_id,slot_id,parent_run_id,continuation_reason,
                    session_fingerprint,requested_model_profile,model_source,observed_model
             FROM terminal_runs WHERE project_id=?1 AND agent_role=?2
             ORDER BY created_at DESC LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![
                required(project_id, "projectId")?,
                role.as_str(),
                limit.clamp(1, 200)
            ],
            map_run,
        )?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(TerminalError::from)
    }

    pub fn list_slot_runs(
        &self,
        project_id: &str,
        slot: &SessionSlot,
        limit: usize,
    ) -> Result<Vec<TerminalRun>, TerminalError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id,project_id,agent_role,origin,execution_mode,backend,model,native_session_id,
                    pulsation_id,job_id,agent_run_id,llm_execution_id,status,exit_code,error_code,
                    error_message,created_at,started_at,finished_at,updated_at,deadline_at,
                    termination_reason,output_bytes,event_count,workspace_snapshot_hash,
                    launch_identity_sha256,detected_cli_version,adapter_contract_version,
                    slot_kind,free_form_slot_id,slot_id,parent_run_id,continuation_reason,
                    session_fingerprint,requested_model_profile,model_source,observed_model
             FROM terminal_runs WHERE project_id=?1 AND slot_id=?2
             ORDER BY created_at DESC LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![
                required(project_id, "projectId")?,
                slot.persisted_id(project_id),
                limit.clamp(1, 200)
            ],
            map_run,
        )?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(TerminalError::from)
    }

    pub fn set_run_status(
        &self,
        run_id: &str,
        status: TerminalRunStatus,
        exit_code: Option<i32>,
        error_code: Option<&str>,
        error_message: Option<&str>,
    ) -> Result<TerminalRun, TerminalError> {
        let run_id = required(run_id, "runId")?;
        let current = self.get_run(run_id)?;
        if current.status == status {
            return Ok(current);
        }
        if !current.status.can_transition_to(status) {
            return Err(TerminalError::InvalidTransition {
                from: current.status.as_str().into(),
                to: status.as_str().into(),
            });
        }
        let error_message = error_message
            .map(|message| redact_secrets(&sanitize_terminal_text(message.as_bytes())))
            .transpose()
            .map_err(TerminalError::Redaction)?
            .map(|redacted| redacted.text);
        let now = Utc::now().to_rfc3339();
        let starting = matches!(
            status,
            TerminalRunStatus::Starting | TerminalRunStatus::Running
        );
        let terminal = matches!(
            status,
            TerminalRunStatus::Completed
                | TerminalRunStatus::Failed
                | TerminalRunStatus::Cancelled
                | TerminalRunStatus::Interrupted
        );
        let termination_reason = terminal.then(|| match status {
            TerminalRunStatus::Completed => "exited",
            TerminalRunStatus::Cancelled => {
                if error_code == Some("pipeline_cancelled") {
                    "pipeline_cancelled"
                } else {
                    "user_cancelled"
                }
            }
            TerminalRunStatus::Interrupted => "interrupted",
            TerminalRunStatus::Failed => match error_code {
                Some("cli_timeout") => "timeout",
                Some("cli_output_limit") => "output_limit",
                Some("cli_persistence_failed") => "persistence_failed",
                Some("cli_io_failed") => "io_failed",
                _ => "exited",
            },
            _ => unreachable!("only terminal statuses produce a termination reason"),
        });
        let db = self.connection()?;
        let transaction = db.unchecked_transaction()?;
        let changed = transaction.execute(
            "UPDATE terminal_runs SET status=?1,exit_code=?2,error_code=?3,error_message=?4,
               started_at=CASE WHEN ?5 THEN COALESCE(started_at,?6) ELSE started_at END,
               finished_at=CASE WHEN ?7 THEN ?6 ELSE finished_at END,updated_at=?6,
               termination_reason=COALESCE(?8,termination_reason)
              WHERE id=?9 AND status=?10",
            params![
                status.as_str(),
                exit_code,
                error_code,
                error_message.as_deref(),
                starting,
                now,
                terminal,
                termination_reason,
                run_id,
                current.status.as_str()
            ],
        )?;
        if changed > 0 && terminal && status != TerminalRunStatus::Completed {
            transaction.execute(
                "UPDATE token_usage_records SET quality='partial',updated_at=?1
                 WHERE source_kind='terminal_run' AND source_id=?2 AND quality='complete'",
                params![now, run_id],
            )?;
        }
        transaction.commit()?;
        if changed == 0 {
            let latest = self.get_run(run_id)?;
            return if latest.status == status {
                Ok(latest)
            } else {
                Err(TerminalError::Conflict)
            };
        }
        self.get_run(run_id)
    }

    pub fn set_run_deadline(
        &self,
        run_id: &str,
        deadline_at: &str,
    ) -> Result<TerminalRun, TerminalError> {
        let now = Utc::now().to_rfc3339();
        let changed = self.connection()?.execute(
            "UPDATE terminal_runs SET deadline_at=?1,updated_at=?2
             WHERE id=?3 AND status IN ('queued','starting','running')",
            params![deadline_at, now, required(run_id, "runId")?],
        )?;
        if changed == 0 {
            return Err(TerminalError::Conflict);
        }
        self.get_run(run_id)
    }

    pub fn set_native_session_id(
        &self,
        run_id: &str,
        session_id: &str,
    ) -> Result<TerminalRun, TerminalError> {
        let session_id = required(session_id, "sessionId")?;
        if session_id.len() > 256 {
            return Err(TerminalError::Validation(
                "identificador de sessão CLI excede o limite".into(),
            ));
        }
        let changed = self.connection()?.execute(
            "UPDATE terminal_runs
             SET native_session_id=?1,updated_at=?2
             WHERE id=?3 AND (native_session_id IS NULL OR native_session_id=?1)",
            params![
                session_id,
                Utc::now().to_rfc3339(),
                required(run_id, "runId")?
            ],
        )?;
        if changed == 0 {
            let current = self.get_run(run_id)?;
            return if current.native_session_id.as_deref() == Some(session_id) {
                Ok(current)
            } else {
                Err(TerminalError::Conflict)
            };
        }
        self.get_run(run_id)
    }

    pub fn set_observed_model(
        &self,
        run_id: &str,
        model: &str,
    ) -> Result<TerminalRun, TerminalError> {
        let model = required(model, "observedModel")?;
        if model.len() > MAX_MODEL_CHARS || model.chars().any(char::is_control) {
            return Err(TerminalError::Validation(
                "observed model ID is invalid".into(),
            ));
        }
        self.connection()?.execute(
            "UPDATE terminal_runs SET observed_model=?1,updated_at=?2 WHERE id=?3",
            params![model, Utc::now().to_rfc3339(), required(run_id, "runId")?],
        )?;
        self.get_run(run_id)
    }

    pub fn set_workspace_snapshot_hash(
        &self,
        run_id: &str,
        manifest_hash: &str,
    ) -> Result<TerminalRun, TerminalError> {
        let changed = self.connection()?.execute(
            "UPDATE terminal_runs
             SET workspace_snapshot_hash=?1,updated_at=?2
             WHERE id=?3 AND status IN ('queued','starting')
               AND (workspace_snapshot_hash IS NULL OR workspace_snapshot_hash=?1)",
            params![
                required(manifest_hash, "manifestHash")?,
                Utc::now().to_rfc3339(),
                required(run_id, "runId")?
            ],
        )?;
        if changed == 0 {
            return Err(TerminalError::Conflict);
        }
        self.get_run(run_id)
    }

    pub fn set_session_fingerprint(
        &self,
        run_id: &str,
        fingerprint: &str,
    ) -> Result<TerminalRun, TerminalError> {
        let changed = self.connection()?.execute(
            "UPDATE terminal_runs SET session_fingerprint=?1,updated_at=?2
             WHERE id=?3 AND status IN ('queued','starting')
               AND (session_fingerprint IS NULL OR session_fingerprint=?1)",
            params![
                required(fingerprint, "sessionFingerprint")?,
                Utc::now().to_rfc3339(),
                required(run_id, "runId")?
            ],
        )?;
        if changed == 0 {
            return Err(TerminalError::Conflict);
        }
        self.get_run(run_id)
    }

    pub fn set_launch_identity(
        &self,
        run_id: &str,
        identity_sha256: &str,
    ) -> Result<TerminalRun, TerminalError> {
        let changed = self.connection()?.execute(
            "UPDATE terminal_runs
             SET launch_identity_sha256=?1,updated_at=?2
             WHERE id=?3 AND status IN ('queued','starting')
               AND (launch_identity_sha256 IS NULL OR launch_identity_sha256=?1)",
            params![
                required(identity_sha256, "launchIdentity")?,
                Utc::now().to_rfc3339(),
                required(run_id, "runId")?
            ],
        )?;
        if changed == 0 {
            return Err(TerminalError::Conflict);
        }
        self.get_run(run_id)
    }

    pub fn set_adapter_metadata(
        &self,
        run_id: &str,
        detected_version: Option<&str>,
    ) -> Result<TerminalRun, TerminalError> {
        let changed = self.connection()?.execute(
            "UPDATE terminal_runs
             SET detected_cli_version=?1,adapter_contract_version=1,updated_at=?2
             WHERE id=?3 AND status='queued'",
            params![
                detected_version,
                Utc::now().to_rfc3339(),
                required(run_id, "runId")?
            ],
        )?;
        if changed == 0 {
            return Err(TerminalError::Conflict);
        }
        self.get_run(run_id)
    }

    pub fn append_event(
        &self,
        run_id: &str,
        sequence: u64,
        kind: TerminalEventKind,
        stream: Option<&str>,
        content: &str,
    ) -> Result<TerminalEvent, TerminalError> {
        if sequence == 0 {
            return Err(TerminalError::Validation(
                "sequence deve ser maior que zero".into(),
            ));
        }
        if !matches!(stream, None | Some("stdout") | Some("stderr")) {
            return Err(TerminalError::Validation("stream inválido".into()));
        }
        let sanitized = sanitize_terminal_text(content.as_bytes());
        let redacted = redact_secrets(&sanitized).map_err(TerminalError::Redaction)?;
        let event = TerminalEvent {
            id: Uuid::new_v4().to_string(),
            run_id: required(run_id, "runId")?.to_string(),
            sequence,
            kind,
            stream: stream.map(str::to_owned),
            content: redacted.text,
            redaction_count: redacted.count,
            redaction_categories: redacted.categories,
            created_at: Utc::now().to_rfc3339(),
        };
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO terminal_events(
               id,run_id,sequence,kind,stream,content,created_at,
               redaction_count,redaction_categories_json
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                &event.id,
                &event.run_id,
                event.sequence,
                event.kind.as_str(),
                &event.stream,
                &event.content,
                &event.created_at,
                event.redaction_count,
                serde_json::to_string(&event.redaction_categories)?
            ],
        )?;
        transaction.execute(
            "UPDATE terminal_runs
             SET event_count=event_count+1,output_bytes=output_bytes+?1,updated_at=?2
             WHERE id=?3",
            params![event.content.len(), event.created_at, event.run_id],
        )?;
        transaction.commit()?;
        Ok(event)
    }

    pub fn next_event_sequence(&self, run_id: &str) -> Result<u64, TerminalError> {
        self.connection()?
            .query_row(
                "SELECT COALESCE(MAX(sequence),0)+1 FROM terminal_events WHERE run_id=?1",
                [required(run_id, "runId")?],
                |row| row.get(0),
            )
            .map_err(TerminalError::from)
    }

    pub fn list_events(
        &self,
        run_id: &str,
        after_sequence: u64,
        limit: usize,
    ) -> Result<Vec<TerminalEvent>, TerminalError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id,run_id,sequence,kind,stream,content,created_at,
                    redaction_count,redaction_categories_json FROM terminal_events
             WHERE run_id=?1 AND sequence>?2 ORDER BY sequence LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![
                required(run_id, "runId")?,
                after_sequence,
                limit.clamp(1, 500)
            ],
            |row| {
                let kind: String = row.get(3)?;
                let categories: String = row.get(8)?;
                Ok(TerminalEvent {
                    id: row.get(0)?,
                    run_id: row.get(1)?,
                    sequence: row.get(2)?,
                    kind: TerminalEventKind::parse(&kind).ok_or_else(|| invalid_column(3, kind))?,
                    stream: row.get(4)?,
                    content: row.get(5)?,
                    redaction_count: row.get(7)?,
                    redaction_categories: serde_json::from_str(&categories).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            8,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?,
                    created_at: row.get(6)?,
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(TerminalError::from)
    }

    pub fn clear_history(
        &self,
        project_id: &str,
        role: Option<AgentRole>,
    ) -> Result<usize, TerminalError> {
        let connection = self.connection()?;
        let active: bool = if let Some(role) = role {
            connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM terminal_runs WHERE project_id=?1 AND agent_role=?2
                 AND status IN ('queued','starting','running','awaiting_fallback'))",
                params![required(project_id, "projectId")?, role.as_str()],
                |row| row.get(0),
            )?
        } else {
            connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM terminal_runs WHERE project_id=?1
                 AND status IN ('queued','starting','running','awaiting_fallback'))",
                [required(project_id, "projectId")?],
                |row| row.get(0),
            )?
        };
        if active {
            return Err(TerminalError::Validation(
                "não é possível limpar o histórico enquanto há execução ou fallback ativo".into(),
            ));
        }
        let affected = if let Some(role) = role {
            connection.execute(
                "DELETE FROM terminal_runs WHERE project_id=?1 AND agent_role=?2",
                params![required(project_id, "projectId")?, role.as_str()],
            )?
        } else {
            connection.execute(
                "DELETE FROM terminal_runs WHERE project_id=?1",
                [required(project_id, "projectId")?],
            )?
        };
        Ok(affected)
    }

    pub fn clear_slot_history(
        &self,
        project_id: &str,
        slot_id: &str,
    ) -> Result<usize, TerminalError> {
        let project_id = required(project_id, "projectId")?;
        let slot_id = required(slot_id, "slotId")?;
        let connection = self.connection()?;
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM terminal_slots
             WHERE id=?1 AND project_id=?2 AND kind='free_form')",
            params![slot_id, project_id],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(TerminalError::NotFound("agente livre".into()));
        }
        let active: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM terminal_runs
             WHERE project_id=?1 AND slot_id=?2
               AND status IN ('queued','starting','running','awaiting_fallback'))",
            params![project_id, slot_id],
            |row| row.get(0),
        )?;
        if active {
            return Err(TerminalError::Validation(
                "não é possível limpar o histórico enquanto o agente livre está ativo".into(),
            ));
        }
        connection
            .execute(
                "DELETE FROM terminal_runs WHERE project_id=?1 AND slot_id=?2",
                params![project_id, slot_id],
            )
            .map_err(TerminalError::from)
    }

    pub fn recover_interrupted_runs(&self) -> Result<usize, TerminalError> {
        let now = Utc::now().to_rfc3339();
        let db = self.connection()?;
        let transaction = db.unchecked_transaction()?;
        transaction.execute(
            "UPDATE token_usage_records SET quality='partial',updated_at=?1
             WHERE source_kind='terminal_run' AND quality='complete' AND source_id IN
             (SELECT id FROM terminal_runs WHERE status IN ('queued','starting','running'))",
            [&now],
        )?;
        let changed = transaction.execute(
            "UPDATE terminal_runs SET status='interrupted',error_code='app_restart',
               error_message='Execução interrompida pelo encerramento do aplicativo.',finished_at=?1,updated_at=?1
             WHERE status IN ('queued','starting','running')",
            [now],
        )?;
        transaction.commit()?;
        Ok(changed)
    }

    pub fn has_active_manual_run(
        &self,
        project_id: &str,
        role: AgentRole,
    ) -> Result<bool, TerminalError> {
        self.connection()?
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM terminal_runs WHERE project_id=?1 AND agent_role=?2
             AND origin='manual' AND status IN ('queued','starting','running'))",
                params![required(project_id, "projectId")?, role.as_str()],
                |row| row.get(0),
            )
            .map_err(TerminalError::from)
    }

    pub fn export_run_history(
        &self,
        project_id: &str,
        run_id: &str,
    ) -> Result<PathBuf, TerminalError> {
        let project_id = required(project_id, "projectId")?;
        let run = self.get_run(run_id)?;
        if run.project_id != project_id {
            return Err(TerminalError::Validation(
                "a execução pertence a outro projeto".into(),
            ));
        }
        let mut events = Vec::new();
        let mut after_sequence = 0;
        loop {
            let page = self.list_events(run_id, after_sequence, 500)?;
            let Some(last) = page.last() else {
                break;
            };
            after_sequence = last.sequence;
            events.extend(page);
            if events.len() >= MAX_RUN_EVENTS {
                break;
            }
        }
        let workspace = self.project_workspace(project_id)?;
        let directory = workspace.join(".hard-vibe-code").join("terminal-exports");
        fs::create_dir_all(&directory)?;
        let file_name = format!("terminal-{}.json", required(run_id, "runId")?);
        let destination = directory.join(file_name);
        let payload = serde_json::to_vec_pretty(&serde_json::json!({
            "schemaVersion": 1,
            "redacted": true,
            "run": run,
            "events": events,
        }))?;
        let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
        std::io::Write::write_all(&mut temporary, &payload)?;
        temporary
            .persist(&destination)
            .map_err(|error| TerminalError::Io(error.error))?;
        Ok(destination)
    }

    pub fn has_active_slot_run(
        &self,
        project_id: &str,
        slot: &SessionSlot,
    ) -> Result<bool, TerminalError> {
        self.connection()?
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM terminal_runs
                 WHERE project_id=?1 AND slot_id=?2
                   AND status IN ('queued','starting','running'))",
                params![
                    required(project_id, "projectId")?,
                    slot.persisted_id(project_id)
                ],
                |row| row.get(0),
            )
            .map_err(TerminalError::from)
    }

    pub fn has_active_automatic_run(
        &self,
        project_id: &str,
        role: AgentRole,
    ) -> Result<bool, TerminalError> {
        let descriptor = canonical_agent(role);
        self.connection()?
            .query_row(
                "SELECT EXISTS(
               SELECT 1 FROM pulsations p JOIN jobs j ON j.pulsation_id=p.id
               WHERE p.project_id=?1 AND j.status='running' AND j.cancellation_reason IS NULL
               AND (j.kind=?2 OR (?3='Learning' AND j.kind='learning_report'))
             )",
                params![
                    required(project_id, "projectId")?,
                    format!("run_stage::{}", descriptor.stage),
                    descriptor.stage
                ],
                |row| row.get(0),
            )
            .map_err(TerminalError::from)
    }

    pub fn has_api_override(
        &self,
        pulsation_id: &str,
        role: AgentRole,
    ) -> Result<bool, TerminalError> {
        self.connection()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM pulse_execution_overrides WHERE pulsation_id=?1 AND agent_role=?2 AND execution_mode='api_direct')",
            params![required(pulsation_id,"pulsationId")?,role.as_str()],
            |row|row.get(0),
        ).map_err(TerminalError::from)
    }
}

fn map_config(row: &rusqlite::Row<'_>) -> rusqlite::Result<CliAgentConfig> {
    let role: String = row.get(1)?;
    let mode: String = row.get(2)?;
    let backend: String = row.get(3)?;
    let thinking: String = row.get(5)?;
    let args: String = row.get(7)?;
    let model_source: String = row.get(13)?;
    Ok(CliAgentConfig {
        project_id: row.get(0)?,
        agent_role: AgentRole::parse(&role).ok_or_else(|| invalid_column(1, role))?,
        execution_mode: AgentExecutionMode::parse(&mode).ok_or_else(|| invalid_column(2, mode))?,
        backend: CliBackend::parse(&backend).ok_or_else(|| invalid_column(3, backend))?,
        model: row.get(4)?,
        model_profile: row.get(12)?,
        model_source: CliModelSelectionSource::parse(&model_source)
            .ok_or_else(|| invalid_column(13, model_source))?,
        thinking_level: ThinkingLevel::parse(&thinking)
            .ok_or_else(|| invalid_column(5, thinking))?,
        custom_executable: row.get(6)?,
        custom_args: serde_json::from_str(&args).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                7,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        custom_risk_ack: row.get(8)?,
        version: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

fn map_terminal_slot(row: &rusqlite::Row<'_>) -> rusqlite::Result<TerminalSlot> {
    let kind: String = row.get(2)?;
    let role: Option<String> = row.get(3)?;
    let mode: String = row.get(5)?;
    let backend: String = row.get(6)?;
    let thinking: String = row.get(8)?;
    let policy: String = row.get(9)?;
    let args: String = row.get(11)?;
    let model_source: String = row.get(19)?;
    let role = role
        .map(|value| AgentRole::parse(&value).ok_or_else(|| invalid_column(3, value)))
        .transpose()?;
    if (kind == "canonical") != role.is_some()
        || !matches!(kind.as_str(), "canonical" | "free_form")
    {
        return Err(invalid_column(2, kind));
    }
    Ok(TerminalSlot {
        id: row.get(0)?,
        project_id: row.get(1)?,
        kind,
        agent_role: role,
        label: row.get(4)?,
        execution_mode: AgentExecutionMode::parse(&mode).ok_or_else(|| invalid_column(5, mode))?,
        backend: CliBackend::parse(&backend).ok_or_else(|| invalid_column(6, backend))?,
        model: row.get(7)?,
        model_profile: row.get(18)?,
        model_source: CliModelSelectionSource::parse(&model_source)
            .ok_or_else(|| invalid_column(19, model_source))?,
        thinking_level: ThinkingLevel::parse(&thinking)
            .ok_or_else(|| invalid_column(8, thinking))?,
        session_policy: SessionPolicy::parse(&policy).ok_or_else(|| invalid_column(9, policy))?,
        custom_executable: row.get(10)?,
        custom_args: serde_json::from_str(&args).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                11,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        custom_risk_ack: row.get(12)?,
        position: row.get(13)?,
        archived: row.get(14)?,
        version: row.get(15)?,
        created_at: row.get(16)?,
        updated_at: row.get(17)?,
    })
}

fn map_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<TerminalRun> {
    let role: Option<String> = row.get(2)?;
    let origin: String = row.get(3)?;
    let mode: String = row.get(4)?;
    let backend: Option<String> = row.get(5)?;
    let status: String = row.get(12)?;
    let slot_kind: String = row.get(28)?;
    let free_form_slot_id: Option<String> = row.get(29)?;
    let slot_id: Option<String> = row.get(30)?;
    let model_source: String = row.get(35)?;
    let project_id: String = row.get(1)?;
    let agent_role = role
        .map(|value| AgentRole::parse(&value).ok_or_else(|| invalid_column(2, value)))
        .transpose()?;
    let slot = match (slot_kind.as_str(), agent_role, free_form_slot_id) {
        ("canonical", Some(agent_role), None) => SessionSlot::Canonical { agent_role },
        ("free_form", None, Some(id)) => SessionSlot::FreeForm { id },
        _ => return Err(invalid_column(28, slot_kind)),
    };
    Ok(TerminalRun {
        id: row.get(0)?,
        slot_id: slot_id.unwrap_or_else(|| slot.persisted_id(&project_id)),
        project_id,
        agent_role,
        slot,
        origin: ExecutionOrigin::parse(&origin).ok_or_else(|| invalid_column(3, origin))?,
        execution_mode: AgentExecutionMode::parse(&mode).ok_or_else(|| invalid_column(4, mode))?,
        backend: backend
            .map(|value| CliBackend::parse(&value).ok_or_else(|| invalid_column(5, value)))
            .transpose()?,
        model: row.get(6)?,
        requested_model_profile: row.get(34)?,
        model_source: CliModelSelectionSource::parse(&model_source)
            .ok_or_else(|| invalid_column(35, model_source))?,
        observed_model: row.get(36)?,
        native_session_id: row.get(7)?,
        pulsation_id: row.get(8)?,
        job_id: row.get(9)?,
        agent_run_id: row.get(10)?,
        llm_execution_id: row.get(11)?,
        status: TerminalRunStatus::parse(&status).ok_or_else(|| invalid_column(12, status))?,
        exit_code: row.get(13)?,
        error_code: row.get(14)?,
        error_message: row.get(15)?,
        created_at: row.get(16)?,
        started_at: row.get(17)?,
        finished_at: row.get(18)?,
        updated_at: row.get(19)?,
        deadline_at: row.get(20)?,
        termination_reason: row.get(21)?,
        output_bytes: row.get(22)?,
        event_count: row.get(23)?,
        workspace_snapshot_hash: row.get(24)?,
        launch_identity_sha256: row.get(25)?,
        detected_cli_version: row.get(26)?,
        adapter_contract_version: row.get(27)?,
        parent_run_id: row.get(31)?,
        continuation_reason: row.get(32)?,
        session_fingerprint: row.get(33)?,
    })
}

fn invalid_column(index: usize, value: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("valor persistido inválido: {value}"),
        )),
    )
}

fn required<'a>(value: &'a str, field: &str) -> Result<&'a str, TerminalError> {
    let value = value.trim();
    if value.is_empty() {
        Err(TerminalError::Validation(format!("{field} é obrigatório")))
    } else {
        Ok(value)
    }
}

fn canonical_slot_id(project_id: &str, role: AgentRole) -> String {
    format!("canonical:{project_id}:{}", role.as_str())
}

fn validate_slot_label(label: &str) -> Result<&str, TerminalError> {
    let label = required(label, "label")?;
    if label.chars().count() > 80 || label.chars().any(char::is_control) {
        return Err(TerminalError::Validation(
            "o nome do agente livre deve ter até 80 caracteres visíveis".into(),
        ));
    }
    Ok(label)
}

fn normalize_optional(
    value: Option<String>,
    max: usize,
    field: &str,
) -> Result<Option<String>, TerminalError> {
    let value = value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if value
        .as_ref()
        .is_some_and(|value| value.chars().count() > max)
    {
        return Err(TerminalError::Validation(format!(
            "{field} excede {max} caracteres"
        )));
    }
    Ok(value)
}

fn normalize_model_profile(value: Option<String>) -> Result<Option<String>, TerminalError> {
    let profile = normalize_optional(value, 100, "modelProfile")?;
    if profile.as_ref().is_some_and(|value| {
        !value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | ':' | '/')
        })
    }) {
        return Err(TerminalError::Validation(
            "modelProfile contains unsupported characters".into(),
        ));
    }
    Ok(profile)
}

fn normalize_model_id(value: Option<String>) -> Result<Option<String>, TerminalError> {
    let model = normalize_optional(value, MAX_MODEL_CHARS, "model")?;
    if model.as_ref().is_some_and(|value| {
        !value.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(character, '-' | '_' | '.' | ':' | '/' | '@' | '+')
        })
    }) {
        return Err(TerminalError::Validation(
            "model contains unsupported characters".into(),
        ));
    }
    Ok(model)
}

fn validate_model_selection(
    backend: CliBackend,
    model: Option<&str>,
    profile: Option<&str>,
    source: CliModelSelectionSource,
) -> Result<(), TerminalError> {
    if source == CliModelSelectionSource::Default && model.is_some() {
        return Err(TerminalError::Validation(
            "default model selection cannot include a model ID".into(),
        ));
    }
    if source != CliModelSelectionSource::Default && model.is_none() {
        return Err(TerminalError::Validation(
            "catalog and manual model selections require a model ID".into(),
        ));
    }
    if matches!(backend, CliBackend::Gemini | CliBackend::Custom) && profile.is_some() {
        return Err(TerminalError::Validation(format!(
            "{} does not expose a verified model profile contract",
            backend.label()
        )));
    }
    if source == CliModelSelectionSource::Manual && profile.is_some() {
        return Err(TerminalError::Validation(
            "unverified manual models cannot include a model profile".into(),
        ));
    }
    Ok(())
}

fn validate_custom(request: &UpdateCliAgentConfigRequest) -> Result<(), TerminalError> {
    validate_custom_parts(
        request.execution_mode,
        request.backend,
        request.custom_executable.as_deref(),
        &request.custom_args,
        request.custom_risk_ack,
    )
}

fn validate_custom_parts(
    execution_mode: AgentExecutionMode,
    backend: CliBackend,
    custom_executable: Option<&str>,
    custom_args: &[String],
    custom_risk_ack: bool,
) -> Result<(), TerminalError> {
    if custom_args.len() > MAX_CUSTOM_ARGS {
        return Err(TerminalError::Validation(format!(
            "Custom aceita no máximo {MAX_CUSTOM_ARGS} argumentos"
        )));
    }
    if custom_args
        .iter()
        .any(|arg| arg.contains('\0') || arg.chars().count() > MAX_CUSTOM_ARG_CHARS)
    {
        return Err(TerminalError::Validation(
            "argumento Custom inválido ou excessivamente longo".into(),
        ));
    }
    if backend != CliBackend::Custom {
        return Ok(());
    }
    let Some(executable) = custom_executable
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return if execution_mode == AgentExecutionMode::CliProcess {
            Err(TerminalError::Validation(
                "Custom exige caminho absoluto do executável".into(),
            ))
        } else {
            Ok(())
        };
    };
    if !Path::new(executable).is_absolute() {
        return Err(TerminalError::Validation(
            "Custom exige caminho absoluto do executável".into(),
        ));
    }
    if Path::new(executable)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "cmd" | "bat" | "ps1"))
    {
        return Err(TerminalError::Validation(
            "Custom não aceita scripts de shell; selecione um executável direto".into(),
        ));
    }
    if execution_mode == AgentExecutionMode::CliProcess && !custom_risk_ack {
        return Err(TerminalError::Validation(
            "confirme o risco do executável Custom antes de ativá-lo".into(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CliAvailabilityState {
    Available,
    Missing,
    Unsupported,
    AuthRequired,
    Probing,
    UnsupportedVersion,
    CapabilityMissing,
    ProbeFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CliOutputFormat {
    Text,
    Json,
    JsonLines,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CliCapabilities {
    pub output_format: CliOutputFormat,
    pub structured_output: bool,
    pub native_resume: bool,
    pub thinking_levels: Vec<ThinkingLevel>,
    pub auth_probe: bool,
    pub read_only: bool,
    pub model_catalog: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliAvailability {
    pub backend: CliBackend,
    pub state: CliAvailabilityState,
    pub executable_path: Option<PathBuf>,
    pub version: Option<String>,
    pub diagnostic: Option<String>,
    pub install_url: &'static str,
    pub install_command: &'static str,
    pub capabilities: CliCapabilities,
}

pub fn discover_clis() -> Vec<CliAvailability> {
    let path_entries = cli_search_paths();
    let extensions = executable_extensions();
    CliBackend::ALL
        .into_iter()
        .map(|backend| {
            let executable_path = if backend == CliBackend::Custom {
                None
            } else {
                find_executable(backend.as_str(), &path_entries, &extensions)
            };
            let (install_url, install_command) = install_guidance(backend);
            CliAvailability {
                backend,
                state: if executable_path.is_some() {
                    CliAvailabilityState::Available
                } else {
                    CliAvailabilityState::Missing
                },
                executable_path,
                version: None,
                diagnostic: None,
                install_url,
                install_command,
                capabilities: adapters::capabilities(backend, None),
            }
        })
        .collect()
}

pub async fn discover_clis_with_probes() -> Vec<CliAvailability> {
    let mut discovered = stream::iter(discover_clis())
        .map(probe_cli_availability)
        .buffer_unordered(4)
        .collect::<Vec<_>>()
        .await;
    discovered.sort_by_key(|availability| match availability.backend {
        CliBackend::Codex => 0,
        CliBackend::Claude => 1,
        CliBackend::Gemini => 2,
        CliBackend::Opencode => 3,
        CliBackend::Custom => 4,
    });
    discovered
}

async fn probe_cli_availability(mut availability: CliAvailability) -> CliAvailability {
    let Some(executable) = availability.executable_path.clone() else {
        return availability;
    };
    match tokio::time::timeout(
        CLI_VERSION_PROBE_TIMEOUT,
        probe_version(availability.backend, &executable),
    )
    .await
    {
        Ok(Ok(version)) if !version.is_empty() => availability.version = Some(version),
        Ok(Ok(_)) => {
            availability.state = CliAvailabilityState::ProbeFailed;
            availability.diagnostic = Some("A CLI retornou uma versão vazia.".into());
            return availability;
        }
        Ok(Err(error)) => {
            availability.state = CliAvailabilityState::ProbeFailed;
            availability.diagnostic = Some(format!("Não foi possível executar --version: {error}"));
            return availability;
        }
        Err(_) => {
            availability.state = CliAvailabilityState::ProbeFailed;
            availability.diagnostic = Some(format!(
                "A verificação de versão excedeu {} segundos.",
                CLI_VERSION_PROBE_TIMEOUT.as_secs()
            ));
            return availability;
        }
    }
    availability.capabilities =
        adapters::capabilities(availability.backend, availability.version.as_deref());
    match adapters::version_compatibility(availability.backend, availability.version.as_deref()) {
        adapters::CliVersionCompatibility::Supported => {}
        adapters::CliVersionCompatibility::Unsupported => {
            availability.state = CliAvailabilityState::UnsupportedVersion;
            availability.diagnostic = Some(format!(
                "A versão detectada do {} não passou pelo contrato do adapter.",
                availability.backend.label()
            ));
            return availability;
        }
        adapters::CliVersionCompatibility::UnknownNewer => {
            availability.state = CliAvailabilityState::CapabilityMissing;
            availability.diagnostic = Some(format!(
                "A versão detectada do {} é mais nova que o contrato validado; atualize o adapter antes de executar.",
                availability.backend.label()
            ));
            return availability;
        }
    }
    if availability.capabilities.auth_probe {
        match tokio::time::timeout(
            CLI_AUTH_PROBE_TIMEOUT,
            probe_auth(availability.backend, &executable),
        )
        .await
        {
            Ok(Ok(true)) => {}
            Ok(Ok(false)) => {
                availability.state = CliAvailabilityState::AuthRequired;
                availability.diagnostic = Some(format!(
                    "{} instalado, mas sem login confirmado.",
                    availability.backend.label()
                ));
            }
            Ok(Err(error)) => {
                availability.state = CliAvailabilityState::ProbeFailed;
                availability.diagnostic = Some(format!(
                    "Falha ao verificar autenticação do {}: {error}",
                    availability.backend.label()
                ));
            }
            Err(_) => {
                availability.state = CliAvailabilityState::ProbeFailed;
                availability.diagnostic = Some(format!(
                    "A verificação de autenticação excedeu {} segundos.",
                    CLI_AUTH_PROBE_TIMEOUT.as_secs()
                ));
            }
        }
    }
    availability
}

async fn probe_version(backend: CliBackend, executable: &Path) -> Result<String, std::io::Error> {
    let secured = secure_probe_spec(backend, executable, vec!["--version".into()]).await?;
    let output = run_bounded_probe(secured).await?;
    if !output.status.success() {
        let diagnostic = redact_secrets(&sanitize_terminal_text(&output.stderr))
            .map_err(std::io::Error::other)?
            .text;
        return Err(std::io::Error::other(format!(
            "exit code {:?}{}",
            output.status.code(),
            if diagnostic.trim().is_empty() {
                String::new()
            } else {
                format!(": {}", diagnostic.trim())
            }
        )));
    }
    let version_bytes = if output.stdout.is_empty() {
        &output.stderr
    } else {
        &output.stdout
    };
    let sanitized = sanitize_terminal_text(version_bytes);
    let redacted = redact_secrets(&sanitized).map_err(std::io::Error::other)?;
    Ok(redacted.text.trim().to_string())
}

async fn probe_auth(backend: CliBackend, executable: &Path) -> Result<bool, std::io::Error> {
    let Some(args) = adapters::auth_probe_args(backend) else {
        return Ok(true);
    };
    let secured = secure_probe_spec(
        backend,
        executable,
        args.iter().map(|value| (*value).to_string()).collect(),
    )
    .await?;
    Ok(run_bounded_probe(secured).await?.status.success())
}

struct ProbeOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

async fn run_bounded_probe(spec: CliProcessSpec) -> Result<ProbeOutput, std::io::Error> {
    run_bounded_probe_in(spec, None).await
}

async fn run_bounded_probe_in(
    spec: CliProcessSpec,
    current_dir: Option<&Path>,
) -> Result<ProbeOutput, std::io::Error> {
    run_bounded_probe_in_with_limit(spec, current_dir, MAX_PROBE_OUTPUT_BYTES).await
}

async fn run_bounded_probe_in_with_limit(
    spec: CliProcessSpec,
    current_dir: Option<&Path>,
    max_output_bytes: usize,
) -> Result<ProbeOutput, std::io::Error> {
    let mut command = Command::new(&spec.executable);
    command
        .args(&spec.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env_clear();
    if let Some(current_dir) = current_dir {
        command.current_dir(current_dir);
    }
    copy_safe_environment(&mut command);
    configure_hidden(&mut command);
    let mut child = command.spawn()?;
    let process_tree = ProcessTreeJob::assign(&child)?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("stdout do probe indisponível"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| std::io::Error::other("stderr do probe indisponível"))?;
    let abort = CancellationToken::new();
    let stdout_task = tokio::spawn(read_bounded_stream(
        stdout,
        abort.clone(),
        max_output_bytes,
        None,
        None,
    ));
    let stderr_task = tokio::spawn(read_bounded_stream(
        stderr,
        abort.clone(),
        max_output_bytes,
        None,
        None,
    ));
    let status = tokio::select! {
        status = child.wait() => status?,
        _ = abort.cancelled() => {
            process_tree.terminate();
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "a saída do probe excedeu o limite seguro",
            ));
        }
    };
    let stdout = stdout_task.await.map_err(|error| {
        std::io::Error::other(format!("leitor do probe interrompido: {error}"))
    })??;
    let stderr = stderr_task.await.map_err(|error| {
        std::io::Error::other(format!("leitor do probe interrompido: {error}"))
    })??;
    Ok(ProbeOutput {
        status,
        stdout,
        stderr,
    })
}

#[cfg(test)]
async fn read_bounded_probe_stream<R: AsyncRead + Unpin>(
    reader: R,
    abort: CancellationToken,
) -> Result<Vec<u8>, std::io::Error> {
    read_bounded_stream(reader, abort, MAX_PROBE_OUTPUT_BYTES, None, None).await
}

async fn read_bounded_stream<R: AsyncRead + Unpin>(
    mut reader: R,
    abort: CancellationToken,
    max_output_bytes: usize,
    watchdog: Option<ProgressWatchdog>,
    usage: Option<Arc<crate::usage::cli::CliUsageRecorder>>,
) -> Result<Vec<u8>, std::io::Error> {
    let mut output = Vec::new();
    let mut buffer = [0u8; 4096];
    let mut usage_offset = 0usize;
    loop {
        let read = match reader.read(&mut buffer).await {
            Ok(read) => read,
            Err(error) => {
                abort.cancel();
                return Err(error);
            }
        };
        if read == 0 {
            if let Some(usage) = &usage
                && let Err(error) = usage.observe_line(&output[usage_offset..])
            {
                abort.cancel();
                return Err(std::io::Error::other(error));
            }
            return Ok(output);
        }
        if let Some(ref watchdog) = watchdog {
            watchdog.record_progress();
        }
        if output.len().saturating_add(read) > max_output_bytes {
            abort.cancel();
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "a saída do probe excedeu o limite seguro",
            ));
        }
        output.extend_from_slice(&buffer[..read]);
        if let Some(usage) = &usage {
            while let Some(newline) = output[usage_offset..].iter().position(|b| *b == b'\n') {
                let end = usage_offset + newline;
                if let Err(error) = usage.observe_line(&output[usage_offset..end]) {
                    abort.cancel();
                    return Err(std::io::Error::other(error));
                }
                usage_offset = end + 1;
            }
        }
    }
}

async fn secure_probe_spec(
    backend: CliBackend,
    executable: &Path,
    args: Vec<String>,
) -> Result<CliProcessSpec, std::io::Error> {
    let executable = executable.to_path_buf();
    tokio::task::spawn_blocking(move || {
        secure_launch_spec(
            CliProcessSpec {
                executable,
                args,
                environment: Vec::new(),
                stdin_payload: Vec::new(),
                resume_session_id: None,
            },
            backend,
        )
        .map(|secured| secured.spec)
        .map_err(std::io::Error::other)
    })
    .await
    .map_err(std::io::Error::other)?
}

fn cli_search_paths() -> Vec<PathBuf> {
    cli_search_paths_from_environment(|name| env::var_os(name))
}

fn cli_search_paths_from_environment<F>(mut read_var: F) -> Vec<PathBuf>
where
    F: FnMut(&str) -> Option<std::ffi::OsString>,
{
    let mut paths = read_var("PATH")
        .map(|value| env::split_paths(&value).collect::<Vec<_>>())
        .unwrap_or_default();

    #[cfg(windows)]
    {
        // GUI applications inherit a snapshot of PATH when they start. Include the standard
        // per-user CLI locations as fallbacks so an npm/WinGet install made later is visible
        // after Refresh without trusting or invoking a shell profile.
        if let Some(app_data) = read_var("APPDATA") {
            paths.push(PathBuf::from(app_data).join("npm"));
        }
        if let Some(local_app_data) = read_var("LOCALAPPDATA") {
            paths.push(PathBuf::from(local_app_data).join("Microsoft/WinGet/Links"));
        }
        if let Some(user_profile) = read_var("USERPROFILE") {
            let user_profile = PathBuf::from(user_profile);
            paths.extend([
                user_profile.join(".local/bin"),
                user_profile.join(".cargo/bin"),
                user_profile.join(".bun/bin"),
            ]);
        }
        if let Some(prefix) = read_var("NPM_CONFIG_PREFIX") {
            paths.push(PathBuf::from(prefix));
        }
        if let Some(pnpm_home) = read_var("PNPM_HOME") {
            paths.push(PathBuf::from(pnpm_home));
        }
        if let Some(bun_install) = read_var("BUN_INSTALL") {
            paths.push(PathBuf::from(bun_install).join("bin"));
        }
        if let Some(volta_home) = read_var("VOLTA_HOME") {
            paths.push(PathBuf::from(volta_home).join("bin"));
        }
    }

    let mut seen = HashSet::new();
    paths
        .into_iter()
        .filter(|path| !path.as_os_str().is_empty())
        .filter(|path| seen.insert(path.to_string_lossy().to_ascii_lowercase()))
        .collect()
}

fn executable_extensions() -> Vec<String> {
    #[cfg(windows)]
    {
        windows_executable_extensions(env::var("PATHEXT").ok().as_deref())
    }
    #[cfg(not(windows))]
    {
        vec![String::new()]
    }
}

#[cfg(windows)]
fn windows_executable_extensions(path_ext: Option<&str>) -> Vec<String> {
    const SUPPORTED: [&str; 4] = [".exe", ".com", ".cmd", ".bat"];
    let mut extensions = Vec::new();
    for extension in path_ext
        .unwrap_or(".EXE;.COM;.CMD;.BAT")
        .split(';')
        .map(|value| value.trim().to_ascii_lowercase())
    {
        if SUPPORTED.contains(&extension.as_str()) && !extensions.contains(&extension) {
            extensions.push(extension);
        }
    }
    for extension in SUPPORTED {
        if !extensions.iter().any(|value| value == extension) {
            extensions.push(extension.to_string());
        }
    }
    extensions
}

fn native_executable_extensions() -> Vec<String> {
    #[cfg(windows)]
    {
        vec![".exe".into(), ".com".into()]
    }
    #[cfg(not(windows))]
    {
        vec![String::new()]
    }
}

fn find_executable(name: &str, paths: &[PathBuf], extensions: &[String]) -> Option<PathBuf> {
    let mut seen = HashSet::new();
    for directory in paths {
        for extension in extensions {
            let candidate = directory.join(format!("{name}{extension}"));
            let key = candidate.to_string_lossy().to_ascii_lowercase();
            if seen.insert(key) && candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn install_guidance(backend: CliBackend) -> (&'static str, &'static str) {
    match backend {
        CliBackend::Codex => (
            "https://developers.openai.com/codex/cli/",
            "npm install -g @openai/codex",
        ),
        CliBackend::Claude => (
            "https://code.claude.com/docs/en/setup",
            "npm install -g @anthropic-ai/claude-code",
        ),
        CliBackend::Gemini => (
            "https://github.com/google-gemini/gemini-cli",
            "npm install -g @google/gemini-cli",
        ),
        CliBackend::Opencode => (
            "https://dev.opencode.ai/docs/cli/",
            "npm install -g opencode-ai",
        ),
        CliBackend::Custom => ("", "Selecione um executável direto"),
    }
}

#[derive(Debug, Clone)]
pub struct CliInvocationRequest {
    pub executable: PathBuf,
    pub backend: CliBackend,
    pub model: Option<String>,
    pub model_profile: Option<String>,
    pub thinking_level: ThinkingLevel,
    pub prompt: String,
    pub schema_file: Option<PathBuf>,
    pub schema: Option<Value>,
    pub custom_args: Vec<String>,
    pub resume_session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliProcessSpec {
    pub executable: PathBuf,
    pub args: Vec<String>,
    pub environment: Vec<(String, String)>,
    pub stdin_payload: Vec<u8>,
    pub resume_session_id: Option<String>,
}

pub fn build_process_spec(request: CliInvocationRequest) -> Result<CliProcessSpec, TerminalError> {
    adapters::build_process_spec(request)
}

pub fn sanitize_terminal_text(bytes: &[u8]) -> String {
    sanitize_terminal_text_bounded(bytes, MAX_EVENT_CHARS)
}

pub fn sanitize_terminal_text_bounded(bytes: &[u8], max_chars: usize) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut output = String::with_capacity(text.len().min(max_chars));
    let mut chars = text.chars().peekable();
    let mut written = 0usize;
    while let Some(ch) = chars.next() {
        if written >= max_chars {
            output.push_str("\n[saída truncada]");
            break;
        }
        if ch == '\u{1b}' {
            if matches!(chars.peek(), Some('[')) {
                chars.next();
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            } else if matches!(chars.peek(), Some(']')) {
                chars.next();
                let mut previous = '\0';
                for next in chars.by_ref() {
                    if next == '\u{7}' || (previous == '\u{1b}' && next == '\\') {
                        break;
                    }
                    previous = next;
                }
            }
            continue;
        }
        if ch == '\n' || ch == '\r' || ch == '\t' || !ch.is_control() {
            output.push(ch);
            written += 1;
        }
    }
    output
}

pub fn normalize_cli_frame(
    backend: CliBackend,
    stream: &str,
    bytes: &[u8],
) -> (TerminalEventKind, String) {
    let normalized = adapters::normalize_frame(backend, stream, bytes);
    (normalized.kind, normalized.content)
}

fn extract_visible_text(value: &Value) -> Option<String> {
    for key in ["text", "content", "message", "output", "result"] {
        if let Some(candidate) = value.get(key) {
            if let Some(text) = candidate.as_str().filter(|text| !text.is_empty()) {
                return Some(text.to_string());
            }
            if let Some(text) = extract_visible_text(candidate) {
                return Some(text);
            }
        }
    }
    match value {
        Value::Array(items) => items.iter().find_map(extract_visible_text),
        Value::Object(values) => values.values().find_map(extract_visible_text),
        _ => None,
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTerminalSnapshot {
    pub config: CliAgentConfig,
    pub recent_runs: Vec<TerminalRun>,
    pub recent_events: Vec<TerminalEvent>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FreeFormTerminalSnapshot {
    pub slot: TerminalSlot,
    pub recent_runs: Vec<TerminalRun>,
    pub recent_events: Vec<TerminalEvent>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalsSnapshot {
    pub project_id: String,
    pub agents: Vec<AgentTerminalSnapshot>,
    pub free_form_slots: Vec<FreeFormTerminalSnapshot>,
    pub archived_free_form_slots: Vec<FreeFormTerminalSnapshot>,
    pub discovery: Vec<CliAvailability>,
}

#[derive(Debug, Clone)]
pub struct StartManualRunRequest {
    pub project_id: String,
    pub agent_role: AgentRole,
    pub prompt: String,
}

#[derive(Debug, Clone)]
pub struct StartFreeFormRunRequest {
    pub project_id: String,
    pub slot_id: String,
    pub prompt: String,
}

#[derive(Debug, Clone)]
pub struct ResumeManualRunRequest {
    pub project_id: String,
    pub source_run_id: String,
    pub prompt: String,
}

#[derive(Debug, Clone)]
pub struct CliCompletion {
    pub terminal_run_id: String,
    pub backend: CliBackend,
    pub model: String,
    pub raw_output: String,
    pub request_payload: Value,
    pub response_payload: Value,
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
    pub schema: Value,
}

#[derive(Debug, Clone)]
pub struct CompanionCliRoundOutput {
    pub envelope_json: String,
    pub detected_cli_version: Option<String>,
    pub exit_code: Option<i32>,
    pub output_bytes: usize,
    pub redaction_count: u64,
}

#[derive(Debug)]
struct CliProcessOutcome {
    success: bool,
    exit_code: Option<i32>,
    raw_output: String,
    termination: ProcessTermination,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessTermination {
    Exited,
    Cancelled,
    TimedOut(TimeoutReason),
}

#[derive(Debug, Clone, Copy)]
enum ProcessAbortReason {
    OutputLimit,
    Persistence,
}

#[derive(Debug)]
enum ProcessStreamItem {
    Frame {
        stream: &'static str,
        bytes: Vec<u8>,
    },
    ReadError {
        stream: &'static str,
        message: String,
    },
}

#[derive(Debug, Default)]
struct ProcessEventSummary {
    raw_output: String,
    last_stderr: String,
}

#[derive(Debug, Clone)]
struct CliDiscoveryCache {
    discovered_at: Instant,
    items: Vec<CliAvailability>,
}

#[derive(Debug)]
struct SafeCliWorkspace {
    directory: tempfile::TempDir,
    manifest_hash: String,
}

#[derive(Debug)]
struct SecuredLaunch {
    spec: CliProcessSpec,
    identity_sha256: String,
}

impl SafeCliWorkspace {
    fn path(&self) -> &Path {
        self.directory.path()
    }
}

#[cfg(windows)]
#[allow(clippy::permissions_set_readonly_false)]
impl Drop for SafeCliWorkspace {
    fn drop(&mut self) {
        for entry in walkdir::WalkDir::new(self.directory.path())
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
        {
            if let Ok(metadata) = entry.metadata() {
                let mut permissions = metadata.permissions();
                permissions.set_readonly(false);
                let _ = fs::set_permissions(entry.path(), permissions);
            }
        }
    }
}

pub struct TerminalService {
    repository: TerminalRepository,
    model_catalog: CliModelCatalogService,
    runtime: Handle,
    event_sink: Arc<dyn TerminalEventSink>,
    active: Mutex<HashMap<String, CancellationToken>>,
    active_changed: Notify,
    discovery_cache: Mutex<Option<CliDiscoveryCache>>,
}

impl TerminalService {
    pub fn new(
        db_path: impl Into<PathBuf>,
        runtime: Handle,
        event_sink: Arc<dyn TerminalEventSink>,
    ) -> Result<Self, TerminalError> {
        let db_path = db_path.into();
        let repository = TerminalRepository::new(db_path.clone());
        repository.recover_interrupted_runs()?;
        Ok(Self {
            repository,
            model_catalog: CliModelCatalogService::new(db_path),
            runtime,
            event_sink,
            active: Mutex::new(HashMap::new()),
            active_changed: Notify::new(),
            discovery_cache: Mutex::new(None),
        })
    }

    pub fn repository(&self) -> &TerminalRepository {
        &self.repository
    }

    async fn discovery(&self, force: bool) -> Vec<CliAvailability> {
        if !force
            && let Some(cache) = self
                .discovery_cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
        {
            return cache.items.clone();
        }
        let handle = self.runtime.clone();
        let mut items = match handle.spawn(discover_clis_with_probes()).await {
            Ok(items) => items,
            Err(_) => discover_clis(),
        };
        if cli_mode_disabled() {
            for availability in &mut items {
                if availability.backend != CliBackend::Custom {
                    availability.state = CliAvailabilityState::Unsupported;
                    availability.diagnostic =
                        Some("CLI Mode está desativado pelo kill switch local.".into());
                }
            }
        }
        *self
            .discovery_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(CliDiscoveryCache {
            discovered_at: Instant::now(),
            items: items.clone(),
        });
        items
    }

    pub async fn refresh_discovery(&self) -> Vec<CliAvailability> {
        self.discovery(true).await
    }

    pub async fn model_catalog(
        &self,
        project_id: &str,
        backend: CliBackend,
        refresh: bool,
    ) -> Result<CliModelCatalogSnapshot, TerminalError> {
        if backend == CliBackend::Custom {
            return Err(TerminalError::Validation(
                "Custom CLI cannot be used by the Companion".into(),
            ));
        }
        let workspace = self.repository.project_workspace(project_id)?;
        let discovery = self.discovery(false).await;
        self.model_catalog
            .catalog(
                project_id,
                &workspace,
                backend,
                discovery.iter().find(|item| item.backend == backend),
                refresh,
            )
            .await
    }

    pub async fn execute_companion_round(
        &self,
        request: CompanionCliRoundRequest,
        cancellation: &CancellationToken,
        deadline: Instant,
    ) -> Result<CompanionCliRoundOutput, TerminalError> {
        if request.backend == CliBackend::Custom {
            return Err(TerminalError::Validation(
                "Custom CLI is not allowed for the Companion".into(),
            ));
        }
        validate_model_selection(
            request.backend,
            request.model.as_deref(),
            request.model_profile.as_deref(),
            request.model_source,
        )?;
        validate_catalog_model_selection(
            &self.repository.connection()?,
            &request.project_id,
            request.backend,
            request.model.as_deref(),
            request.model_profile.as_deref(),
            request.model_source,
        )?;
        let availability = self
            .discovery(false)
            .await
            .into_iter()
            .find(|item| item.backend == request.backend)
            .ok_or_else(|| TerminalError::NotFound(format!("{} CLI", request.backend.label())))?;
        if availability.state != CliAvailabilityState::Available {
            return Err(match availability.state {
                CliAvailabilityState::AuthRequired => {
                    TerminalError::AuthRequired(request.backend.label().into())
                }
                CliAvailabilityState::UnsupportedVersion
                | CliAvailabilityState::CapabilityMissing => {
                    TerminalError::VersionUnsupported(request.backend.label().into())
                }
                _ => TerminalError::Process(
                    availability
                        .diagnostic
                        .unwrap_or_else(|| format!("{} is not ready", request.backend.label())),
                ),
            });
        }
        let executable = availability
            .executable_path
            .ok_or_else(|| TerminalError::NotFound(format!("{} CLI", request.backend.label())))?;
        let mut schema_guard = None;
        let schema_file = if request.backend == CliBackend::Codex {
            let mut file = tempfile::NamedTempFile::new()?;
            serde_json::to_writer(file.as_file_mut(), &request.schema)?;
            let path = file.path().to_path_buf();
            schema_guard = Some(file);
            Some(path)
        } else {
            None
        };
        let native_schema = matches!(request.backend, CliBackend::Codex | CliBackend::Claude);
        let mut prompt = request.prompt;
        if !native_schema {
            prompt.push_str(
                "\n\nReturn only one JSON object that satisfies this control envelope schema. Do not wrap it in Markdown:\n",
            );
            prompt.push_str(&serde_json::to_string(&request.schema)?);
        }
        let mut spec = build_process_spec(CliInvocationRequest {
            executable,
            backend: request.backend,
            model: request.model,
            model_profile: request.model_profile,
            thinking_level: ThinkingLevel::Default,
            prompt,
            schema_file,
            schema: native_schema.then_some(request.schema),
            custom_args: Vec::new(),
            resume_session_id: None,
        })?;
        let hardening_guard = harden_spec(request.backend, Hardening::Companion, &mut spec)?;
        let result = run_companion_process(
            spec,
            request.backend,
            cancellation,
            deadline,
            availability.version,
            self.repository.db_path.clone(),
            request.usage_context,
        )
        .await;
        drop(hardening_guard);
        drop(schema_guard);
        result
    }

    pub async fn snapshot(&self, project_id: &str) -> Result<TerminalsSnapshot, TerminalError> {
        let discovery = self.discovery(false).await;
        let codex_ready = discovery.iter().any(|item| {
            item.backend == CliBackend::Codex && item.state == CliAvailabilityState::Available
        });
        let configs = self
            .repository
            .ensure_project_configs(project_id, codex_ready)?;
        let mut agents = Vec::with_capacity(configs.len());
        for config in configs {
            let recent_runs = self
                .repository
                .list_runs(project_id, config.agent_role, 20)?;
            let recent_events = match recent_runs.first() {
                Some(run) => self.repository.list_events(&run.id, 0, 200)?,
                None => Vec::new(),
            };
            agents.push(AgentTerminalSnapshot {
                config,
                recent_runs,
                recent_events,
            });
        }
        let mut free_form_slots = Vec::new();
        let mut archived_free_form_slots = Vec::new();
        for slot in self.repository.list_free_form_slots(project_id, true)? {
            let recent_runs = self.repository.list_slot_runs(
                project_id,
                &SessionSlot::FreeForm {
                    id: slot.id.clone(),
                },
                20,
            )?;
            let recent_events = match recent_runs.first() {
                Some(run) => self.repository.list_events(&run.id, 0, 200)?,
                None => Vec::new(),
            };
            let archived = slot.archived;
            let snapshot = FreeFormTerminalSnapshot {
                slot,
                recent_runs,
                recent_events,
            };
            if archived {
                archived_free_form_slots.push(snapshot);
            } else {
                free_form_slots.push(snapshot);
            }
        }
        Ok(TerminalsSnapshot {
            project_id: project_id.to_string(),
            agents,
            free_form_slots,
            archived_free_form_slots,
            discovery,
        })
    }

    pub fn update_config(
        &self,
        request: UpdateCliAgentConfigRequest,
    ) -> Result<CliAgentConfig, TerminalError> {
        self.repository.update_config(request)
    }

    pub fn load_events(
        &self,
        run_id: &str,
        after_sequence: u64,
        limit: usize,
    ) -> Result<Vec<TerminalEvent>, TerminalError> {
        self.repository.list_events(run_id, after_sequence, limit)
    }

    pub fn clear_history(
        &self,
        project_id: &str,
        role: Option<AgentRole>,
    ) -> Result<usize, TerminalError> {
        self.repository.clear_history(project_id, role)
    }

    pub fn clear_slot_history(
        &self,
        project_id: &str,
        slot_id: &str,
    ) -> Result<usize, TerminalError> {
        self.repository.clear_slot_history(project_id, slot_id)
    }

    pub fn cancel_run(&self, run_id: &str) -> bool {
        self.active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(run_id)
            .is_some_and(|token| {
                token.cancel();
                true
            })
    }

    pub fn cancel_all(&self) {
        for token in self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
        {
            token.cancel();
        }
    }

    pub async fn wait_for_idle(&self, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let notified = self.active_changed.notified();
            if self
                .active
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
            {
                return true;
            }
            let now = tokio::time::Instant::now();
            if now >= deadline
                || tokio::time::timeout(deadline - now, notified)
                    .await
                    .is_err()
            {
                return false;
            }
        }
    }

    fn resolve_launch(
        &self,
        backend: CliBackend,
        custom_executable: Option<&str>,
    ) -> Result<(PathBuf, Option<String>, CliCapabilities), TerminalError> {
        if backend == CliBackend::Custom {
            return Ok((
                custom_executable.map(PathBuf::from).ok_or_else(|| {
                    TerminalError::Validation("configure o executável Custom".into())
                })?,
                None,
                adapters::capabilities(CliBackend::Custom, None),
            ));
        }
        let availability = self
            .discovery_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .filter(|cache| cache.discovered_at.elapsed() < CLI_DISCOVERY_CACHE_TTL)
            .map(|cache| cache.items.clone())
            .ok_or_else(|| {
                TerminalError::Validation(
                    "os diagnósticos das CLIs ainda não foram executados ou expiraram; atualize o painel antes de iniciar"
                        .into(),
                )
            })?
            .into_iter()
            .find(|item| item.backend == backend)
            .ok_or_else(|| TerminalError::NotFound(format!("{} CLI", backend.label())))?;
        if availability.state != CliAvailabilityState::Available {
            return Err(TerminalError::Process(
                availability.diagnostic.unwrap_or_else(|| {
                    format!("{} não está pronto para execução", backend.label())
                }),
            ));
        }
        Ok((
            availability
                .executable_path
                .ok_or_else(|| TerminalError::NotFound(format!("{} CLI", backend.label())))?,
            availability.version,
            availability.capabilities,
        ))
    }

    pub fn start_manual(
        self: &Arc<Self>,
        request: StartManualRunRequest,
    ) -> Result<TerminalRun, TerminalError> {
        let prompt = required(&request.prompt, "prompt")?.to_string();
        if self
            .repository
            .has_active_automatic_run(&request.project_id, request.agent_role)?
        {
            return Err(TerminalError::Validation(
                "o agente está ocupado por uma execução automática".into(),
            ));
        }
        if self
            .repository
            .has_active_manual_run(&request.project_id, request.agent_role)?
        {
            return Err(TerminalError::Validation(
                "já existe uma execução manual ativa para este agente".into(),
            ));
        }
        let config = self
            .repository
            .get_config(&request.project_id, request.agent_role)?;
        if config.execution_mode != AgentExecutionMode::CliProcess {
            return Err(TerminalError::Validation(
                "execução manual está disponível somente para agentes em CLI Mode".into(),
            ));
        }
        if cli_mode_disabled() {
            return Err(TerminalError::Validation(
                "CLI Mode está desativado pelo kill switch local".into(),
            ));
        }
        let (executable, detected_version, _) =
            self.resolve_launch(config.backend, config.custom_executable.as_deref())?;
        let workspace = self.repository.project_workspace(&request.project_id)?;
        let spec = build_process_spec(CliInvocationRequest {
            executable,
            backend: config.backend,
            model: config.model.clone(),
            model_profile: config.model_profile.clone(),
            thinking_level: config.thinking_level,
            prompt,
            schema_file: None,
            schema: None,
            custom_args: config.custom_args.clone(),
            resume_session_id: None,
        })?;
        let deadline = Instant::now() + DEFAULT_MANUAL_TIMEOUT;
        let run = self.repository.create_run(CreateTerminalRunRequest {
            project_id: request.project_id.clone(),
            agent_role: request.agent_role,
            origin: ExecutionOrigin::Manual,
            execution_mode: AgentExecutionMode::CliProcess,
            backend: Some(config.backend),
            model: config.model,
            pulsation_id: None,
            job_id: None,
            agent_run_id: None,
            llm_execution_id: None,
        })?;
        let run = self
            .repository
            .set_run_deadline(&run.id, &deadline_timestamp(deadline))?;
        let run = self
            .repository
            .set_adapter_metadata(&run.id, detected_version.as_deref())?;
        let cancellation = CancellationToken::new();
        self.active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(run.id.clone(), cancellation.clone());
        let service = Arc::clone(self);
        let run_for_task = run.clone();
        self.runtime.spawn(async move {
            service
                .run_process(run_for_task, spec, workspace, cancellation, deadline)
                .await;
        });
        Ok(run)
    }

    pub fn start_free_form(
        self: &Arc<Self>,
        request: StartFreeFormRunRequest,
    ) -> Result<TerminalRun, TerminalError> {
        let prompt = required(&request.prompt, "prompt")?.to_string();
        if cli_mode_disabled() {
            return Err(TerminalError::Validation(
                "CLI Mode está desativado pelo kill switch local".into(),
            ));
        }
        let slot_ref = SessionSlot::FreeForm {
            id: required(&request.slot_id, "slotId")?.to_string(),
        };
        if self
            .repository
            .has_active_slot_run(&request.project_id, &slot_ref)?
        {
            return Err(TerminalError::Validation(
                "já existe uma execução ativa para este agente livre".into(),
            ));
        }
        let slot = self.repository.get_slot(&request.project_id, &slot_ref)?;
        if slot.execution_mode != AgentExecutionMode::CliProcess {
            return Err(TerminalError::Validation(
                "execução manual está disponível somente em CLI Mode".into(),
            ));
        }
        let (executable, detected_version, _) =
            self.resolve_launch(slot.backend, slot.custom_executable.as_deref())?;
        let workspace = self.repository.project_workspace(&request.project_id)?;
        let spec = build_process_spec(CliInvocationRequest {
            executable,
            backend: slot.backend,
            model: slot.model.clone(),
            model_profile: slot.model_profile.clone(),
            thinking_level: slot.thinking_level,
            prompt,
            schema_file: None,
            schema: None,
            custom_args: slot.custom_args,
            resume_session_id: None,
        })?;
        let deadline = Instant::now() + DEFAULT_MANUAL_TIMEOUT;
        let run = self
            .repository
            .create_session_run(CreateSessionRunRequest {
                project_id: request.project_id,
                slot: slot_ref,
                origin: ExecutionOrigin::Manual,
                execution_mode: AgentExecutionMode::CliProcess,
                backend: Some(slot.backend),
                model: slot.model,
                pulsation_id: None,
                job_id: None,
                agent_run_id: None,
                llm_execution_id: None,
                parent_run_id: None,
                continuation_reason: None,
            })?;
        let run = self
            .repository
            .set_run_deadline(&run.id, &deadline_timestamp(deadline))?;
        let run = self
            .repository
            .set_adapter_metadata(&run.id, detected_version.as_deref())?;
        let cancellation = CancellationToken::new();
        self.active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(run.id.clone(), cancellation.clone());
        let service = Arc::clone(self);
        let run_for_task = run.clone();
        self.runtime.spawn(async move {
            service
                .run_process(run_for_task, spec, workspace, cancellation, deadline)
                .await;
        });
        Ok(run)
    }

    pub fn resume_manual(
        self: &Arc<Self>,
        request: ResumeManualRunRequest,
    ) -> Result<TerminalRun, TerminalError> {
        let prompt = required(&request.prompt, "prompt")?.to_string();
        if cli_mode_disabled() {
            return Err(TerminalError::Validation(
                "CLI Mode está desativado pelo kill switch local".into(),
            ));
        }
        let source = self.repository.get_run(&request.source_run_id)?;
        if source.project_id != request.project_id {
            return Err(TerminalError::Validation(
                "a sessão escolhida pertence a outro projeto".into(),
            ));
        }
        if source.status.is_active() {
            return Err(TerminalError::Validation(
                "a execução escolhida ainda está ativa".into(),
            ));
        }
        if source.workspace_snapshot_hash.is_none() || source.session_fingerprint.is_none() {
            return Err(TerminalError::Validation(
                "a execução escolhida não possui fingerprint compatível para retomada".into(),
            ));
        }
        let native_session_id = source.native_session_id.clone().ok_or_else(|| {
            TerminalError::Validation("a execução escolhida não possui sessão nativa".into())
        })?;
        if self
            .repository
            .has_active_slot_run(&request.project_id, &source.slot)?
        {
            return Err(TerminalError::Validation(
                "já existe uma execução ativa para este slot".into(),
            ));
        }
        let (backend, model, model_profile, thinking_level, custom_executable, custom_args) =
            match source.agent_role {
                Some(role) => {
                    if self
                        .repository
                        .has_active_automatic_run(&request.project_id, role)?
                    {
                        return Err(TerminalError::Validation(
                            "o agente está ocupado por uma execução automática".into(),
                        ));
                    }
                    let config = self.repository.get_config(&request.project_id, role)?;
                    if config.execution_mode != AgentExecutionMode::CliProcess {
                        return Err(TerminalError::Validation(
                            "ative CLI Mode antes de retomar esta sessão".into(),
                        ));
                    }
                    (
                        config.backend,
                        config.model,
                        config.model_profile,
                        config.thinking_level,
                        config.custom_executable,
                        config.custom_args,
                    )
                }
                None => {
                    let slot = self
                        .repository
                        .get_slot(&request.project_id, &source.slot)?;
                    if slot.execution_mode != AgentExecutionMode::CliProcess
                        || slot.session_policy != SessionPolicy::ResumeExplicit
                    {
                        return Err(TerminalError::Validation(
                            "o agente livre não está configurado para retomada explícita".into(),
                        ));
                    }
                    (
                        slot.backend,
                        slot.model,
                        slot.model_profile,
                        slot.thinking_level,
                        slot.custom_executable,
                        slot.custom_args,
                    )
                }
            };
        if source.backend != Some(backend) || source.model != model {
            return Err(TerminalError::Validation(
                "backend ou modelo mudou desde a sessão escolhida".into(),
            ));
        }
        let (executable, detected_version, capabilities) =
            self.resolve_launch(backend, custom_executable.as_deref())?;
        if !capabilities.native_resume {
            return Err(TerminalError::Validation(format!(
                "{} não oferece retomada nativa compatível nesta versão",
                backend.label()
            )));
        }
        let workspace = self.repository.project_workspace(&request.project_id)?;
        let spec = build_process_spec(CliInvocationRequest {
            executable,
            backend,
            model: model.clone(),
            model_profile,
            thinking_level,
            prompt,
            schema_file: None,
            schema: None,
            custom_args,
            resume_session_id: Some(native_session_id),
        })?;
        let deadline = Instant::now() + DEFAULT_MANUAL_TIMEOUT;
        let run = self
            .repository
            .create_session_run(CreateSessionRunRequest {
                project_id: request.project_id,
                slot: source.slot,
                origin: ExecutionOrigin::Manual,
                execution_mode: AgentExecutionMode::CliProcess,
                backend: Some(backend),
                model,
                pulsation_id: None,
                job_id: None,
                agent_run_id: None,
                llm_execution_id: None,
                parent_run_id: Some(source.id),
                continuation_reason: Some("explicit_resume".into()),
            })?;
        let run = self
            .repository
            .set_run_deadline(&run.id, &deadline_timestamp(deadline))?;
        let run = self
            .repository
            .set_adapter_metadata(&run.id, detected_version.as_deref())?;
        let cancellation = CancellationToken::new();
        self.active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(run.id.clone(), cancellation.clone());
        let service = Arc::clone(self);
        let run_for_task = run.clone();
        self.runtime.spawn(async move {
            service
                .run_process(run_for_task, spec, workspace, cancellation, deadline)
                .await;
        });
        Ok(run)
    }

    pub async fn execute_llm_request(
        self: &Arc<Self>,
        request: &LlmRequest,
        cancellation: &CancellationToken,
    ) -> Result<Option<CliCompletion>, TerminalError> {
        if request.pulsation_id.as_deref().is_some_and(|pulsation_id| {
            self.repository
                .has_api_override(pulsation_id, request.agent_role)
                .unwrap_or(false)
        }) {
            return Ok(None);
        }
        let config = match self
            .repository
            .get_config(&request.project_id, request.agent_role)
        {
            Ok(config) => config,
            Err(TerminalError::NotFound(_)) => {
                let discovery = self.discovery(false).await;
                let codex_ready = discovery.iter().any(|item| {
                    item.backend == CliBackend::Codex
                        && item.state == CliAvailabilityState::Available
                });
                self.repository
                    .ensure_project_configs(&request.project_id, codex_ready)?;
                self.repository
                    .get_config(&request.project_id, request.agent_role)?
            }
            Err(error) => return Err(error),
        };
        if config.execution_mode == AgentExecutionMode::ApiDirect {
            return Ok(None);
        }
        let feature_discovery =
            request.pulsation_id.is_none() && request.job_chain.starts_with("feature:");
        if feature_discovery && config.backend == CliBackend::Custom {
            return Err(TerminalError::Validation(
                "Feature discovery requires an official read-only CLI adapter or API transport"
                    .into(),
            ));
        }
        if cli_mode_disabled() {
            return Err(TerminalError::Validation(
                "CLI Mode está desativado pelo kill switch local".into(),
            ));
        }
        if self
            .repository
            .has_active_manual_run(&request.project_id, request.agent_role)?
        {
            return Err(TerminalError::Process(
                "o agente possui uma execução manual ativa".into(),
            ));
        }
        let (executable, detected_version) = match config.backend {
            CliBackend::Custom => (
                config
                    .custom_executable
                    .as_deref()
                    .map(PathBuf::from)
                    .ok_or_else(|| {
                        TerminalError::Validation("configure o executável Custom".into())
                    })?,
                None,
            ),
            backend => {
                let discovery = self.discovery(false).await;
                let availability = discovery
                    .into_iter()
                    .find(|item| item.backend == backend)
                    .ok_or_else(|| TerminalError::NotFound(format!("{} CLI", backend.label())))?;
                if availability.state != CliAvailabilityState::Available {
                    return Err(TerminalError::Process(
                        availability
                            .diagnostic
                            .unwrap_or_else(|| format!("{} não está pronto", backend.label())),
                    ));
                }
                (
                    availability.executable_path.ok_or_else(|| {
                        TerminalError::NotFound(format!("{} CLI", backend.label()))
                    })?,
                    availability.version,
                )
            }
        };
        let mut schema_guard = None;
        let schema = match &request.response_format {
            ResponseFormat::JsonSchema { schema, .. } => Some(schema.clone()),
            _ => None,
        };
        let schema_file = if config.backend == CliBackend::Codex {
            if let Some(schema) = schema.as_ref() {
                let mut file = tempfile::NamedTempFile::new()?;
                serde_json::to_writer(file.as_file_mut(), schema)?;
                let path = file.path().to_path_buf();
                schema_guard = Some(file);
                Some(path)
            } else {
                None
            }
        } else {
            None
        };
        let mut prompt = format!(
            "<system>\n{}\n</system>\n<user>\n{}\n</user>",
            request.system_prompt, request.user_prompt
        );
        if matches!(
            config.backend,
            CliBackend::Gemini | CliBackend::Opencode | CliBackend::Custom
        ) && let Some(schema) = schema.as_ref()
        {
            prompt.push_str("\nReturn only valid JSON that satisfies this schema. Follow the request language contract; generated artifacts must be English:\n");
            prompt.push_str(&serde_json::to_string(schema)?);
        }
        // Discovery receives only host-selected context, with the Companion's tool restrictions.
        let discovery_directory = feature_discovery.then(tempfile::tempdir).transpose()?;
        let workspace = match &discovery_directory {
            Some(directory) => directory.path().to_path_buf(),
            None => self.repository.project_workspace(&request.project_id)?,
        };
        let mut spec = build_process_spec(CliInvocationRequest {
            executable,
            backend: config.backend,
            model: config.model.clone(),
            model_profile: config.model_profile.clone(),
            thinking_level: config.thinking_level,
            prompt,
            schema_file,
            schema,
            custom_args: config.custom_args.clone(),
            resume_session_id: None,
        })?;
        // Every pulsation is hardened now, not only discovery: a stage that
        // inherits the developer's MCP servers or rules can be handed write and
        // shell tools back, contradicting the read-only contract in its prompt.
        let hardening_guard = harden_spec(
            config.backend,
            if feature_discovery {
                Hardening::Companion
            } else {
                Hardening::Pipeline
            },
            &mut spec,
        )?;
        let origin = if feature_discovery {
            ExecutionOrigin::Feature
        } else if request.agent_role == AgentRole::LearningAgent {
            ExecutionOrigin::Learning
        } else {
            ExecutionOrigin::Pipeline
        };
        let run = self.repository.create_run(CreateTerminalRunRequest {
            project_id: request.project_id.clone(),
            agent_role: request.agent_role,
            origin,
            execution_mode: AgentExecutionMode::CliProcess,
            backend: Some(config.backend),
            model: config.model.clone(),
            pulsation_id: request.pulsation_id.clone(),
            job_id: request.job_id.clone(),
            agent_run_id: None,
            llm_execution_id: Some(request.execution_id.0.clone()),
        })?;
        let run = self
            .repository
            .set_run_deadline(&run.id, &deadline_timestamp(request.deadline))?;
        let run = self
            .repository
            .set_adapter_metadata(&run.id, detected_version.as_deref())?;
        let local_cancellation = CancellationToken::new();
        self.active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(run.id.clone(), local_cancellation.clone());
        let combined = CancellationToken::new();
        let combined_child = combined.clone();
        let parent = cancellation.clone();
        let local = local_cancellation.clone();
        let cancellation_bridge = tokio::spawn(async move {
            tokio::select! {_=parent.cancelled()=>{},_=local.cancelled()=>{}}
            combined_child.cancel();
        });
        let outcome = self
            .run_process_inner(&run, spec, workspace, &combined, request.deadline)
            .await;
        cancellation_bridge.abort();
        self.active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&run.id);
        self.active_changed.notify_waiters();
        drop(schema_guard);
        drop(hardening_guard);
        drop(discovery_directory);
        if let Err(error) = &outcome {
            self.record_process_failure(&run, error);
        }
        let outcome = outcome?;
        match outcome.termination {
            ProcessTermination::Cancelled => return Err(TerminalError::Cancelled),
            ProcessTermination::TimedOut(_) => return Err(TerminalError::Timeout),
            ProcessTermination::Exited => {}
        }
        if !outcome.success {
            let message = format!(
                "{} encerrou com código {:?}",
                config.backend.label(),
                outcome.exit_code
            );
            return Err(
                match self.repository.get_run(&run.id)?.error_code.as_deref() {
                    Some("cli_auth_required") => {
                        TerminalError::AuthRequired(config.backend.label().into())
                    }
                    Some("cli_version_unsupported") => {
                        TerminalError::VersionUnsupported(config.backend.label().into())
                    }
                    Some("cli_rate_limited") => {
                        TerminalError::RateLimited(config.backend.label().into())
                    }
                    _ => TerminalError::Process(message),
                },
            );
        }
        if outcome.raw_output.trim().is_empty() {
            return Err(TerminalError::Process(format!(
                "{} não produziu conteúdo final",
                config.backend.label()
            )));
        }
        Ok(Some(CliCompletion {
            terminal_run_id: run.id.clone(),
            backend: config.backend,
            model: config.model.unwrap_or_else(|| "default".into()),
            raw_output: outcome.raw_output,
            request_payload: serde_json::json!({"mode":"cli_process","backend":config.backend.as_str(),"thinking":config.thinking_level.as_str()}),
            response_payload: serde_json::json!({"terminalRunId":run.id,"exitCode":outcome.exit_code}),
        }))
    }

    async fn run_process(
        self: Arc<Self>,
        run: TerminalRun,
        spec: CliProcessSpec,
        workspace: PathBuf,
        cancellation: CancellationToken,
        deadline: Instant,
    ) {
        let result = self
            .run_process_inner(&run, spec, workspace, &cancellation, deadline)
            .await;
        if let Err(error) = result {
            self.record_process_failure(&run, &error);
        }
        self.active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&run.id);
        self.active_changed.notify_waiters();
    }

    async fn run_process_inner(
        &self,
        run: &TerminalRun,
        spec: CliProcessSpec,
        workspace: PathBuf,
        cancellation: &CancellationToken,
        deadline: Instant,
    ) -> Result<CliProcessOutcome, TerminalError> {
        if cancellation.is_cancelled() {
            return Err(TerminalError::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(TerminalError::Timeout);
        }
        let backend = spec_backend(run);
        let (safe_workspace, secured_launch) = tokio::task::spawn_blocking(move || {
            Ok::<_, TerminalError>((
                prepare_safe_cli_workspace(&workspace)?,
                secure_launch_spec(spec, backend)?,
            ))
        })
        .await
        .map_err(|error| {
            TerminalError::Security(format!("worker do snapshot foi interrompido: {error}"))
        })??;
        if cancellation.is_cancelled() {
            return Err(TerminalError::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(TerminalError::Timeout);
        }
        let launch_identity_sha256 = secured_launch.identity_sha256;
        let spec = secured_launch.spec;
        let session_fingerprint = session_fingerprint(run, &safe_workspace.manifest_hash);
        if let Some(parent_run_id) = run.parent_run_id.as_deref() {
            let parent = self.repository.get_run(parent_run_id)?;
            validate_resume_parent(
                run,
                &parent,
                &safe_workspace.manifest_hash,
                spec.resume_session_id.as_deref(),
            )?;
        }
        self.repository
            .set_workspace_snapshot_hash(&run.id, &safe_workspace.manifest_hash)?;
        self.repository
            .set_session_fingerprint(&run.id, &session_fingerprint)?;
        self.repository
            .set_launch_identity(&run.id, &launch_identity_sha256)?;
        let starting = self.repository.set_run_status(
            &run.id,
            TerminalRunStatus::Starting,
            None,
            None,
            None,
        )?;
        self.emit(
            &starting,
            self.repository.next_event_sequence(&run.id)?,
            TerminalEventKind::System,
            None,
            "Execução iniciando.",
        )?;
        let mut command = Command::new(&spec.executable);
        command
            .args(&spec.args)
            .current_dir(safe_workspace.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .env_clear();
        copy_safe_environment(&mut command);
        command.envs(spec.environment.iter().map(|(key, value)| (key, value)));
        configure_hidden(&mut command);
        let mut child = command.spawn()?;
        let process_tree = ProcessTreeJob::assign(&child)?;
        let mut stdin = child.stdin.take().ok_or_else(|| {
            TerminalError::Validation("stdin do processo CLI indisponível".into())
        })?;
        stdin.write_all(&spec.stdin_payload).await?;
        stdin.write_all(b"\n").await?;
        stdin.shutdown().await?;
        drop(stdin);
        let stdout = child.stdout.take().ok_or_else(|| {
            TerminalError::Validation("stdout do processo CLI indisponível".into())
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            TerminalError::Validation("stderr do processo CLI indisponível".into())
        })?;
        let running = self.repository.set_run_status(
            &run.id,
            TerminalRunStatus::Running,
            None,
            None,
            None,
        )?;
        self.emit(
            &running,
            self.repository.next_event_sequence(&run.id)?,
            TerminalEventKind::System,
            None,
            "Execução em andamento.",
        )?;
        let (stream_sender, stream_receiver) = mpsc::channel(PROCESS_EVENT_CHANNEL_CAPACITY);
        let abort_token = CancellationToken::new();
        let abort_reason = Arc::new(Mutex::new(None));
        let caller_remaining = deadline.saturating_duration_since(Instant::now());
        let timeout_policy = match run.slot {
            SessionSlot::Canonical { agent_role } if run.origin != ExecutionOrigin::Manual => {
                let mut policy = AgentTimeoutPolicy::for_role(agent_role);
                if caller_remaining > Duration::ZERO && caller_remaining < policy.max_timeout() {
                    policy.max_timeout_ms = caller_remaining.as_millis() as u64;
                    policy.base_timeout_ms = policy.base_timeout_ms.min(policy.max_timeout_ms);
                    policy.inactivity_window_ms =
                        policy.inactivity_window_ms.min(policy.max_timeout_ms);
                }
                policy
            }
            SessionSlot::FreeForm { .. } => {
                let mut policy = AgentTimeoutPolicy::manual();
                if caller_remaining > Duration::ZERO && caller_remaining < policy.max_timeout() {
                    policy.max_timeout_ms = caller_remaining.as_millis() as u64;
                    policy.base_timeout_ms = policy.base_timeout_ms.min(policy.max_timeout_ms);
                    policy.inactivity_window_ms =
                        policy.inactivity_window_ms.min(policy.max_timeout_ms);
                }
                policy
            }
            SessionSlot::Canonical { agent_role } => {
                let mut policy = AgentTimeoutPolicy::for_role(agent_role);
                if caller_remaining > Duration::ZERO && caller_remaining < policy.max_timeout() {
                    policy.max_timeout_ms = caller_remaining.as_millis() as u64;
                    policy.base_timeout_ms = policy.base_timeout_ms.min(policy.max_timeout_ms);
                    policy.inactivity_window_ms =
                        policy.inactivity_window_ms.min(policy.max_timeout_ms);
                }
                policy
            }
        };
        let watchdog = ProgressWatchdog::new(timeout_policy);
        let coordinator_task = tokio::spawn(coordinate_process_events(
            self.repository.clone(),
            self.event_sink.clone(),
            running,
            spec_backend(run),
            stream_receiver,
            abort_token.clone(),
            Arc::clone(&abort_reason),
            watchdog.clone(),
        ));
        let stdout_task = tokio::spawn(read_process_stream(
            "stdout",
            stdout,
            stream_sender.clone(),
            watchdog.clone(),
        ));
        let stderr_task = tokio::spawn(read_process_stream(
            "stderr",
            stderr,
            stream_sender.clone(),
            watchdog.clone(),
        ));
        drop(stream_sender);
        let (status, termination) = tokio::select! {
            status=child.wait()=>(status?,ProcessTermination::Exited),
            _=cancellation.cancelled()=>{
                process_tree.terminate();
                let _=child.kill().await;
                (child.wait().await?,ProcessTermination::Cancelled)
            }
            reason=watchdog.wait_for_timeout()=>{
                process_tree.terminate();
                let _=child.kill().await;
                (child.wait().await?,ProcessTermination::TimedOut(reason))
            }
            _=abort_token.cancelled()=>{
                process_tree.terminate();
                let _=child.kill().await;
                (child.wait().await?,ProcessTermination::Exited)
            }
        };
        stdout_task.await.map_err(|error| {
            TerminalError::Process(format!("leitor stdout interrompido: {error}"))
        })?;
        stderr_task.await.map_err(|error| {
            TerminalError::Process(format!("leitor stderr interrompido: {error}"))
        })?;
        let process_events = coordinator_task.await.map_err(|error| {
            TerminalError::Process(format!("coordenador de eventos interrompido: {error}"))
        })??;
        if let Some(reason) = *abort_reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            return Err(match reason {
                ProcessAbortReason::OutputLimit => TerminalError::OutputLimit,
                ProcessAbortReason::Persistence => {
                    TerminalError::Process("persistência do stream foi interrompida".into())
                }
            });
        }
        let cancellation_code = if run.origin == ExecutionOrigin::Manual {
            "user_cancelled"
        } else {
            "pipeline_cancelled"
        };
        let failure_kind = adapters::classify_failure(
            spec_backend(run),
            status.code(),
            &process_events.last_stderr,
        );
        let (terminal_status, error_code, error_message, event_kind, final_message) =
            match termination {
                ProcessTermination::Cancelled => (
                    TerminalRunStatus::Cancelled,
                    Some(cancellation_code),
                    Some("A execução foi cancelada."),
                    TerminalEventKind::Warning,
                    "Execução cancelada.",
                ),
                ProcessTermination::TimedOut(reason) => (
                    TerminalRunStatus::Failed,
                    Some("cli_timeout"),
                    Some(reason.description()),
                    TerminalEventKind::Error,
                    match reason {
                        TimeoutReason::Inactivity => "Execução encerrada por inatividade.",
                        TimeoutReason::MaxCeiling => "Execução encerrada por timeout.",
                    },
                ),
                ProcessTermination::Exited if status.success() => (
                    TerminalRunStatus::Completed,
                    None,
                    None,
                    TerminalEventKind::Completed,
                    "Execução concluída.",
                ),
                ProcessTermination::Exited => (
                    TerminalRunStatus::Failed,
                    Some(failure_kind.code()),
                    Some("O CLI encerrou com código diferente de zero."),
                    TerminalEventKind::Error,
                    "Execução encerrada com falha.",
                ),
            };
        self.repository.set_run_status(
            &run.id,
            terminal_status,
            status.code(),
            error_code,
            error_message,
        )?;
        self.emit(
            run,
            self.repository.next_event_sequence(&run.id)?,
            event_kind,
            None,
            final_message,
        )?;
        Ok(CliProcessOutcome {
            success: termination == ProcessTermination::Exited && status.success(),
            exit_code: status.code(),
            raw_output: process_events.raw_output,
            termination,
        })
    }

    fn record_process_failure(&self, run: &TerminalRun, error: &TerminalError) {
        let current = self.repository.get_run(&run.id).ok();
        if current.is_some_and(|current| current.status.is_active()) {
            let (status, event_kind) = if matches!(error, TerminalError::Cancelled) {
                (TerminalRunStatus::Cancelled, TerminalEventKind::Warning)
            } else {
                (TerminalRunStatus::Failed, TerminalEventKind::Error)
            };
            let _ = self.repository.set_run_status(
                &run.id,
                status,
                None,
                Some(
                    if matches!(error, TerminalError::Cancelled)
                        && run.origin != ExecutionOrigin::Manual
                    {
                        "pipeline_cancelled"
                    } else {
                        error.code()
                    },
                ),
                Some(&error.to_string()),
            );
            if let Ok(sequence) = self.repository.next_event_sequence(&run.id) {
                let _ = self.emit(run, sequence, event_kind, None, &error.to_string());
            }
        }
    }

    fn emit(
        &self,
        run: &TerminalRun,
        sequence: u64,
        kind: TerminalEventKind,
        stream: Option<&str>,
        content: &str,
    ) -> Result<(), TerminalError> {
        let event = self
            .repository
            .append_event(&run.id, sequence, kind, stream, content)?;
        let run = self.repository.get_run(&run.id)?;
        self.event_sink.emit(&TerminalEventEnvelope {
            protocol_version: 2,
            project_id: run.project_id.clone(),
            agent_role: run.agent_role,
            run_id: run.id.clone(),
            sequence,
            run,
            event,
        });
        Ok(())
    }
}

fn spec_backend(run: &TerminalRun) -> CliBackend {
    run.backend.unwrap_or(CliBackend::Custom)
}

async fn read_process_stream<R: AsyncRead + Unpin>(
    stream: &'static str,
    mut reader: R,
    sender: mpsc::Sender<ProcessStreamItem>,
    watchdog: ProgressWatchdog,
) {
    let mut buffer = [0u8; 8192];
    let mut pending = Vec::new();
    loop {
        let read = match reader.read(&mut buffer).await {
            Ok(value) => value,
            Err(error) => {
                let _ = sender
                    .send(ProcessStreamItem::ReadError {
                        stream,
                        message: error.to_string(),
                    })
                    .await;
                break;
            }
        };
        if read == 0 {
            if !pending.is_empty() {
                let _ = sender
                    .send(ProcessStreamItem::Frame {
                        stream,
                        bytes: pending,
                    })
                    .await;
            }
            break;
        }
        watchdog.record_progress();
        pending.extend_from_slice(&buffer[..read]);
        while let Some(index) = pending.iter().position(|byte| *byte == b'\n') {
            let frame = pending.drain(..=index).collect::<Vec<_>>();
            if sender
                .send(ProcessStreamItem::Frame {
                    stream,
                    bytes: frame,
                })
                .await
                .is_err()
            {
                return;
            }
        }
        while pending.len() > MAX_PENDING_STREAM_BYTES {
            let frame = pending.drain(..MAX_EVENT_CHARS).collect::<Vec<_>>();
            if sender
                .send(ProcessStreamItem::Frame {
                    stream,
                    bytes: frame,
                })
                .await
                .is_err()
            {
                return;
            }
        }
    }
}

async fn coordinate_process_events(
    repository: TerminalRepository,
    event_sink: Arc<dyn TerminalEventSink>,
    run: TerminalRun,
    backend: CliBackend,
    mut receiver: mpsc::Receiver<ProcessStreamItem>,
    abort_token: CancellationToken,
    abort_reason: Arc<Mutex<Option<ProcessAbortReason>>>,
    watchdog: ProgressWatchdog,
) -> Result<ProcessEventSummary, TerminalError> {
    let mut sequence = repository.next_event_sequence(&run.id)?;
    let mut event_count = 0usize;
    let mut observed_bytes = 0usize;
    let mut summary = ProcessEventSummary::default();
    let mut incomplete_stream = false;
    let usage = crate::usage::cli::CliUsageRecorder::new(
        &repository.db_path,
        crate::usage::UsageContext {
            source_kind: "terminal_run".into(),
            source_id: run.id.clone(),
            pulsation_id: run.pulsation_id.clone(),
            project_id: Some(run.project_id.clone()),
            transport: crate::usage::UsageTransport::Cli,
            provider_id: backend.as_str().into(),
            requested_model: run.model.clone(),
            started_at: run
                .started_at
                .clone()
                .unwrap_or_else(|| run.created_at.clone()),
        },
        backend,
    )
    .map_err(|error| {
        signal_process_abort(&abort_token, &abort_reason, ProcessAbortReason::Persistence);
        TerminalError::Database(error)
    })?;
    while let Some(item) = receiver.recv().await {
        watchdog.record_progress();
        let (kind, stream, content, input_bytes, native_session_id, observed_model, is_text) =
            match item {
                ProcessStreamItem::Frame { stream, bytes } => {
                    let input_bytes = bytes.len();
                    if stream == "stdout"
                        && let Err(error) = usage.observe_line(&bytes)
                    {
                        signal_process_abort(
                            &abort_token,
                            &abort_reason,
                            ProcessAbortReason::Persistence,
                        );
                        return Err(error.into());
                    }
                    let normalized = adapters::normalize_frame(backend, stream, &bytes);
                    (
                        normalized.kind,
                        stream,
                        normalized.content,
                        input_bytes,
                        normalized.native_session_id,
                        normalized.observed_model,
                        normalized.is_text,
                    )
                }
                ProcessStreamItem::ReadError { stream, message } => {
                    incomplete_stream = true;
                    let input_bytes = message.len();
                    (
                        TerminalEventKind::Error,
                        stream,
                        message,
                        input_bytes,
                        None,
                        None,
                        false,
                    )
                }
            };
        if let Some(session_id) = native_session_id
            && let Err(error) = repository.set_native_session_id(&run.id, &session_id)
        {
            signal_process_abort(&abort_token, &abort_reason, ProcessAbortReason::Persistence);
            return Err(error);
        }
        if let Some(model) = observed_model
            && let Err(error) = repository.set_observed_model(&run.id, &model)
        {
            signal_process_abort(&abort_token, &abort_reason, ProcessAbortReason::Persistence);
            return Err(error);
        }
        if content.trim().is_empty() {
            continue;
        }
        event_count = event_count.saturating_add(1);
        observed_bytes = observed_bytes.saturating_add(input_bytes);
        if event_count > MAX_RUN_EVENTS || observed_bytes > MAX_RUN_OUTPUT_BYTES {
            signal_process_abort(&abort_token, &abort_reason, ProcessAbortReason::OutputLimit);
            return Err(TerminalError::OutputLimit);
        }
        let event = match persist_and_emit(
            &repository,
            &event_sink,
            &run,
            sequence,
            kind,
            Some(stream),
            &content,
        ) {
            Ok(event) => event,
            Err(error) => {
                signal_process_abort(&abort_token, &abort_reason, ProcessAbortReason::Persistence);
                return Err(error);
            }
        };
        if is_text
            && matches!(
                event.kind,
                TerminalEventKind::Completed
                    | TerminalEventKind::Message
                    | TerminalEventKind::Stdout
            )
        {
            summary.raw_output.clone_from(&event.content);
        }
        if stream == "stderr" {
            summary.last_stderr.clone_from(&event.content);
        }
        sequence = sequence.saturating_add(1);
    }
    if incomplete_stream {
        usage.mark_partial()?;
    }
    Ok(summary)
}

fn signal_process_abort(
    abort_token: &CancellationToken,
    abort_reason: &Mutex<Option<ProcessAbortReason>>,
    reason: ProcessAbortReason,
) {
    *abort_reason
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(reason);
    abort_token.cancel();
}

fn persist_and_emit(
    repository: &TerminalRepository,
    event_sink: &Arc<dyn TerminalEventSink>,
    run: &TerminalRun,
    sequence: u64,
    kind: TerminalEventKind,
    stream: Option<&str>,
    content: &str,
) -> Result<TerminalEvent, TerminalError> {
    let event = repository.append_event(&run.id, sequence, kind, stream, content)?;
    let run = repository.get_run(&run.id)?;
    event_sink.emit(&TerminalEventEnvelope {
        protocol_version: 2,
        project_id: run.project_id.clone(),
        agent_role: run.agent_role,
        run_id: run.id.clone(),
        sequence,
        run,
        event: event.clone(),
    });
    Ok(event)
}

fn copy_safe_environment(command: &mut Command) {
    for name in [
        "PATH",
        "PATHEXT",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "TEMP",
        "TMP",
        "SystemRoot",
        "ComSpec",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramData",
    ] {
        if let Some(value) = env::var_os(name) {
            command.env(name, value);
        }
    }
}

/// How much of the host's environment a spawned CLI is allowed to see.
///
/// `Companion` turns every native tool off: discovery receives only
/// host-selected context. `Pipeline` keeps the read tools, because a stage
/// agent legitimately inspects the workspace, but still isolates the
/// developer's own configuration, rules, MCP servers and skills - any of which
/// could hand write or shell tools back to the model and falsify the read-only
/// contract the stage prompt states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Hardening {
    Companion,
    Pipeline,
}

fn harden_spec(
    backend: CliBackend,
    mode: Hardening,
    spec: &mut CliProcessSpec,
) -> Result<Option<tempfile::NamedTempFile>, TerminalError> {
    let mut settings_guard = None;
    match backend {
        CliBackend::Codex => {
            if let Some(exec_index) = spec.args.iter().position(|argument| argument == "exec") {
                spec.args
                    .insert(exec_index + 1, "--ignore-user-config".into());
                spec.args.insert(exec_index + 2, "--ignore-rules".into());
                if mode == Hardening::Companion {
                    spec.args.insert(exec_index + 3, "--ephemeral".into());
                }
            }
        }
        CliBackend::Claude => {
            if mode == Hardening::Companion
                && let Some(index) = spec
                    .args
                    .iter()
                    .position(|argument| argument == "Read,Glob,Grep")
            {
                spec.args[index].clear();
            }
            spec.args.push("--no-session-persistence".into());
            spec.args
                .extend(["--setting-sources".into(), String::new()]);
            spec.args.push("--strict-mcp-config".into());
            spec.args.push("--no-chrome".into());
            spec.args.push("--disable-slash-commands".into());
        }
        CliBackend::Gemini => {
            let tools = if mode == Hardening::Companion {
                serde_json::json!({ "core": [], "allowed": [] })
            } else {
                // Stage agents keep the read tools; only user configuration goes.
                serde_json::json!({ "allowed": [] })
            };
            let mut settings = tempfile::NamedTempFile::new()?;
            serde_json::to_writer(
                settings.as_file_mut(),
                &serde_json::json!({
                    "tools": tools,
                    "mcp": { "allowed": [] },
                    "context": { "fileName": [], "loadFromIncludeDirectories": false },
                    "hooksConfig": { "enabled": false },
                    "skills": { "enabled": false }
                }),
            )?;
            spec.environment.push((
                "GEMINI_CLI_SYSTEM_SETTINGS_PATH".into(),
                settings.path().to_string_lossy().into_owned(),
            ));
            spec.args.extend(["--extensions".into(), "none".into()]);
            spec.args
                .extend(["--allowed-mcp-server-names".into(), String::new()]);
            settings_guard = Some(settings);
        }
        CliBackend::Opencode => {
            if mode == Hardening::Companion
                && let Some((_, permissions)) = spec
                    .environment
                    .iter_mut()
                    .find(|(name, _)| name == "OPENCODE_PERMISSION")
            {
                *permissions = r#"{"*":"deny"}"#.into();
            }
            // Pipeline keeps the adapter's read-only permission set as-is.
        }
        CliBackend::Custom => {
            if mode == Hardening::Companion {
                return Err(TerminalError::Validation(
                    "Custom CLI is not allowed for the Companion".into(),
                ));
            }
            // An arbitrary binary exposes no config, tool or sandbox knob to turn
            // off. What actually holds is that publication only ever happens from
            // the JSON `artifacts` array, so files this CLI writes to disk never
            // enter the pipeline. The marker lets a cooperating CLI opt in.
            spec.environment
                .push(("HVC_STAGE_READ_ONLY".into(), "1".into()));
        }
    }
    Ok(settings_guard)
}

async fn run_companion_process(
    spec: CliProcessSpec,
    backend: CliBackend,
    cancellation: &CancellationToken,
    deadline: Instant,
    detected_cli_version: Option<String>,
    usage_path: PathBuf,
    usage_context: crate::usage::UsageContext,
) -> Result<CompanionCliRoundOutput, TerminalError> {
    if cancellation.is_cancelled() {
        return Err(TerminalError::Cancelled);
    }
    if Instant::now() >= deadline {
        return Err(TerminalError::Timeout);
    }
    let directory = tempfile::tempdir()?;
    let mut directory_permissions = fs::metadata(directory.path())?.permissions();
    directory_permissions.set_readonly(true);
    fs::set_permissions(directory.path(), directory_permissions)?;
    let secured = tokio::task::spawn_blocking(move || secure_launch_spec(spec, backend))
        .await
        .map_err(|error| TerminalError::Security(format!("CLI launch worker failed: {error}")))??;
    let mut command = Command::new(&secured.spec.executable);
    command
        .args(&secured.spec.args)
        .current_dir(directory.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env_clear();
    copy_safe_environment(&mut command);
    for (name, value) in &secured.spec.environment {
        command.env(name, value);
    }
    configure_hidden(&mut command);
    let mut child = command.spawn()?;
    let usage = Arc::new(crate::usage::cli::CliUsageRecorder::new(
        &usage_path,
        usage_context,
        backend,
    )?);
    let process_tree = ProcessTreeJob::assign(&child)?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(&secured.spec.stdin_payload).await?;
        stdin.shutdown().await?;
    }
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| TerminalError::Process("Companion CLI stdout is unavailable".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| TerminalError::Process("Companion CLI stderr is unavailable".into()))?;
    let abort = CancellationToken::new();
    let caller_remaining = deadline.saturating_duration_since(Instant::now());
    let mut policy = AgentTimeoutPolicy::companion();
    if caller_remaining > Duration::ZERO && caller_remaining < policy.max_timeout() {
        policy.max_timeout_ms = caller_remaining.as_millis() as u64;
        policy.base_timeout_ms = policy.base_timeout_ms.min(policy.max_timeout_ms);
        policy.inactivity_window_ms = policy.inactivity_window_ms.min(policy.max_timeout_ms);
    }
    let watchdog = ProgressWatchdog::new(policy);
    let stdout_task = tokio::spawn(read_bounded_stream(
        stdout,
        abort.clone(),
        MAX_COMPANION_CLI_OUTPUT_BYTES,
        Some(watchdog.clone()),
        Some(usage.clone()),
    ));
    let stderr_task = tokio::spawn(read_bounded_stream(
        stderr,
        abort.clone(),
        MAX_COMPANION_CLI_OUTPUT_BYTES,
        Some(watchdog.clone()),
        None,
    ));
    let termination = tokio::select! {
        status = child.wait() => (ProcessTermination::Exited, Some(status?)),
        _ = cancellation.cancelled() => {
            process_tree.terminate();
            let _ = child.kill().await;
            let _ = child.wait().await;
            (ProcessTermination::Cancelled, None)
        }
        reason = watchdog.wait_for_timeout() => {
            process_tree.terminate();
            let _ = child.kill().await;
            let _ = child.wait().await;
            (ProcessTermination::TimedOut(reason), None)
        }
        _ = abort.cancelled() => {
            process_tree.terminate();
            let _ = child.kill().await;
            let _ = child.wait().await;
            (ProcessTermination::Exited, None)
        }
    };
    // Drain both readers before settling accounting, including aborts caused by
    // persistence errors. A database error must not become an output-limit error.
    let (stdout, stderr) = tokio::join!(stdout_task, stderr_task);
    let stdout = companion_stream_output(stdout, "stdout");
    let stderr = companion_stream_output(stderr, "stderr");
    if termination.0 != ProcessTermination::Exited
        || termination
            .1
            .as_ref()
            .is_none_or(|status| !status.success())
        || stdout.is_err()
        || stderr.is_err()
    {
        usage.mark_partial()?;
    }
    let stdout = stdout?;
    let stderr = stderr?;
    match termination.0 {
        ProcessTermination::Cancelled => return Err(TerminalError::Cancelled),
        ProcessTermination::TimedOut(_) => return Err(TerminalError::Timeout),
        ProcessTermination::Exited => {}
    }
    let status = termination
        .1
        .ok_or_else(|| TerminalError::Process("Companion CLI status is unavailable".into()))?;
    let sanitized_stdout = sanitize_terminal_text(&stdout);
    let sanitized_stderr = sanitize_terminal_text(&stderr);
    let stdout = redact_secrets(&sanitized_stdout).map_err(TerminalError::Redaction)?;
    let stderr = redact_secrets(&sanitized_stderr).map_err(TerminalError::Redaction)?;
    if !status.success() {
        // Codex reports provider failures in JSONL stdout, often with empty stderr.
        // Decode only error events; never expose reasoning or the control stream.
        let diagnostic = if backend == CliBackend::Codex {
            codex_companion_error(&stdout.text).unwrap_or_else(|| stderr.text.clone())
        } else {
            stderr.text.clone()
        };
        // Decode before the final redaction, including JSON-escaped credentials.
        let diagnostic = redact_secrets(&sanitize_terminal_text(diagnostic.as_bytes()))
            .map_err(TerminalError::Redaction)?
            .text;
        return Err(
            match adapters::classify_failure(backend, status.code(), &diagnostic) {
                adapters::CliFailureKind::AuthenticationRequired => {
                    TerminalError::AuthRequired(backend.label().into())
                }
                adapters::CliFailureKind::VersionUnsupported => {
                    TerminalError::VersionUnsupported(backend.label().into())
                }
                adapters::CliFailureKind::RateLimited => {
                    TerminalError::RateLimited(backend.label().into())
                }
                adapters::CliFailureKind::ProcessFailed => {
                    TerminalError::Process(if diagnostic.is_empty() {
                        format!(
                            "{} CLI exited with status {:?}",
                            backend.label(),
                            status.code()
                        )
                    } else {
                        diagnostic
                    })
                }
            },
        );
    }
    let envelope_json = extract_companion_envelope_json(&stdout.text).ok_or_else(|| {
        TerminalError::Process("Companion CLI returned an invalid control envelope".into())
    })?;
    Ok(CompanionCliRoundOutput {
        envelope_json,
        detected_cli_version,
        exit_code: status.code(),
        output_bytes: stdout.text.len().saturating_add(stderr.text.len()),
        redaction_count: stdout.count.saturating_add(stderr.count),
    })
}

fn companion_stream_output(
    result: Result<Result<Vec<u8>, std::io::Error>, tokio::task::JoinError>,
    stream: &str,
) -> Result<Vec<u8>, TerminalError> {
    result
        .map_err(|error| TerminalError::Process(format!("{stream} reader failed: {error}")))?
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::InvalidData {
                return TerminalError::OutputLimit;
            }
            if error
                .get_ref()
                .is_some_and(|inner| inner.is::<rusqlite::Error>())
            {
                return match error
                    .into_inner()
                    .and_then(|inner| inner.downcast::<rusqlite::Error>().ok())
                {
                    Some(error) => TerminalError::Database(*error),
                    None => TerminalError::Process("Token usage persistence failed".into()),
                };
            }
            TerminalError::Io(error)
        })
}

fn codex_companion_error(stdout: &str) -> Option<String> {
    fn message(value: &Value, depth: usize) -> Option<String> {
        if depth > 6 {
            return None;
        }
        if let Some(text) = value.as_str() {
            return match serde_json::from_str::<Value>(text) {
                Ok(nested) => message(&nested, depth + 1),
                Err(_) => (!text.trim().is_empty()).then(|| text.to_owned()),
            };
        }
        value
            .get("error")
            .or_else(|| value.get("message"))
            .and_then(|value| message(value, depth + 1))
    }
    stdout.lines().rev().find_map(|line| {
        let value: Value = serde_json::from_str(line).ok()?;
        let error = match value.get("type").and_then(Value::as_str)? {
            "turn.failed" | "error" => &value,
            "item.completed" if value["item"]["type"] == "error" => &value["item"],
            _ => return None,
        };
        let mut diagnostic = message(error, 0)?;
        let lower = diagnostic.to_ascii_lowercase();
        if lower.contains("model") && ["not supported", "does not exist", "not found", "do not have access"].iter().any(|reason| lower.contains(reason)) {
            diagnostic.push_str(" Select an available model (or CLI default) and create a new conversation. Existing conversations keep their original model.");
        }
        Some(diagnostic)
    })
}

fn extract_companion_envelope_json(output: &str) -> Option<String> {
    fn find(value: &Value, depth: usize) -> Option<Value> {
        if depth > 12 {
            return None;
        }
        if value
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|kind| matches!(kind, "final" | "tool_calls"))
        {
            return Some(value.clone());
        }
        match value {
            Value::String(text) => parse_candidate(text, depth + 1),
            Value::Array(items) => items.iter().find_map(|item| find(item, depth + 1)),
            Value::Object(fields) => fields.values().find_map(|item| find(item, depth + 1)),
            _ => None,
        }
    }

    fn parse_candidate(text: &str, depth: usize) -> Option<Value> {
        let trimmed = text
            .trim()
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim();
        if let Ok(value) = serde_json::from_str::<Value>(trimmed)
            && let Some(found) = find(&value, depth)
        {
            return Some(found);
        }
        for (index, _) in trimmed.match_indices('{').take(256) {
            let mut values =
                serde_json::Deserializer::from_str(&trimmed[index..]).into_iter::<Value>();
            if let Some(Ok(value)) = values.next()
                && let Some(found) = find(&value, depth)
            {
                return Some(found);
            }
        }
        None
    }

    parse_candidate(output, 0).and_then(|value| serde_json::to_string(&value).ok())
}

fn cli_mode_disabled() -> bool {
    env::var("HVC_DISABLE_CLI_MODE").is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

fn deadline_timestamp(deadline: Instant) -> String {
    let remaining = deadline.saturating_duration_since(Instant::now());
    let offset = chrono::Duration::from_std(remaining).unwrap_or_default();
    Utc::now()
        .checked_add_signed(offset)
        .unwrap_or_else(Utc::now)
        .to_rfc3339()
}

fn session_fingerprint(run: &TerminalRun, workspace_snapshot_hash: &str) -> String {
    let mut digest = Sha256::new();
    for part in [
        run.project_id.as_str(),
        run.slot_id.as_str(),
        run.backend.map(CliBackend::as_str).unwrap_or_default(),
        run.model.as_deref().unwrap_or_default(),
        workspace_snapshot_hash,
    ] {
        digest.update(part.as_bytes());
        digest.update([0]);
    }
    format!("{:x}", digest.finalize())
}

fn validate_resume_parent(
    run: &TerminalRun,
    parent: &TerminalRun,
    workspace_snapshot_hash: &str,
    requested_session_id: Option<&str>,
) -> Result<(), TerminalError> {
    if parent.project_id != run.project_id
        || parent.slot_id != run.slot_id
        || parent.backend != run.backend
        || parent.model != run.model
    {
        return Err(TerminalError::Validation(
            "a sessão escolhida pertence a outro projeto, slot, backend ou modelo".into(),
        ));
    }
    if parent.status.is_active() {
        return Err(TerminalError::Validation(
            "não é possível retomar uma sessão cuja execução ainda está ativa".into(),
        ));
    }
    if parent.workspace_snapshot_hash.as_deref() != Some(workspace_snapshot_hash) {
        return Err(TerminalError::Validation(
            "o workspace mudou desde a sessão escolhida; inicie uma nova sessão".into(),
        ));
    }
    let native_session_id = parent.native_session_id.as_deref().ok_or_else(|| {
        TerminalError::Validation("a execução escolhida não possui ID de sessão nativo".into())
    })?;
    if requested_session_id != Some(native_session_id) {
        return Err(TerminalError::Validation(
            "o ID de retomada não corresponde à sessão escolhida".into(),
        ));
    }
    let expected = session_fingerprint(run, workspace_snapshot_hash);
    if parent.session_fingerprint.as_deref() != Some(expected.as_str()) {
        return Err(TerminalError::Validation(
            "o fingerprint da sessão não é compatível com esta execução".into(),
        ));
    }
    Ok(())
}

fn prepare_safe_cli_workspace(workspace: &Path) -> Result<SafeCliWorkspace, TerminalError> {
    let context = RepositoryContext::new(workspace)
        .map_err(|error| TerminalError::Security(error.to_string()))?;
    let files = context
        .safe_snapshot_files(MAX_SAFE_SNAPSHOT_FILES, MAX_SAFE_SNAPSHOT_BYTES)
        .map_err(|error| TerminalError::Security(error.to_string()))?;
    let directory = tempfile::Builder::new()
        .prefix("hvc-cli-workspace-")
        .tempdir()
        .map_err(TerminalError::Io)?;
    let mut digest = Sha256::new();
    let mut manifest_files = Vec::with_capacity(files.len());
    for file in files {
        let relative = Path::new(&file.relative_path);
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(TerminalError::Security(
                "snapshot tentou materializar path fora da raiz".into(),
            ));
        }
        digest.update(file.relative_path.as_bytes());
        digest.update([0]);
        digest.update(file.content.as_bytes());
        digest.update([0]);
        let destination = directory.path().join(relative);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&destination, file.content.as_bytes())?;
        manifest_files.push(file.relative_path);
    }
    let manifest_hash = format!("{:x}", digest.finalize());
    let manifest = serde_json::json!({
        "schemaVersion": 1,
        "manifestSha256": manifest_hash,
        "files": manifest_files,
    });
    fs::write(
        directory.path().join(".hvc-cli-snapshot.json"),
        serde_json::to_vec(&manifest)?,
    )?;
    for entry in walkdir::WalkDir::new(directory.path())
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
    {
        let mut permissions = entry
            .metadata()
            .map_err(|error| TerminalError::Security(error.to_string()))?
            .permissions();
        permissions.set_readonly(true);
        fs::set_permissions(entry.path(), permissions)?;
    }
    Ok(SafeCliWorkspace {
        directory,
        manifest_hash,
    })
}

fn secure_launch_spec(
    mut spec: CliProcessSpec,
    backend: CliBackend,
) -> Result<SecuredLaunch, TerminalError> {
    let executable = spec.executable.canonicalize().map_err(|error| {
        TerminalError::Security(format!(
            "executável CLI não pôde ser canonicalizado: {error}"
        ))
    })?;
    if !executable.is_file() {
        return Err(TerminalError::Security(
            "o launch target da CLI não é um arquivo".into(),
        ));
    }
    let extension = executable
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    #[cfg(windows)]
    if matches!(extension.as_str(), "cmd" | "bat") {
        if backend == CliBackend::Custom {
            return Err(TerminalError::Security(
                "Custom não pode executar wrappers de shell".into(),
            ));
        }
        return resolve_trusted_npm_shim(spec, backend, &executable);
    }

    if matches!(extension.as_str(), "cmd" | "bat" | "ps1" | "lnk") {
        return Err(TerminalError::Security(
            "wrapper de shell ou atalho não é um launch target permitido".into(),
        ));
    }
    let identity_sha256 = hash_launch_files(&[&executable])?;
    spec.executable = executable;
    Ok(SecuredLaunch {
        spec,
        identity_sha256,
    })
}

#[cfg(windows)]
fn resolve_trusted_npm_shim(
    mut spec: CliProcessSpec,
    backend: CliBackend,
    shim: &Path,
) -> Result<SecuredLaunch, TerminalError> {
    let metadata = shim.metadata()?;
    if metadata.len() > 64 * 1024 {
        return Err(TerminalError::Security(
            "wrapper npm excede o tamanho permitido".into(),
        ));
    }
    let shim_text = fs::read_to_string(shim)
        .map_err(|error| TerminalError::Security(format!("wrapper npm inválido: {error}")))?;
    let normalized_shim = shim_text.replace('\\', "/").to_ascii_lowercase();
    if !normalized_shim.contains("%*") {
        return Err(TerminalError::Security(
            "wrapper npm não possui o formato esperado".into(),
        ));
    }
    let shim_directory = shim
        .parent()
        .ok_or_else(|| TerminalError::Security("wrapper npm sem diretório pai".into()))?;
    let relative_candidates: &[&str] = match backend {
        CliBackend::Codex => &["node_modules/@openai/codex/bin/codex.js"],
        CliBackend::Claude => &["node_modules/@anthropic-ai/claude-code/cli.js"],
        CliBackend::Gemini => &[
            "node_modules/@google/gemini-cli/bundle/gemini.js",
            "node_modules/@google/gemini-cli/dist/index.js",
        ],
        CliBackend::Opencode => &[
            "node_modules/opencode-ai/bin/opencode.exe",
            "node_modules/opencode-ai/bin/opencode",
            "node_modules/opencode-ai/bin/opencode.js",
        ],
        CliBackend::Custom => &[],
    };
    let (entrypoint, marker) = relative_candidates
        .iter()
        .find_map(|relative| {
            let marker = relative.to_ascii_lowercase();
            if !normalized_shim.contains(&marker) {
                return None;
            }
            let path = shim_directory.join(relative.replace('/', "\\"));
            path.is_file().then_some((path, marker))
        })
        .ok_or_else(|| {
            TerminalError::Security(format!(
                "wrapper npm do {} não aponta para o pacote esperado",
                backend.label()
            ))
        })?;
    if marker.is_empty() {
        return Err(TerminalError::Security("entrypoint npm inválido".into()));
    }
    let entrypoint = entrypoint.canonicalize()?;
    let is_native_binary = entrypoint
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"));
    let (executable, identity_sha256) = if is_native_binary {
        let identity = hash_launch_files(&[&entrypoint, shim])?;
        (entrypoint, identity)
    } else {
        let node = find_node_for_shim(shim_directory)?;
        spec.args.insert(0, process_argument_path(&entrypoint));
        let identity = hash_launch_files(&[&node, &entrypoint, shim])?;
        (node, identity)
    };
    spec.executable = executable;
    Ok(SecuredLaunch {
        spec,
        identity_sha256,
    })
}

fn process_argument_path(path: &Path) -> String {
    let value = path.to_string_lossy();
    #[cfg(windows)]
    {
        if let Some(unc_path) = value.strip_prefix(r"\\?\UNC\") {
            return format!(r"\\{unc_path}");
        }
        if let Some(drive_path) = value.strip_prefix(r"\\?\") {
            return drive_path.to_string();
        }
    }
    value.into_owned()
}

#[cfg(windows)]
fn find_node_for_shim(shim_directory: &Path) -> Result<PathBuf, TerminalError> {
    let adjacent = shim_directory.join("node.exe");
    let candidate = if adjacent.is_file() {
        Some(adjacent)
    } else {
        let paths = cli_search_paths();
        find_executable("node", &paths, &native_executable_extensions())
    };
    candidate
        .ok_or_else(|| {
            TerminalError::Security("node.exe não encontrado para o wrapper npm".into())
        })?
        .canonicalize()
        .map_err(TerminalError::Io)
}

fn hash_launch_files(paths: &[&Path]) -> Result<String, TerminalError> {
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    for path in paths {
        let mut file = fs::File::open(path)?;
        loop {
            let read = std::io::Read::read(&mut file, &mut buffer)?;
            if read == 0 {
                break;
            }
            digest.update(&buffer[..read]);
        }
        digest.update([0]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn configure_hidden(command: &mut Command) {
    #[cfg(windows)]
    {
        command.creation_flags(0x0800_0000);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct CollectTerminalEvents {
        envelopes: Mutex<Vec<TerminalEventEnvelope>>,
    }

    impl TerminalEventSink for CollectTerminalEvents {
        fn emit(&self, envelope: &TerminalEventEnvelope) {
            self.envelopes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(envelope.clone());
        }
    }

    #[test]
    fn canonical_catalog_covers_every_role_once() {
        let roles = CANONICAL_AGENTS
            .iter()
            .map(|item| item.role)
            .collect::<HashSet<_>>();
        assert_eq!(roles.len(), AgentRole::ALL.len());
        assert!(AgentRole::ALL.into_iter().all(|role| roles.contains(&role)));
    }

    #[test]
    fn codex_process_is_non_interactive_read_only_and_uses_stdin() {
        let spec = build_process_spec(CliInvocationRequest {
            executable: PathBuf::from(r"C:\Tools\codex.exe"),
            backend: CliBackend::Codex,
            model: Some("gpt-test".into()),
            model_profile: None,
            thinking_level: ThinkingLevel::High,
            prompt: "segredo do prompt".into(),
            schema_file: Some(PathBuf::from(r"C:\Temp\schema.json")),
            schema: None,
            custom_args: vec![],
            resume_session_id: None,
        })
        .unwrap();
        assert_eq!(spec.executable, PathBuf::from(r"C:\Tools\codex.exe"));
        assert!(
            spec.args
                .windows(2)
                .any(|pair| pair == ["--sandbox", "read-only"])
        );
        assert!(spec.args.windows(2).any(|pair| pair == ["-a", "never"]));
        assert!(
            !spec
                .args
                .iter()
                .any(|arg| arg.contains("segredo do prompt"))
        );
        assert_eq!(spec.stdin_payload, b"segredo do prompt");
    }

    #[test]
    fn gemini_and_opencode_processes_enforce_read_only_modes() {
        let gemini = build_process_spec(CliInvocationRequest {
            executable: PathBuf::from(r"C:\Tools\gemini.exe"),
            backend: CliBackend::Gemini,
            model: None,
            model_profile: None,
            thinking_level: ThinkingLevel::Default,
            prompt: "inspect".into(),
            schema_file: None,
            schema: None,
            custom_args: vec![],
            resume_session_id: None,
        })
        .unwrap();
        assert!(
            gemini
                .args
                .windows(2)
                .any(|pair| pair == ["--approval-mode", "plan"])
        );

        let opencode = build_process_spec(CliInvocationRequest {
            executable: PathBuf::from(r"C:\Tools\opencode.exe"),
            backend: CliBackend::Opencode,
            model: None,
            model_profile: None,
            thinking_level: ThinkingLevel::Default,
            prompt: "inspect".into(),
            schema_file: None,
            schema: None,
            custom_args: vec![],
            resume_session_id: None,
        })
        .unwrap();
        assert!(opencode.args.iter().any(|arg| arg == "--pure"));
        let permissions = opencode
            .environment
            .iter()
            .find(|(key, _)| key == "OPENCODE_PERMISSION")
            .map(|(_, value)| value)
            .unwrap();
        assert!(!permissions.contains(r#""edit""#));
        assert!(permissions.contains(r#""*":"deny""#));
        assert!(permissions.contains(r#""read":"allow""#));
    }

    #[test]
    fn codex_companion_reads_nested_stdout_errors_without_exposing_other_events() {
        let error = serde_json::json!({"type":"error","status":400,"error":{
            "type":"invalid_request_error",
            "message":"The 'gpt-5.2-codex' model is not supported when using Codex with a ChatGPT account."
        }});
        let output = format!(
            "{}\n{}\n{}\n",
            serde_json::json!({"type":"item.completed","item":{"type":"reasoning","text":"private trace"}}),
            serde_json::json!({"type":"turn.failed","error":{"message":error.to_string()}}),
            serde_json::json!({"type":"item.completed","item":{"type":"agent_message","text":"internal control stream"}}),
        );
        let diagnostic = codex_companion_error(&output).unwrap();
        assert!(diagnostic.contains("gpt-5.2-codex"));
        assert!(diagnostic.contains("create a new conversation"));
        assert!(!diagnostic.contains("private trace"));
        assert!(!diagnostic.contains("internal control stream"));
        assert!(!diagnostic.contains("invalid_request_error"));
        assert!(
            codex_companion_error(
                r#"{"type":"item.completed","item":{"type":"agent_message","text":"not an error"}}"#
            )
            .is_none()
        );
        let wrapped =
            serde_json::json!({"type":"item.completed","item":{"type":"agent_message","text":
                serde_json::json!({"response":{"type":"final","text":"Ready"}}).to_string()
            }})
            .to_string();
        assert_eq!(
            serde_json::from_str::<Value>(&extract_companion_envelope_json(&wrapped).unwrap())
                .unwrap(),
            serde_json::json!({"type":"final","text":"Ready"})
        );
    }

    fn spec_for(backend: CliBackend) -> CliProcessSpec {
        let executable = match backend {
            CliBackend::Codex => r"C:\Tools\codex.exe",
            CliBackend::Claude => r"C:\Tools\claude.exe",
            CliBackend::Gemini => r"C:\Tools\gemini.exe",
            CliBackend::Opencode => r"C:\Tools\opencode.exe",
            CliBackend::Custom => r"C:\Tools\custom.exe",
        };
        build_process_spec(CliInvocationRequest {
            executable: PathBuf::from(executable),
            backend,
            model: None,
            model_profile: None,
            thinking_level: ThinkingLevel::Default,
            prompt: "Return one stage output".into(),
            schema_file: (backend == CliBackend::Codex)
                .then(|| PathBuf::from(r"C:\Temp\schema.json")),
            schema: (backend == CliBackend::Claude).then(|| serde_json::json!({})),
            custom_args: vec![],
            resume_session_id: None,
        })
        .unwrap()
    }

    /// A stage agent must keep its read tools - it legitimately inspects the
    /// workspace - while the developer's own configuration is still isolated.
    #[test]
    fn pipeline_hardening_isolates_user_configuration_but_keeps_read_tools() {
        let mut claude = spec_for(CliBackend::Claude);
        harden_spec(CliBackend::Claude, Hardening::Pipeline, &mut claude).unwrap();
        let tools = claude.args.iter().position(|arg| arg == "--tools").unwrap();
        assert_eq!(
            claude.args[tools + 1],
            "Read,Glob,Grep",
            "Pipeline must not blank the read tools the way Companion does"
        );
        assert!(claude.args.iter().any(|arg| arg == "--strict-mcp-config"));
        let sources = claude
            .args
            .iter()
            .position(|arg| arg == "--setting-sources")
            .unwrap();
        assert_eq!(claude.args[sources + 1], "");

        let mut codex = spec_for(CliBackend::Codex);
        harden_spec(CliBackend::Codex, Hardening::Pipeline, &mut codex).unwrap();
        let exec = codex.args.iter().position(|arg| arg == "exec").unwrap();
        assert_eq!(codex.args[exec + 1], "--ignore-user-config");
        assert_eq!(codex.args[exec + 2], "--ignore-rules");
        assert!(
            codex.args.iter().any(|arg| arg == "--sandbox"),
            "the adapter's read-only sandbox must survive hardening"
        );
        assert!(
            !codex.args.iter().any(|arg| arg == "--ephemeral"),
            "--ephemeral is a Companion-only concern"
        );

        // OpenCode keeps the adapter's read-only permission set, not the
        // Companion's blanket deny.
        let mut opencode = spec_for(CliBackend::Opencode);
        harden_spec(CliBackend::Opencode, Hardening::Pipeline, &mut opencode).unwrap();
        let (_, permissions) = opencode
            .environment
            .iter()
            .find(|(name, _)| name == "OPENCODE_PERMISSION")
            .unwrap();
        assert!(permissions.contains(r#""read":"allow""#));
        assert_ne!(permissions, r#"{"*":"deny"}"#);
    }

    #[test]
    fn gemini_pipeline_keeps_core_tools_while_companion_removes_them() {
        for (mode, expects_empty_core) in
            [(Hardening::Companion, true), (Hardening::Pipeline, false)]
        {
            let mut spec = spec_for(CliBackend::Gemini);
            let guard = harden_spec(CliBackend::Gemini, mode, &mut spec).unwrap();
            let (_, path) = spec
                .environment
                .iter()
                .find(|(name, _)| name == "GEMINI_CLI_SYSTEM_SETTINGS_PATH")
                .unwrap();
            let settings: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
            assert_eq!(
                settings["tools"]["core"].is_array(),
                expects_empty_core,
                "{mode:?} disagreed about core tools"
            );
            assert!(settings["mcp"]["allowed"].as_array().unwrap().is_empty());
            assert_eq!(settings["skills"]["enabled"], false);
            drop(guard);
        }
    }

    #[test]
    fn custom_is_marked_read_only_for_pipelines_and_refused_for_the_companion() {
        let mut spec = spec_for(CliBackend::Custom);
        harden_spec(CliBackend::Custom, Hardening::Pipeline, &mut spec).unwrap();
        assert!(
            spec.environment
                .iter()
                .any(|(name, value)| name == "HVC_STAGE_READ_ONLY" && value == "1")
        );

        let mut spec = spec_for(CliBackend::Custom);
        assert!(harden_spec(CliBackend::Custom, Hardening::Companion, &mut spec).is_err());
    }

    #[test]
    fn companion_hardening_disables_native_tools_and_user_configuration() {
        let cases = [
            (CliBackend::Codex, r"C:\Tools\codex.exe"),
            (CliBackend::Claude, r"C:\Tools\claude.exe"),
            (CliBackend::Gemini, r"C:\Tools\gemini.exe"),
            (CliBackend::Opencode, r"C:\Tools\opencode.exe"),
        ];
        for (backend, executable) in cases {
            let schema_file =
                (backend == CliBackend::Codex).then(|| PathBuf::from(r"C:\Temp\schema.json"));
            let schema = (backend == CliBackend::Claude).then(|| serde_json::json!({}));
            let mut spec = build_process_spec(CliInvocationRequest {
                executable: PathBuf::from(executable),
                backend,
                model: None,
                model_profile: None,
                thinking_level: ThinkingLevel::Default,
                prompt: "Return one control envelope".into(),
                schema_file,
                schema,
                custom_args: vec![],
                resume_session_id: None,
            })
            .unwrap();
            let guard = harden_spec(backend, Hardening::Companion, &mut spec).unwrap();
            match backend {
                CliBackend::Codex => {
                    let exec = spec.args.iter().position(|arg| arg == "exec").unwrap();
                    assert_eq!(spec.args[exec + 1], "--ignore-user-config");
                    assert_eq!(spec.args[exec + 2], "--ignore-rules");
                    assert_eq!(spec.args[exec + 3], "--ephemeral");
                }
                CliBackend::Claude => {
                    let tools = spec.args.iter().position(|arg| arg == "--tools").unwrap();
                    assert_eq!(spec.args[tools + 1], "");
                    assert!(spec.args.iter().any(|arg| arg == "--strict-mcp-config"));
                    assert!(
                        spec.args
                            .iter()
                            .any(|arg| arg == "--no-session-persistence")
                    );
                    assert!(
                        spec.args
                            .iter()
                            .any(|arg| arg == "--disable-slash-commands")
                    );
                }
                CliBackend::Gemini => {
                    let settings_path = spec
                        .environment
                        .iter()
                        .find(|(name, _)| name == "GEMINI_CLI_SYSTEM_SETTINGS_PATH")
                        .map(|(_, value)| value)
                        .unwrap();
                    let settings: Value =
                        serde_json::from_str(&fs::read_to_string(settings_path).unwrap()).unwrap();
                    assert_eq!(
                        settings.pointer("/tools/core"),
                        Some(&serde_json::json!([]))
                    );
                    assert_eq!(
                        settings.pointer("/mcp/allowed"),
                        Some(&serde_json::json!([]))
                    );
                    assert_eq!(
                        settings.pointer("/hooksConfig/enabled"),
                        Some(&Value::Bool(false))
                    );
                    assert!(
                        spec.args
                            .windows(2)
                            .any(|pair| pair == ["--extensions", "none"])
                    );
                    assert!(guard.is_some());
                }
                CliBackend::Opencode => {
                    let permissions = spec
                        .environment
                        .iter()
                        .find(|(name, _)| name == "OPENCODE_PERMISSION")
                        .map(|(_, value)| value)
                        .unwrap();
                    assert_eq!(permissions, r#"{"*":"deny"}"#);
                }
                CliBackend::Custom => unreachable!(),
            }
        }
    }

    #[test]
    fn model_identifiers_and_manual_profiles_fail_closed() {
        assert!(normalize_model_id(Some("provider/model:v1@fast".into())).is_ok());
        assert!(normalize_model_id(Some("provider/model with spaces".into())).is_err());
        assert!(
            validate_model_selection(
                CliBackend::Claude,
                Some("unverified-model"),
                Some("high"),
                CliModelSelectionSource::Manual,
            )
            .is_err()
        );
    }

    #[test]
    fn every_official_adapter_receives_the_requested_model_and_supported_profile() {
        for (backend, model, profile, profile_flag, profile_value) in [
            (
                CliBackend::Codex,
                "gpt-5.4",
                Some("xhigh"),
                "-c",
                "model_reasoning_effort=\"xhigh\"",
            ),
            (
                CliBackend::Claude,
                "sonnet",
                Some("high"),
                "--effort",
                "high",
            ),
            (CliBackend::Gemini, "pro", None, "", ""),
            (
                CliBackend::Opencode,
                "openai/gpt-5.4",
                Some("deep"),
                "--variant",
                "deep",
            ),
        ] {
            let spec = build_process_spec(CliInvocationRequest {
                executable: PathBuf::from(format!(r"C:\Tools\{}.exe", backend.as_str())),
                backend,
                model: Some(model.into()),
                model_profile: profile.map(str::to_string),
                thinking_level: ThinkingLevel::Default,
                prompt: "Inspect the project".into(),
                schema_file: None,
                schema: None,
                custom_args: vec![],
                resume_session_id: None,
            })
            .unwrap();
            assert!(
                spec.args.windows(2).any(|pair| pair == ["--model", model]),
                "model argument missing for {backend:?}"
            );
            if let Some(profile) = profile {
                assert!(
                    spec.args
                        .windows(2)
                        .any(|pair| pair == [profile_flag, profile_value]),
                    "profile {profile} missing for {backend:?}"
                );
            }
        }
    }

    #[test]
    fn custom_rejects_shell_scripts() {
        let error = build_process_spec(CliInvocationRequest {
            executable: PathBuf::from(r"C:\Temp\agent.cmd"),
            backend: CliBackend::Custom,
            model: None,
            model_profile: None,
            thinking_level: ThinkingLevel::Default,
            prompt: String::new(),
            schema_file: None,
            schema: None,
            custom_args: vec![],
            resume_session_id: None,
        })
        .unwrap_err();
        assert!(error.to_string().contains("shell"));
    }

    #[cfg(windows)]
    #[test]
    fn trusted_npm_shim_resolves_to_node_and_tampering_is_rejected() {
        let installation = tempfile::tempdir().unwrap();
        let node = installation.path().join("node.exe");
        fs::copy(std::env::current_exe().unwrap(), &node).unwrap();
        let entrypoint = installation
            .path()
            .join("node_modules/@openai/codex/bin/codex.js");
        fs::create_dir_all(entrypoint.parent().unwrap()).unwrap();
        fs::write(&entrypoint, "console.log('fixture');\n").unwrap();
        let shim = installation.path().join("codex.cmd");
        fs::write(
            &shim,
            "@\"%~dp0\\node.exe\" \"%~dp0\\node_modules\\@openai\\codex\\bin\\codex.js\" %*\r\n",
        )
        .unwrap();

        let original = CliProcessSpec {
            executable: shim.clone(),
            args: vec!["exec".into()],
            environment: Vec::new(),
            stdin_payload: Vec::new(),
            resume_session_id: None,
        };
        let secured = secure_launch_spec(original.clone(), CliBackend::Codex).unwrap();
        assert_eq!(secured.spec.executable, node.canonicalize().unwrap());
        assert_eq!(
            secured.spec.args.first(),
            Some(&process_argument_path(&entrypoint.canonicalize().unwrap()))
        );
        assert_eq!(secured.spec.args.get(1).map(String::as_str), Some("exec"));
        assert_eq!(secured.identity_sha256.len(), 64);

        fs::write(
            &shim,
            "@\"%~dp0\\node.exe\" \"%~dp0\\node_modules\\untrusted\\index.js\" %*\r\n",
        )
        .unwrap();
        let error = secure_launch_spec(original.clone(), CliBackend::Codex).unwrap_err();
        assert!(error.to_string().contains("pacote esperado"));

        fs::write(
            &shim,
            "@\"%~dp0\\node.exe\" \"%~dp0\\node_modules\\@openai\\codex\\bin\\codex.js\"\r\n",
        )
        .unwrap();
        let error = secure_launch_spec(original, CliBackend::Codex).unwrap_err();
        assert!(error.to_string().contains("formato esperado"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_discovery_prefers_cmd_over_posix_and_powershell_npm_shims() {
        let installation = tempfile::tempdir().unwrap();
        let extensions = windows_executable_extensions(Some(".PS1;.CMD;.EXE"));
        assert!(!extensions.iter().any(String::is_empty));
        assert!(!extensions.iter().any(|extension| extension == ".ps1"));

        for backend in ["gemini", "opencode"] {
            fs::write(installation.path().join(backend), "#!/bin/sh\nexit 0\n").unwrap();
            fs::write(
                installation.path().join(format!("{backend}.ps1")),
                "exit 0\n",
            )
            .unwrap();
            let command_shim = installation.path().join(format!("{backend}.cmd"));
            fs::write(&command_shim, "@ECHO off\r\nEXIT /b 0\r\n").unwrap();

            assert_eq!(
                find_executable(backend, &[installation.path().to_path_buf()], &extensions),
                Some(command_shim),
                "{backend} discovery must not select a POSIX or PowerShell shim"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_discovery_recovers_default_npm_path_from_gui_environment() {
        let inherited = tempfile::tempdir().unwrap();
        let app_data = tempfile::tempdir().unwrap();
        let joined_path = env::join_paths([inherited.path()]).unwrap();
        let environment = HashMap::from([
            ("PATH", joined_path),
            ("APPDATA", app_data.path().as_os_str().to_owned()),
        ]);

        let paths = cli_search_paths_from_environment(|name| environment.get(name).cloned());

        assert_eq!(paths.first(), Some(&inherited.path().to_path_buf()));
        assert!(paths.contains(&app_data.path().join("npm")));
    }

    #[cfg(windows)]
    #[test]
    fn gemini_and_opencode_npm_shims_resolve_current_entrypoints() {
        for (backend, command, relative_entrypoint) in [
            (
                CliBackend::Gemini,
                "gemini",
                "node_modules/@google/gemini-cli/bundle/gemini.js",
            ),
            (
                CliBackend::Opencode,
                "opencode",
                "node_modules/opencode-ai/bin/opencode",
            ),
        ] {
            let installation = tempfile::tempdir().unwrap();
            let node = installation.path().join("node.exe");
            fs::copy(std::env::current_exe().unwrap(), &node).unwrap();
            let entrypoint = installation.path().join(relative_entrypoint);
            fs::create_dir_all(entrypoint.parent().unwrap()).unwrap();
            fs::write(&entrypoint, "console.log('fixture');\n").unwrap();
            let shim = installation.path().join(format!("{command}.cmd"));
            fs::write(
                &shim,
                format!(
                    "@ECHO off\r\n\"%~dp0\\node.exe\" \"%~dp0\\{}\" %*\r\n",
                    relative_entrypoint.replace('/', "\\")
                ),
            )
            .unwrap();

            let secured = secure_launch_spec(
                CliProcessSpec {
                    executable: shim,
                    args: vec!["--version".into()],
                    environment: Vec::new(),
                    stdin_payload: Vec::new(),
                    resume_session_id: None,
                },
                backend,
            )
            .unwrap();

            assert_eq!(secured.spec.executable, node.canonicalize().unwrap());
            assert!(!secured.spec.args[0].starts_with(r"\\?\"));
            assert_eq!(
                secured.spec.args,
                vec![
                    process_argument_path(&entrypoint.canonicalize().unwrap()),
                    "--version".to_string(),
                ]
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn opencode_native_executable_npm_shim_resolves_directly() {
        let installation = tempfile::tempdir().unwrap();
        let entrypoint = installation
            .path()
            .join("node_modules/opencode-ai/bin/opencode.exe");
        fs::create_dir_all(entrypoint.parent().unwrap()).unwrap();
        fs::write(&entrypoint, "binary-fixture\n").unwrap();
        let shim = installation.path().join("opencode.cmd");
        fs::write(
            &shim,
            "@ECHO off\r\n\"%~dp0\\node_modules\\opencode-ai\\bin\\opencode.exe\" %*\r\n",
        )
        .unwrap();

        let secured = secure_launch_spec(
            CliProcessSpec {
                executable: shim.clone(),
                args: vec!["--version".into()],
                environment: Vec::new(),
                stdin_payload: Vec::new(),
                resume_session_id: None,
            },
            CliBackend::Opencode,
        )
        .unwrap();

        assert_eq!(secured.spec.executable, entrypoint.canonicalize().unwrap());
        assert_eq!(secured.spec.args, vec!["--version".to_string()]);
        assert_eq!(secured.identity_sha256.len(), 64);
    }

    #[test]
    fn sanitizer_removes_ansi_osc_and_control_characters() {
        let value = sanitize_terminal_text(b"ok\x1b[31m red\x1b[0m\x1b]0;title\x07\0done");
        assert_eq!(value, "ok reddone");
    }

    #[test]
    fn safe_cli_workspace_excludes_secrets_ignored_files_and_vendor_configuration() {
        let workspace = tempfile::tempdir().unwrap();
        fs::create_dir_all(workspace.path().join("src")).unwrap();
        fs::create_dir_all(workspace.path().join(".codex")).unwrap();
        fs::write(workspace.path().join("src/lib.rs"), "pub fn safe() {}\n").unwrap();
        fs::write(workspace.path().join(".env"), "TOKEN=do-not-copy\n").unwrap();
        fs::write(
            workspace.path().join(".codex/instructions.md"),
            "untrusted vendor instructions\n",
        )
        .unwrap();
        fs::write(workspace.path().join(".gitignore"), "ignored.txt\n").unwrap();
        fs::write(workspace.path().join("ignored.txt"), "ignored\n").unwrap();

        let snapshot = prepare_safe_cli_workspace(workspace.path()).unwrap();
        assert!(snapshot.path().join("src/lib.rs").is_file());
        assert!(!snapshot.path().join(".env").exists());
        assert!(!snapshot.path().join(".codex").exists());
        assert!(!snapshot.path().join("ignored.txt").exists());
        assert!(snapshot.path().join(".hvc-cli-snapshot.json").is_file());
        assert_eq!(snapshot.manifest_hash.len(), 64);
    }

    #[test]
    fn normalizes_structured_tool_events() {
        let (kind, text) = normalize_cli_frame(
            CliBackend::Gemini,
            "stdout",
            br#"{"type":"tool_result","result":"arquivo lido"}"#,
        );
        assert_eq!(kind, TerminalEventKind::ToolResult);
        assert_eq!(text, "arquivo lido");
    }

    #[test]
    fn interrupted_cli_usage_remains_partial_after_history_deletion() {
        use crate::persistence_service::{CreateProjectRequest, PersistenceService};
        use crate::usage::{UsageContext, UsageTransport, cli::CliUsageRecorder};

        let data = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let persistence = PersistenceService::initialize(data.path()).unwrap();
        let project = persistence
            .create_project(CreateProjectRequest {
                workspace_path: workspace.path().to_string_lossy().into_owned(),
                name: Some("Usage coverage".into()),
            })
            .unwrap()
            .project;
        let repository = TerminalRepository::new(persistence.database_path());
        repository
            .ensure_project_configs(&project.id, true)
            .unwrap();
        for cancelled in [true, false] {
            let run = repository
                .create_run(CreateTerminalRunRequest {
                    project_id: project.id.clone(),
                    agent_role: AgentRole::BackendAgent,
                    origin: ExecutionOrigin::Manual,
                    execution_mode: AgentExecutionMode::CliProcess,
                    backend: Some(CliBackend::Codex),
                    model: None,
                    pulsation_id: None,
                    job_id: None,
                    agent_run_id: None,
                    llm_execution_id: None,
                })
                .unwrap();
            repository
                .set_run_status(&run.id, TerminalRunStatus::Starting, None, None, None)
                .unwrap();
            repository
                .set_run_status(&run.id, TerminalRunStatus::Running, None, None, None)
                .unwrap();
            let usage = CliUsageRecorder::new(
                persistence.database_path(),
                UsageContext {
                    source_kind: "terminal_run".into(),
                    source_id: run.id.clone(),
                    pulsation_id: run.pulsation_id.clone(),
                    project_id: Some(project.id.clone()),
                    transport: UsageTransport::Cli,
                    provider_id: "codex".into(),
                    requested_model: None,
                    started_at: run.created_at.clone(),
                },
                CliBackend::Codex,
            )
            .unwrap();
            usage
                .observe_line(
                    br#"{"type":"turn.completed","usage":{"input_tokens":7,"output_tokens":3}}"#,
                )
                .unwrap();
            if cancelled {
                repository
                    .set_run_status(
                        &run.id,
                        TerminalRunStatus::Cancelled,
                        None,
                        Some("user_cancelled"),
                        None,
                    )
                    .unwrap();
            } else {
                assert_eq!(repository.recover_interrupted_runs().unwrap(), 1);
            }
        }
        repository
            .clear_history(&project.id, Some(AgentRole::BackendAgent))
            .unwrap();
        let totals = crate::usage::query_usage(persistence.database_path(), &Default::default())
            .unwrap()
            .totals;
        assert_eq!(totals.total_tokens, Some(20));
        assert_eq!(totals.partial_records, 2);
    }

    #[test]
    fn companion_stream_failure_preserves_database_error_classification() {
        let error = companion_stream_output(
            Ok(Err(std::io::Error::other(rusqlite::Error::InvalidQuery))),
            "stdout",
        )
        .unwrap_err();
        assert_eq!(error.code(), "cli_persistence_failed");
        let error = companion_stream_output(
            Ok(Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "output limit",
            ))),
            "stderr",
        )
        .unwrap_err();
        assert_eq!(error.code(), "cli_output_limit");
    }

    #[test]
    fn repository_round_trips_project_configs_runs_and_events() {
        use crate::persistence_service::{CreateProjectRequest, PersistenceService};

        let data = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let persistence = PersistenceService::initialize(data.path()).unwrap();
        let project = persistence
            .create_project(CreateProjectRequest {
                workspace_path: workspace.path().to_string_lossy().into_owned(),
                name: Some("Agents".into()),
            })
            .unwrap()
            .project;
        let repository = TerminalRepository::new(persistence.database_path());

        let configs = repository
            .ensure_project_configs(&project.id, true)
            .unwrap();
        assert_eq!(configs.len(), AgentRole::ALL.len());
        assert!(configs.iter().all(|config| config.execution_mode
            == AgentExecutionMode::CliProcess
            && config.backend == CliBackend::Codex));

        let original = repository
            .get_config(&project.id, AgentRole::BackendAgent)
            .unwrap();
        let updated = repository
            .update_config(UpdateCliAgentConfigRequest {
                project_id: project.id.clone(),
                agent_role: AgentRole::BackendAgent,
                execution_mode: AgentExecutionMode::ApiDirect,
                backend: CliBackend::Claude,
                model: Some(" test-model ".into()),
                model_profile: None,
                model_source: CliModelSelectionSource::Manual,
                thinking_level: ThinkingLevel::Medium,
                custom_executable: None,
                custom_args: vec![],
                custom_risk_ack: false,
                expected_version: original.version,
            })
            .unwrap();
        assert_eq!(updated.version, original.version + 1);
        assert_eq!(updated.model.as_deref(), Some("test-model"));
        assert_eq!(updated.model_profile, None);
        assert!(matches!(
            repository.update_config(UpdateCliAgentConfigRequest {
                expected_version: original.version,
                project_id: project.id.clone(),
                agent_role: AgentRole::BackendAgent,
                execution_mode: AgentExecutionMode::ApiDirect,
                backend: CliBackend::Claude,
                model: None,
                model_profile: None,
                model_source: CliModelSelectionSource::Default,
                thinking_level: ThinkingLevel::Default,
                custom_executable: None,
                custom_args: vec![],
                custom_risk_ack: false
            }),
            Err(TerminalError::Conflict)
        ));

        let run = repository
            .create_run(CreateTerminalRunRequest {
                project_id: project.id.clone(),
                agent_role: AgentRole::BackendAgent,
                origin: ExecutionOrigin::Manual,
                execution_mode: AgentExecutionMode::CliProcess,
                backend: Some(CliBackend::Codex),
                model: None,
                pulsation_id: None,
                job_id: None,
                agent_run_id: None,
                llm_execution_id: None,
            })
            .unwrap();
        repository
            .set_run_status(&run.id, TerminalRunStatus::Starting, None, None, None)
            .unwrap();
        repository
            .set_run_status(&run.id, TerminalRunStatus::Running, None, None, None)
            .unwrap();
        repository
            .append_event(
                &run.id,
                1,
                TerminalEventKind::Message,
                Some("stdout"),
                "\u{1b}[31msaída\u{1b}[0m",
            )
            .unwrap();
        assert_eq!(
            repository.list_events(&run.id, 0, 20).unwrap()[0].content,
            "saída"
        );
        repository
            .append_event(
                &run.id,
                2,
                TerminalEventKind::Stderr,
                Some("stderr"),
                "Authorization: Bearer fixture-secret-token",
            )
            .unwrap();
        let redacted = &repository.list_events(&run.id, 1, 20).unwrap()[0];
        assert_eq!(redacted.content, "Authorization: Bearer [REDACTED]");
        assert_eq!(redacted.redaction_count, 1);
        assert_eq!(redacted.redaction_categories, ["bearer_token"]);
        assert!(matches!(
            repository.clear_history(&project.id, Some(AgentRole::BackendAgent)),
            Err(TerminalError::Validation(_))
        ));
        assert_eq!(repository.recover_interrupted_runs().unwrap(), 1);
        assert_eq!(
            repository.get_run(&run.id).unwrap().status,
            TerminalRunStatus::Interrupted
        );
        assert!(matches!(
            repository.set_run_status(&run.id, TerminalRunStatus::Running, None, None, None),
            Err(TerminalError::InvalidTransition { .. })
        ));
        assert_eq!(
            repository
                .clear_history(&project.id, Some(AgentRole::BackendAgent))
                .unwrap(),
            1
        );
    }

    #[test]
    fn custom_identity_change_requires_a_fresh_explicit_acknowledgement() {
        use crate::persistence_service::{CreateProjectRequest, PersistenceService};

        let data = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let persistence = PersistenceService::initialize(data.path()).unwrap();
        let project = persistence
            .create_project(CreateProjectRequest {
                workspace_path: workspace.path().to_string_lossy().into_owned(),
                name: Some("Custom ack".into()),
            })
            .unwrap()
            .project;
        let repository = TerminalRepository::new(persistence.database_path());
        repository
            .ensure_project_configs(&project.id, false)
            .unwrap();
        let original = repository
            .get_config(&project.id, AgentRole::BackendAgent)
            .unwrap();
        let executable = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let error = repository
            .update_config(UpdateCliAgentConfigRequest {
                project_id: project.id.clone(),
                agent_role: AgentRole::BackendAgent,
                execution_mode: AgentExecutionMode::CliProcess,
                backend: CliBackend::Custom,
                model: None,
                model_profile: None,
                model_source: CliModelSelectionSource::Default,
                thinking_level: ThinkingLevel::Default,
                custom_executable: Some(executable.clone()),
                custom_args: Vec::new(),
                custom_risk_ack: true,
                expected_version: original.version,
            })
            .unwrap_err();
        assert!(error.to_string().contains("salvar em API Mode"));
        let staged = repository
            .update_config(UpdateCliAgentConfigRequest {
                project_id: project.id.clone(),
                agent_role: AgentRole::BackendAgent,
                execution_mode: AgentExecutionMode::ApiDirect,
                backend: CliBackend::Custom,
                model: None,
                model_profile: None,
                model_source: CliModelSelectionSource::Default,
                thinking_level: ThinkingLevel::Default,
                custom_executable: Some(executable.clone()),
                custom_args: Vec::new(),
                custom_risk_ack: true,
                expected_version: original.version,
            })
            .unwrap();
        assert!(!staged.custom_risk_ack);
        let acknowledged = repository
            .update_config(UpdateCliAgentConfigRequest {
                project_id: project.id,
                agent_role: AgentRole::BackendAgent,
                execution_mode: AgentExecutionMode::CliProcess,
                backend: CliBackend::Custom,
                model: None,
                model_profile: None,
                model_source: CliModelSelectionSource::Default,
                thinking_level: ThinkingLevel::Default,
                custom_executable: Some(executable),
                custom_args: Vec::new(),
                custom_risk_ack: true,
                expected_version: staged.version,
            })
            .unwrap();
        assert!(acknowledged.custom_risk_ack);
    }

    #[test]
    fn terminal_slots_backfill_and_free_form_crud_enforce_the_project_limit() {
        use crate::persistence_service::{CreateProjectRequest, PersistenceService};

        let data = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let persistence = PersistenceService::initialize(data.path()).unwrap();
        let project = persistence
            .create_project(CreateProjectRequest {
                workspace_path: workspace.path().to_string_lossy().into_owned(),
                name: Some("Slots".into()),
            })
            .unwrap()
            .project;
        let repository = TerminalRepository::new(persistence.database_path());
        repository
            .ensure_project_configs(&project.id, true)
            .unwrap();
        let canonical_count: u32 = rusqlite::Connection::open(persistence.database_path())
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM terminal_slots WHERE project_id=?1 AND kind='canonical'",
                [&project.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(canonical_count as usize, AgentRole::ALL.len());

        let mut slots = Vec::new();
        for index in 0..8 {
            slots.push(
                repository
                    .create_free_form_slot(CreateFreeFormSlotRequest {
                        project_id: project.id.clone(),
                        label: format!("Livre {index}"),
                    })
                    .unwrap(),
            );
        }
        assert!(matches!(
            repository.create_free_form_slot(CreateFreeFormSlotRequest {
                project_id: project.id.clone(),
                label: "Excedente".into(),
            }),
            Err(TerminalError::Validation(_))
        ));
        let mut order = slots.iter().map(|slot| slot.id.clone()).collect::<Vec<_>>();
        order.reverse();
        repository
            .reorder_free_form_slots(&project.id, &order)
            .unwrap();
        assert_eq!(
            repository
                .list_free_form_slots(&project.id, false)
                .unwrap()
                .iter()
                .map(|slot| slot.id.clone())
                .collect::<Vec<_>>(),
            order
        );
        let first = repository
            .get_slot(
                &project.id,
                &SessionSlot::FreeForm {
                    id: slots[0].id.clone(),
                },
            )
            .unwrap();
        let archived = repository
            .update_free_form_slot(UpdateFreeFormSlotRequest {
                id: first.id.clone(),
                project_id: first.project_id.clone(),
                label: first.label.clone(),
                execution_mode: first.execution_mode,
                backend: first.backend,
                model: first.model.clone(),
                model_profile: first.model_profile.clone(),
                model_source: first.model_source,
                thinking_level: first.thinking_level,
                session_policy: first.session_policy,
                custom_executable: first.custom_executable.clone(),
                custom_args: first.custom_args.clone(),
                custom_risk_ack: first.custom_risk_ack,
                position: first.position,
                archived: true,
                expected_version: first.version,
            })
            .unwrap();
        assert_eq!(
            repository
                .list_free_form_slots(&project.id, false)
                .unwrap()
                .len(),
            7
        );
        assert_eq!(
            repository
                .list_free_form_slots(&project.id, true)
                .unwrap()
                .iter()
                .filter(|slot| slot.archived)
                .count(),
            1
        );
        let restored = repository
            .update_free_form_slot(UpdateFreeFormSlotRequest {
                id: archived.id.clone(),
                project_id: archived.project_id.clone(),
                label: archived.label.clone(),
                execution_mode: archived.execution_mode,
                backend: archived.backend,
                model: archived.model.clone(),
                model_profile: archived.model_profile.clone(),
                model_source: archived.model_source,
                thinking_level: archived.thinking_level,
                session_policy: archived.session_policy,
                custom_executable: archived.custom_executable.clone(),
                custom_args: archived.custom_args.clone(),
                custom_risk_ack: archived.custom_risk_ack,
                position: archived.position,
                archived: false,
                expected_version: archived.version,
            })
            .unwrap();
        let run = repository
            .create_session_run(CreateSessionRunRequest {
                project_id: project.id.clone(),
                slot: SessionSlot::FreeForm {
                    id: restored.id.clone(),
                },
                origin: ExecutionOrigin::Manual,
                execution_mode: restored.execution_mode,
                backend: Some(restored.backend),
                model: restored.model.clone(),
                pulsation_id: None,
                job_id: None,
                agent_run_id: None,
                llm_execution_id: None,
                parent_run_id: None,
                continuation_reason: None,
            })
            .unwrap();
        assert!(matches!(
            repository.clear_slot_history(&project.id, &restored.id),
            Err(TerminalError::Validation(_))
        ));
        repository
            .set_run_status(&run.id, TerminalRunStatus::Starting, None, None, None)
            .unwrap();
        repository
            .set_run_status(&run.id, TerminalRunStatus::Running, None, None, None)
            .unwrap();
        repository
            .set_run_status(&run.id, TerminalRunStatus::Completed, Some(0), None, None)
            .unwrap();
        for sequence in 1..=501 {
            repository
                .append_event(
                    &run.id,
                    sequence,
                    TerminalEventKind::Stdout,
                    Some("stdout"),
                    "linha",
                )
                .unwrap();
        }
        let export = repository.export_run_history(&project.id, &run.id).unwrap();
        let payload: Value = serde_json::from_slice(&fs::read(&export).unwrap()).unwrap();
        assert_eq!(payload["events"].as_array().unwrap().len(), 501);
        let second_export = repository.export_run_history(&project.id, &run.id).unwrap();
        assert_eq!(second_export, export);
        assert_eq!(
            repository
                .clear_slot_history(&project.id, &restored.id)
                .unwrap(),
            1
        );
        repository
            .delete_free_form_slot(&project.id, &restored.id, restored.version)
            .unwrap();
        assert_eq!(
            repository
                .list_free_form_slots(&project.id, false)
                .unwrap()
                .len(),
            7
        );
    }

    #[test]
    fn explicit_resume_fencing_rejects_workspace_backend_model_and_session_changes() {
        use crate::persistence_service::{CreateProjectRequest, PersistenceService};

        let data = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let persistence = PersistenceService::initialize(data.path()).unwrap();
        let project = persistence
            .create_project(CreateProjectRequest {
                workspace_path: workspace.path().to_string_lossy().into_owned(),
                name: Some("Resume".into()),
            })
            .unwrap()
            .project;
        let repository = TerminalRepository::new(persistence.database_path());
        repository
            .ensure_project_configs(&project.id, true)
            .unwrap();
        let parent = repository
            .create_run(CreateTerminalRunRequest {
                project_id: project.id.clone(),
                agent_role: AgentRole::BackendAgent,
                origin: ExecutionOrigin::Manual,
                execution_mode: AgentExecutionMode::CliProcess,
                backend: Some(CliBackend::Codex),
                model: Some("fixture-model".into()),
                pulsation_id: None,
                job_id: None,
                agent_run_id: None,
                llm_execution_id: None,
            })
            .unwrap();
        let fingerprint = session_fingerprint(&parent, "workspace-a");
        repository
            .set_workspace_snapshot_hash(&parent.id, "workspace-a")
            .unwrap();
        repository
            .set_session_fingerprint(&parent.id, &fingerprint)
            .unwrap();
        repository
            .set_native_session_id(&parent.id, "session-a")
            .unwrap();
        repository
            .set_run_status(&parent.id, TerminalRunStatus::Starting, None, None, None)
            .unwrap();
        repository
            .set_run_status(&parent.id, TerminalRunStatus::Running, None, None, None)
            .unwrap();
        let parent = repository
            .set_run_status(
                &parent.id,
                TerminalRunStatus::Completed,
                Some(0),
                None,
                None,
            )
            .unwrap();
        let child = TerminalRun {
            parent_run_id: Some(parent.id.clone()),
            ..parent.clone()
        };
        validate_resume_parent(&child, &parent, "workspace-a", Some("session-a")).unwrap();
        assert!(validate_resume_parent(&child, &parent, "workspace-b", Some("session-a")).is_err());
        assert!(validate_resume_parent(&child, &parent, "workspace-a", Some("session-b")).is_err());
        let mut changed = child.clone();
        changed.model = Some("other-model".into());
        assert!(
            validate_resume_parent(&changed, &parent, "workspace-a", Some("session-a")).is_err()
        );
        changed = child;
        changed.backend = Some(CliBackend::Claude);
        assert!(
            validate_resume_parent(&changed, &parent, "workspace-a", Some("session-a")).is_err()
        );
    }

    #[tokio::test]
    async fn probe_reader_aborts_when_output_exceeds_the_bound() {
        let abort = CancellationToken::new();
        let error = read_bounded_probe_stream(
            std::io::Cursor::new(vec![b'x'; MAX_PROBE_OUTPUT_BYTES + 1]),
            abort.clone(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(abort.is_cancelled());
    }

    #[tokio::test]
    async fn event_coordinator_persists_one_monotonic_stream_with_run_snapshots() {
        use crate::persistence_service::{CreateProjectRequest, PersistenceService};

        let data = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let persistence = PersistenceService::initialize(data.path()).unwrap();
        let project = persistence
            .create_project(CreateProjectRequest {
                workspace_path: workspace.path().to_string_lossy().into_owned(),
                name: Some("Agents stream".into()),
            })
            .unwrap()
            .project;
        let repository = TerminalRepository::new(persistence.database_path());
        repository
            .ensure_project_configs(&project.id, true)
            .unwrap();
        let run = repository
            .create_run(CreateTerminalRunRequest {
                project_id: project.id,
                agent_role: AgentRole::BackendAgent,
                origin: ExecutionOrigin::Manual,
                execution_mode: AgentExecutionMode::CliProcess,
                backend: Some(CliBackend::Codex),
                model: None,
                pulsation_id: None,
                job_id: None,
                agent_run_id: None,
                llm_execution_id: None,
            })
            .unwrap();
        repository
            .set_run_status(&run.id, TerminalRunStatus::Starting, None, None, None)
            .unwrap();
        let run = repository
            .set_run_status(&run.id, TerminalRunStatus::Running, None, None, None)
            .unwrap();
        let run_id = run.id.clone();
        let collected = Arc::new(CollectTerminalEvents::default());
        let sink: Arc<dyn TerminalEventSink> = collected.clone();
        let (sender, receiver) = mpsc::channel(PROCESS_EVENT_CHANNEL_CAPACITY);
        sender
            .send(ProcessStreamItem::Frame {
                stream: "stdout",
                bytes: b"first\n".to_vec(),
            })
            .await
            .unwrap();
        sender
            .send(ProcessStreamItem::Frame {
                stream: "stderr",
                bytes: b"warning\n".to_vec(),
            })
            .await
            .unwrap();
        drop(sender);

        let output = coordinate_process_events(
            repository.clone(),
            sink,
            run,
            CliBackend::Codex,
            receiver,
            CancellationToken::new(),
            Arc::new(Mutex::new(None)),
            ProgressWatchdog::new(AgentTimeoutPolicy::default()),
        )
        .await
        .unwrap();
        assert_eq!(output.raw_output, "first\n");
        assert_eq!(output.last_stderr, "warning\n");
        let events = repository.list_events(&run_id, 0, 20).unwrap();
        assert_eq!(
            events
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        let persisted_run = repository.get_run(&run_id).unwrap();
        assert_eq!(persisted_run.event_count, 2);
        assert_eq!(persisted_run.output_bytes, 14);
        let envelopes = collected
            .envelopes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(
            envelopes
                .iter()
                .map(|envelope| envelope.sequence)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert!(
            envelopes
                .iter()
                .all(|envelope| envelope.protocol_version == 2
                    && envelope.run.status == TerminalRunStatus::Running)
        );
    }

    #[tokio::test]
    async fn coordinate_process_events_does_not_overwrite_raw_output_with_non_text_completed_frame()
    {
        use crate::persistence_service::{CreateProjectRequest, PersistenceService};

        let data = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let persistence = PersistenceService::initialize(data.path()).unwrap();
        let project = persistence
            .create_project(CreateProjectRequest {
                workspace_path: workspace.path().to_string_lossy().into_owned(),
                name: Some("Agents stream non-text".into()),
            })
            .unwrap()
            .project;
        let repository = TerminalRepository::new(persistence.database_path());
        repository
            .ensure_project_configs(&project.id, true)
            .unwrap();
        let run = repository
            .create_run(CreateTerminalRunRequest {
                project_id: project.id,
                agent_role: AgentRole::BackendAgent,
                origin: ExecutionOrigin::Manual,
                execution_mode: AgentExecutionMode::CliProcess,
                backend: Some(CliBackend::Codex),
                model: None,
                pulsation_id: None,
                job_id: None,
                agent_run_id: None,
                llm_execution_id: None,
            })
            .unwrap();
        repository
            .set_run_status(&run.id, TerminalRunStatus::Starting, None, None, None)
            .unwrap();
        let run = repository
            .set_run_status(&run.id, TerminalRunStatus::Running, None, None, None)
            .unwrap();
        let collected = Arc::new(CollectTerminalEvents::default());
        let sink: Arc<dyn TerminalEventSink> = collected.clone();
        let (sender, receiver) = mpsc::channel(PROCESS_EVENT_CHANNEL_CAPACITY);
        sender
            .send(ProcessStreamItem::Frame {
                stream: "stdout",
                bytes: br#"{"type":"item.completed","item":{"type":"message","text":"{\"outputLanguage\":\"en\",\"message\":\"ok\"}"}}"#.to_vec(),
            })
            .await
            .unwrap();
        sender
            .send(ProcessStreamItem::Frame {
                stream: "stdout",
                bytes: br#"{"type":"turn.completed","usage":{"input_tokens":100}}"#.to_vec(),
            })
            .await
            .unwrap();
        drop(sender);

        let output = coordinate_process_events(
            repository.clone(),
            sink,
            run,
            CliBackend::Codex,
            receiver,
            CancellationToken::new(),
            Arc::new(Mutex::new(None)),
            ProgressWatchdog::new(AgentTimeoutPolicy::default()),
        )
        .await
        .unwrap();
        assert_eq!(
            output.raw_output,
            "{\"outputLanguage\":\"en\",\"message\":\"ok\"}"
        );
    }
}
