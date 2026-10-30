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
use crate::host::{
    LlmRequest, MAX_REDACTION_SPANS, PreparedModelInput, RedactionResult,
    SANITIZATION_POLICY_VERSION, TerminalHost,
};
use hvc_contracts::{AgentRole, ResponseFormat, UnitContextScope};

mod adapters;
mod codex_model_provenance;
mod model_catalog;
pub mod model_identity;
mod observation_report;
mod plan_usage;

pub use model_catalog::{
    CliModelCatalogEntry, CliModelCatalogService, CliModelCatalogSnapshot, CliModelCatalogSource,
    CliModelCatalogState, CliModelSelectionSource, cli_model_catalog_version,
};
pub use model_catalog::{validate_catalog_effort, validate_catalog_model_selection};
pub use observation_report::{
    CliObservationGroup, CliObservationRun, ObservationOutcome, PulsationTimeBreakdown,
};
pub(crate) use plan_usage::read_codex_app_server_usage;
pub use plan_usage::{CliConnectionStatus, CliConnectionsSnapshot};

#[cfg(windows)]
#[must_use = "keep this guard alive until the complete child process tree has exited"]
pub struct ProcessTreeJob {
    handle: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
// SAFETY: the guard exclusively owns its valid Windows job handle; ownership may move
// between threads because the Windows job APIs impose no thread affinity.
unsafe impl Send for ProcessTreeJob {}

#[cfg(windows)]
// SAFETY: shared methods only query or signal the thread-safe Windows job object.
// The handle remains owned until the final guard is dropped.
unsafe impl Sync for ProcessTreeJob {}

#[cfg(windows)]
impl ProcessTreeJob {
    pub fn assign(child: &tokio::process::Child) -> std::io::Result<Self> {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };

        let process = child
            .raw_handle()
            .ok_or_else(|| std::io::Error::other("CLI process handle unavailable"))?
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

    pub fn terminate(&self) {
        // SAFETY: `handle` remains owned by this guard until Drop.
        unsafe {
            windows_sys::Win32::System::JobObjects::TerminateJobObject(self.handle, 1);
        }
    }

    fn observation(&self) -> std::io::Result<(u64, u64, u64, u64)> {
        use windows_sys::Win32::System::JobObjects::{
            JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
            QueryInformationJobObject,
        };
        // SAFETY: the guard owns a live job handle. Each output pointer refers to an
        // initialized structure of the size passed to the Windows query API.
        unsafe {
            let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
            if QueryInformationJobObject(
                self.handle,
                JobObjectBasicAccountingInformation,
                (&raw mut accounting).cast(),
                std::mem::size_of_val(&accounting) as u32,
                std::ptr::null_mut(),
            ) == 0
            {
                return Err(std::io::Error::last_os_error());
            }
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            if QueryInformationJobObject(
                self.handle,
                JobObjectExtendedLimitInformation,
                (&raw mut limits).cast(),
                std::mem::size_of_val(&limits) as u32,
                std::ptr::null_mut(),
            ) == 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok((
                accounting.TotalUserTime.max(0) as u64,
                accounting.TotalKernelTime.max(0) as u64,
                limits.PeakProcessMemoryUsed as u64,
                limits.PeakJobMemoryUsed as u64,
            ))
        }
    }
}

#[cfg(windows)]
impl Drop for ProcessTreeJob {
    fn drop(&mut self) {
        // KILL_ON_JOB_CLOSE makes this a final safety net for grandchildren as well.
        // SAFETY: this guard exclusively owns the non-null handle produced by assign;
        // construction failures close their handles before returning without a guard.
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

#[cfg(not(windows))]
#[must_use = "keep this guard alive until the child process has exited"]
pub struct ProcessTreeJob;

#[cfg(not(windows))]
impl ProcessTreeJob {
    pub fn assign(_child: &tokio::process::Child) -> std::io::Result<Self> {
        Ok(Self)
    }

    pub fn terminate(&self) {}

    fn observation(&self) -> std::io::Result<(u64, u64, u64, u64)> {
        Ok((0, 0, 0, 0))
    }
}

/// Lowercase hexadecimal of a digest. sha2 0.11 digests no longer implement `LowerHex`.
pub(crate) fn lower_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

const MAX_MODEL_CHARS: usize = 200;
const MAX_CUSTOM_ARGS: usize = 64;
const MAX_CUSTOM_ARG_CHARS: usize = 1_024;
const MAX_EVENT_CHARS: usize = 64 * 1_024;
const MAX_PENDING_STREAM_BYTES: usize = MAX_RUN_OUTPUT_BYTES;
const MAX_RUN_OUTPUT_BYTES: usize = 32 * 1024 * 1024;
/// How much of the CLI prompt is kept on the agent run. Without it the stored
/// request is only the transport mode, so a rejected generation cannot be
/// reproduced from the record. Matches the context character budget.
const CLI_REQUEST_PROMPT_MAX_CHARS: usize = 64_000;
const MAX_RUN_EVENTS: usize = 100_000;
/// Least a CLI call may spend before the host stops it, whatever it asked for.
///
/// A CLI run is an agent session, not one completion: its output tokens add up
/// the thinking and every turn of the tool loop, so a call that asked for 1,600
/// tokens routinely and legitimately spends three times that. Of the 99
/// pipeline CLI runs recorded by 2026-09, all but one spent 23,175 tokens or
/// fewer (16,701 for calls asking for 12,000 or less, accepted or rejected);
/// the 72 learning runs peaked at 12,826. The floor sits above all of them.
const CLI_OUTPUT_CEILING_FLOOR: u64 = 24_000;
/// How many times a call's requested output a CLI run may spend. Two mirrors
/// the stage account, whose extra pool is as large as its initial one.
const CLI_OUTPUT_CEILING_FACTOR: u64 = 2;

/// Output tokens a CLI process may produce for a call that reserved
/// `max_output_tokens`, counting everything the session emits. Over it the host
/// stops the process instead of paying for a run that will not converge.
///
/// "Haiku CLI teste 1" (2026-09-26) printed the same 9,000-token test file five
/// times before calling the structured-output tool, spent 44,354 tokens
/// against a 16,000 reservation, and was rejected afterwards for missing
/// evidence. Nothing had been enforcing the reservation: the HTTP adapters
/// send it to the provider as `max_tokens`, but the CLI has no such flag.
pub fn cli_output_ceiling(max_output_tokens: u32) -> u64 {
    u64::from(max_output_tokens)
        .saturating_mul(CLI_OUTPUT_CEILING_FACTOR)
        .max(CLI_OUTPUT_CEILING_FLOOR)
}

const PROCESS_EVENT_CHANNEL_CAPACITY: usize = 256;
const DEFAULT_MANUAL_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const MAX_SAFE_SNAPSHOT_FILES: usize = 10_000;
const MAX_SAFE_SNAPSHOT_BYTES: u64 = 64 * 1024 * 1024;
const CLI_DISCOVERY_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const CLI_VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const CLI_AUTH_PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const CLI_SLOW_AUTH_PROBE_TIMEOUT: Duration = Duration::from_secs(45);
const MAX_PROBE_OUTPUT_BYTES: usize = 128 * 1024;
const MAX_MODEL_CATALOG_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_COMPANION_CLI_OUTPUT_BYTES: usize = 1024 * 1024;
/// A contract probe launches the CLI for real (Gemini also self-updates on launch), so it gets
/// far more time than a `--version` probe.
const CLI_VALIDATION_TIMEOUT: Duration = Duration::from_secs(180);
const CLI_VALIDATION_PROMPT: &str = "Reply with exactly the word OK and nothing else.";
/// `runtime_settings` key prefix under which a contract-validated CLI version is remembered.
const VALIDATED_CLI_VERSION_KEY_PREFIX: &str = "cli.validated_version.";

#[derive(Debug, thiserror::Error)]
pub enum TerminalError {
    #[error("invalid Agents configuration: {0}")]
    Validation(String),
    #[error("Agents record not found: {0}")]
    NotFound(String),
    #[error("the Agents configuration was changed by another operation")]
    Conflict,
    #[error("local Agents database failure: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("I/O failure in the CLI runtime: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid JSON in the CLI runtime: {0}")]
    Json(#[from] serde_json::Error),
    #[error("CLI run failed: {0}")]
    Process(String),
    #[error("CLI run timed out")]
    Timeout,
    /// The process went quiet past its silence window rather than running out
    /// of total time. Separate because the remedy differs: the ceiling means
    /// the work is too big, silence usually means the model is reasoning and
    /// the window is too narrow for it.
    #[error("CLI run was idle past the limit without producing output")]
    InactivityTimeout,
    #[error("CLI run exceeded the output limit")]
    OutputLimit,
    /// The host stopped the process because the model had already produced
    /// more output tokens than the attempt's ceiling allows (see
    /// [`cli_output_ceiling`]). Unlike `OutputTruncated` nothing cut the model
    /// off: it kept going, and every further token would have been billed.
    /// Repeating the prompt against the same ceiling spends it again.
    #[error(
        "the CLI was stopped after producing {observed} output tokens, past the {ceiling}-token ceiling for this call"
    )]
    OutputBudgetExceeded { observed: u64, ceiling: u64 },
    #[error("model_mismatch: modelo esperado `{expected}`, observado `{observed}`")]
    ModelMismatch { expected: String, observed: String },
    #[error(
        "model_unverified: the model that produced this generation could not be verified ({detail})"
    )]
    ModelUnverified { detail: String },
    /// The model itself ran out of output budget mid-answer. Distinct from
    /// `OutputLimit` (the host's own byte ceiling on a run) and from `Process`
    /// (the CLI failed): the process succeeded and the answer is simply
    /// incomplete, so retrying the same prompt against the same budget
    /// truncates again at the same place.
    #[error(
        "{backend} stopped its output at the token limit ({reason}) after {produced_chars} characters"
    )]
    OutputTruncated {
        backend: String,
        reason: String,
        produced_chars: usize,
    },
    #[error("CLI run was cancelled")]
    Cancelled,
    #[error("{0} requires authentication")]
    AuthRequired(String),
    #[error("{0} version is incompatible with the CLI contract")]
    VersionUnsupported(String),
    #[error("{0} hit a temporary request limit")]
    RateLimited(String),
    #[error("invalid CLI run transition: {from} -> {to}")]
    InvalidTransition { from: String, to: String },
    #[error("failed to redact secrets from CLI output: {0}")]
    Redaction(String),
    #[error("CLI runtime security failure: {0}")]
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
            Self::InactivityTimeout => "cli_inactivity_timeout",
            Self::OutputLimit => "cli_output_limit",
            Self::OutputBudgetExceeded { .. } => "cli_output_budget_exceeded",
            Self::ModelMismatch { .. } => "model_mismatch",
            Self::ModelUnverified { .. } => "model_unverified",
            Self::OutputTruncated { .. } => "cli_output_truncated",
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
    Antigravity,
    Custom,
}

impl CliBackend {
    /// Backends whose self-updates are gated behind an explicit validation when a pulsation
    /// would run a version above the adapter's validated ceiling: Gemini and Antigravity both
    /// update themselves on launch, and Gemini's update cadence is what kept breaking
    /// pulsations. The other CLIs run newer versions with an advisory only (see
    /// `apply_version_verdict`).
    pub const fn gates_unvalidated_versions(self) -> bool {
        matches!(self, Self::Gemini | Self::Antigravity)
    }

    pub const ALL: [Self; 6] = [
        Self::Codex,
        Self::Claude,
        Self::Gemini,
        Self::Opencode,
        Self::Antigravity,
        Self::Custom,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Gemini => "gemini",
            Self::Opencode => "opencode",
            Self::Antigravity => "antigravity",
            Self::Custom => "custom",
        }
    }

    /// The file stem discovery looks for on PATH. It equals the backend id except for
    /// Antigravity: its CLI ships as `agy`, while `antigravity.cmd` launches the IDE.
    pub const fn executable_name(self) -> &'static str {
        match self {
            Self::Antigravity => "agy",
            _ => self.as_str(),
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude",
            Self::Gemini => "Gemini",
            Self::Opencode => "OpenCode",
            Self::Antigravity => "Antigravity",
            Self::Custom => "Custom",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "codex" => Some(Self::Codex),
            "claude" => Some(Self::Claude),
            "gemini" => Some(Self::Gemini),
            "opencode" => Some(Self::Opencode),
            "antigravity" => Some(Self::Antigravity),
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

    pub fn parse(value: &str) -> Option<Self> {
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
    /// The Visualizer's notes on a checkpoint's Review Brief (ADR-027 M5).
    Review,
    /// The Notes Scribe structuring or refining a Companion note (ADR-029).
    Notes,
    /// The Documentation Reconciler proposing a note update (ADR-034 M5).
    Reconciliation,
}

impl ExecutionOrigin {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Feature => "feature",
            Self::Pipeline => "pipeline",
            Self::Learning => "learning",
            Self::Manual => "manual",
            Self::Review => "review",
            Self::Notes => "notes",
            Self::Reconciliation => "reconciliation",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "feature" => Some(Self::Feature),
            "pipeline" => Some(Self::Pipeline),
            "learning" => Some(Self::Learning),
            "manual" => Some(Self::Manual),
            "review" => Some(Self::Review),
            "notes" => Some(Self::Notes),
            "reconciliation" => Some(Self::Reconciliation),
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

pub const CANONICAL_AGENTS: [CanonicalAgentDescriptor; 11] = [
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
    CanonicalAgentDescriptor {
        role: AgentRole::Visualizer,
        name: "Visualizer",
        glyph: "~",
        stage: "Review",
        color_key: "violet",
    },
    CanonicalAgentDescriptor {
        role: AgentRole::NotesScribe,
        name: "NotesScribe",
        glyph: "=",
        stage: "Notes",
        color_key: "orange",
    },
    CanonicalAgentDescriptor {
        role: AgentRole::DocumentationReconciler,
        name: "DocumentationReconciler",
        glyph: "%",
        stage: "Reconciliation",
        color_key: "teal",
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
    Canonical {
        agent_role: AgentRole,
    },
    /// A non-user-visible slot bound to one composed stage execution/work unit.
    /// Its configuration is copied from the canonical role when it is created.
    Ephemeral {
        id: String,
        agent_role: AgentRole,
    },
    FreeForm {
        id: String,
    },
}

impl SessionSlot {
    fn persisted_id(&self, project_id: &str) -> String {
        match self {
            Self::Canonical { agent_role } => canonical_slot_id(project_id, *agent_role),
            Self::Ephemeral { id, .. } => id.clone(),
            Self::FreeForm { id } => id.clone(),
        }
    }

    pub fn agent_role(&self) -> Option<AgentRole> {
        match self {
            Self::Canonical { agent_role } => Some(*agent_role),
            Self::Ephemeral { agent_role, .. } => Some(*agent_role),
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

/// A composed-stage specialty's optional configuration override (ADR-022).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpecialtyOverride {
    pub execution_mode: AgentExecutionMode,
    pub backend: Option<CliBackend>,
    pub model: Option<String>,
    pub thinking_level: ThinkingLevel,
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
    /// Set only for the bounded set of automatic Pulsation CLI attempts that
    /// deliberately run without a wall-clock watchdog.
    pub observation_trial_id: Option<String>,
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
    pub observation_trial_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CliObservationTrial {
    pub id: String,
    pub project_id: String,
    pub state: String,
    pub target_runs: u32,
    pub admitted_runs: u32,
    pub settled_runs: u32,
    pub started_at: String,
    pub closed_at: Option<String>,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CliObservationSample {
    pub observed_at: String,
    pub elapsed_ms: u64,
    pub user_cpu_100ns: Option<u64>,
    pub kernel_cpu_100ns: Option<u64>,
    pub peak_process_memory_bytes: Option<u64>,
    pub peak_job_memory_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CliObservationReport {
    pub trial: CliObservationTrial,
    /// Runs whose output the stage (or Learning) accepted; the only ones in
    /// the duration statistics below.
    pub successful_runs: u32,
    /// Exited normally but the output was rejected (e.g. `outcome: failed`).
    pub stage_rejected_runs: u32,
    pub failed_runs: u32,
    pub cancelled_runs: u32,
    pub active_runs: u32,
    pub mean_success_duration_ms: Option<u64>,
    pub median_success_duration_ms: Option<u64>,
    pub p95_success_duration_ms: Option<u64>,
    pub max_success_duration_ms: Option<u64>,
    /// Longest interval between process frames, including start-to-first-frame
    /// and last-frame-to-termination. Active runs are censored at the last
    /// observed frame rather than projected into duration statistics.
    pub max_frame_silence_ms: Option<u64>,
    /// Longest interval between two reads of the CLI's pipes - the signal the
    /// silence watchdog acts on outside the trial.
    pub max_transport_silence_ms: Option<u64>,
    pub peak_process_memory_bytes: Option<u64>,
    pub peak_job_memory_bytes: Option<u64>,
    /// `debug` builds inflate host-side timings (hashing, snapshots).
    pub build_profile: String,
    pub groups: Vec<CliObservationGroup>,
    /// Critical-path breakdown of every Pulsation that has a trial run.
    pub pulsations: Vec<PulsationTimeBreakdown>,
    pub runs: Vec<CliObservationRun>,
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
    host: Arc<dyn TerminalHost>,
    db_path: PathBuf,
    /// Receives Claude's `rate_limit_event` frames from the run coordinator (ADR-028).
    quota: Option<Arc<crate::quota::QuotaMeter>>,
}

impl TerminalRepository {
    pub fn new(db_path: impl Into<PathBuf>, host: Arc<dyn TerminalHost>) -> Self {
        Self {
            host,
            db_path: db_path.into(),
            quota: None,
        }
    }

    fn with_quota_meter(mut self, quota: Arc<crate::quota::QuotaMeter>) -> Self {
        self.quota = Some(quota);
        self
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
            return Err(TerminalError::NotFound("active project".into()));
        }
        let now = Utc::now().to_rfc3339();
        let default_mode = if codex_ready {
            "cli_process"
        } else {
            "api_direct"
        };
        let transaction = connection.transaction()?;
        for (position, role) in AgentRole::ALL.into_iter().enumerate() {
            if matches!(
                role,
                AgentRole::NotesScribe | AgentRole::DocumentationReconciler
            ) {
                // A first note should not fail on a route nobody chose: start from the
                // Feature Agent's, which already serves this project's one-shot planning
                // calls. A Custom route is not inherited, because the Scribe refuses it
                // (the Documentation Reconciler, ADR-034 M5, too).
                transaction.execute(
                    "INSERT OR IGNORE INTO cli_agent_configs(
                       project_id,agent_role,execution_mode,backend,model,thinking_level,
                       custom_args_json,custom_risk_ack,version,created_at,updated_at,
                       model_profile,model_source
                     )
                     SELECT project_id,?2,execution_mode,backend,model,thinking_level,
                            '[]',0,1,?3,?3,model_profile,model_source
                     FROM cli_agent_configs
                     WHERE project_id=?1 AND agent_role='feature_agent' AND backend<>'custom'",
                    params![project_id, role.as_str(), now],
                )?;
            }
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
        validate_catalog_effort(
            &self.connection()?,
            &request.project_id,
            request.backend,
            request.model.as_deref(),
            request.model_profile.as_deref(),
            request.thinking_level,
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
                    "changing the Custom executable or arguments requires saving in API Mode and confirming the new risk"
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
                TerminalError::NotFound("agent configuration".into())
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
            .ok_or_else(|| TerminalError::NotFound("agent configuration".into()))
    }

    /// Materializes a hidden work-unit slot from its canonical role. The
    /// identity is caller-supplied and deterministic, making retries and
    /// restart recovery reuse only their own terminal history.
    pub fn ensure_ephemeral_slot(
        &self,
        project_id: &str,
        id: &str,
        agent_role: AgentRole,
    ) -> Result<(), TerminalError> {
        let project_id = required(project_id, "projectId")?;
        let id = required(id, "slotId")?;
        let now = Utc::now().to_rfc3339();
        let changed = self.connection()?.execute(
            "INSERT OR IGNORE INTO terminal_slots(
               id,project_id,kind,agent_role,label,execution_mode,backend,model,
               thinking_level,session_policy,custom_executable,custom_args_json,
               custom_risk_ack,position,archived,version,created_at,updated_at,
               model_profile,model_source
             )
             SELECT ?1,project_id,'ephemeral',agent_role,?2,execution_mode,backend,model,
                    thinking_level,'fresh_per_run',custom_executable,custom_args_json,
                    custom_risk_ack,position,0,version,?3,?3,model_profile,model_source
             FROM terminal_slots
             WHERE id=?4 AND project_id=?5 AND kind='canonical' AND agent_role=?6",
            params![
                id,
                format!("Execution {id}"),
                now,
                canonical_slot_id(project_id, agent_role),
                project_id,
                agent_role.as_str(),
            ],
        )?;
        if changed == 0 {
            let exists: bool = self.connection()?.query_row(
                "SELECT EXISTS(SELECT 1 FROM terminal_slots
                 WHERE id=?1 AND project_id=?2 AND kind='ephemeral' AND agent_role=?3)",
                params![id, project_id, agent_role.as_str()],
                |row| row.get(0),
            )?;
            if !exists {
                return Err(TerminalError::NotFound("canonical agent slot".into()));
            }
        }
        Ok(())
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
                "a project accepts at most 8 active free-form agents".into(),
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
        validate_catalog_effort(
            &self.connection()?,
            &request.project_id,
            request.backend,
            request.model.as_deref(),
            request.model_profile.as_deref(),
            request.thinking_level,
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
                    "changing the Custom executable or arguments requires saving in API Mode and confirming the new risk"
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
                    "cancel the active run before archiving the free-form agent".into(),
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
                    "a project accepts at most 8 active free-form agents".into(),
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
                "cancel the active run before deleting the free-form agent".into(),
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
                "the order contains duplicate slots".into(),
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
            .ok_or_else(|| TerminalError::NotFound("project workspace".into()))
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
            observation_trial_id: None,
        })
    }

    pub fn create_session_run(
        &self,
        request: CreateSessionRunRequest,
    ) -> Result<TerminalRun, TerminalError> {
        required(&request.project_id, "projectId")?;
        if request.execution_mode == AgentExecutionMode::CliProcess && request.backend.is_none() {
            return Err(TerminalError::Validation(
                "a backend is required for a CLI run".into(),
            ));
        }
        let slot = self.get_slot(&request.project_id, &request.slot)?;
        if slot.archived {
            return Err(TerminalError::Validation(
                "the terminal slot is archived".into(),
            ));
        }
        if request.origin != ExecutionOrigin::Manual && slot.kind == "free_form" {
            return Err(TerminalError::Validation(
                "free-form agents accept manual runs only".into(),
            ));
        }
        if request.origin == ExecutionOrigin::Manual && slot.kind == "ephemeral" {
            return Err(TerminalError::Validation(
                "internal run slots do not accept manual actions".into(),
            ));
        }
        let continuation_reason = request
            .continuation_reason
            .as_deref()
            .map(|value| match value {
                "explicit_resume" | "resume_per_pulsation" | "restart_recovery" => Ok(value),
                _ => Err(TerminalError::Validation(
                    "invalid continuation reason".into(),
                )),
            })
            .transpose()?;
        if request.parent_run_id.is_some() != continuation_reason.is_some() {
            return Err(TerminalError::Validation(
                "resuming requires a parent run and a continuation reason".into(),
            ));
        }
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let slot_id = request.slot.persisted_id(&request.project_id);
        let agent_role = request.slot.agent_role();
        let (slot_kind, free_form_slot_id, free_form_label) = match &request.slot {
            SessionSlot::Canonical { .. } => ("canonical", None, None),
            SessionSlot::Ephemeral { .. } => ("ephemeral", None, None),
            SessionSlot::FreeForm { id } => {
                ("free_form", Some(id.as_str()), Some(slot.label.as_str()))
            }
        };
        let mut connection = self.connection()?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if let (Some(pulsation_id), Some(role)) = (request.pulsation_id.as_deref(), agent_role) {
            let frozen: Option<(Option<String>, Option<String>)> = transaction
                .query_row(
                    "SELECT backend,model FROM terminal_runs
                     WHERE project_id=?1 AND pulsation_id=?2 AND agent_role=?3 AND execution_mode='cli_process'
                     ORDER BY created_at,id LIMIT 1",
                    params![request.project_id, pulsation_id, role.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if let Some((frozen_backend, frozen_model)) = frozen
                && (frozen_backend.as_deref() != request.backend.map(CliBackend::as_str)
                    || frozen_model.as_deref() != request.model.as_deref())
            {
                return Err(TerminalError::ModelMismatch {
                    expected: format!(
                        "{}/{}",
                        frozen_backend.as_deref().unwrap_or("unknown"),
                        frozen_model.as_deref().unwrap_or("default")
                    ),
                    observed: format!(
                        "{}/{}",
                        request.backend.map(CliBackend::as_str).unwrap_or("unknown"),
                        request.model.as_deref().unwrap_or("default")
                    ),
                });
            }
        }
        transaction.execute(
            "INSERT INTO terminal_runs(
               id,project_id,slot_kind,agent_role,free_form_slot_id,free_form_label,
               origin,execution_mode,backend,model,
               pulsation_id,job_id,agent_run_id,llm_execution_id,status,created_at,updated_at,
               slot_id,parent_run_id,continuation_reason,requested_model_profile,model_source,observation_trial_id
              ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,
                       'queued',?15,?15,?16,?17,?18,?19,?20,?21)",
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
                request.observation_trial_id,
            ],
        )?;
        transaction.commit()?;
        self.get_run(&id)
    }

    pub fn get_run(&self, run_id: &str) -> Result<TerminalRun, TerminalError> {
        let host = self.host.as_ref();
        self.connection()?.query_row(
            "SELECT id,project_id,agent_role,origin,execution_mode,backend,model,native_session_id,
                    pulsation_id,job_id,agent_run_id,llm_execution_id,status,exit_code,error_code,
                    error_message,created_at,started_at,finished_at,updated_at,deadline_at,
                    termination_reason,output_bytes,event_count,workspace_snapshot_hash,
                    launch_identity_sha256,detected_cli_version,adapter_contract_version,
                    slot_kind,free_form_slot_id,slot_id,parent_run_id,continuation_reason,
                    session_fingerprint,requested_model_profile,model_source,observed_model,observation_trial_id
             FROM terminal_runs WHERE id=?1",
            [required(run_id,"runId")?],
            |row| map_run(host, row),
        ).optional()?.ok_or_else(|| TerminalError::NotFound("run".into()))
    }

    pub fn list_runs(
        &self,
        project_id: &str,
        role: AgentRole,
        limit: usize,
    ) -> Result<Vec<TerminalRun>, TerminalError> {
        let host = self.host.as_ref();
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id,project_id,agent_role,origin,execution_mode,backend,model,native_session_id,
                    pulsation_id,job_id,agent_run_id,llm_execution_id,status,exit_code,error_code,
                    error_message,created_at,started_at,finished_at,updated_at,deadline_at,
                    termination_reason,output_bytes,event_count,workspace_snapshot_hash,
                    launch_identity_sha256,detected_cli_version,adapter_contract_version,
                    slot_kind,free_form_slot_id,slot_id,parent_run_id,continuation_reason,
                    session_fingerprint,requested_model_profile,model_source,observed_model,observation_trial_id
             FROM terminal_runs WHERE project_id=?1 AND agent_role=?2
             ORDER BY created_at DESC LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![
                required(project_id, "projectId")?,
                role.as_str(),
                limit.clamp(1, 200)
            ],
            |row| map_run(host, row),
        )?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(TerminalError::from)
    }

    fn has_cli_role_run(&self, pulsation_id: &str, role: AgentRole) -> Result<bool, TerminalError> {
        self.connection()?
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM terminal_runs
                 WHERE pulsation_id=?1 AND agent_role=?2 AND execution_mode='cli_process')",
                params![pulsation_id, role.as_str()],
                |row| row.get(0),
            )
            .map_err(TerminalError::from)
    }

    pub fn list_slot_runs(
        &self,
        project_id: &str,
        slot: &SessionSlot,
        limit: usize,
    ) -> Result<Vec<TerminalRun>, TerminalError> {
        let host = self.host.as_ref();
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id,project_id,agent_role,origin,execution_mode,backend,model,native_session_id,
                    pulsation_id,job_id,agent_run_id,llm_execution_id,status,exit_code,error_code,
                    error_message,created_at,started_at,finished_at,updated_at,deadline_at,
                    termination_reason,output_bytes,event_count,workspace_snapshot_hash,
                    launch_identity_sha256,detected_cli_version,adapter_contract_version,
                    slot_kind,free_form_slot_id,slot_id,parent_run_id,continuation_reason,
                    session_fingerprint,requested_model_profile,model_source,observed_model,observation_trial_id
             FROM terminal_runs WHERE project_id=?1 AND slot_id=?2
             ORDER BY created_at DESC LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![
                required(project_id, "projectId")?,
                slot.persisted_id(project_id),
                limit.clamp(1, 200)
            ],
            |row| map_run(host, row),
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
        let host = self.host.as_ref();
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
            .map(|message| host.redact_secrets(&sanitize_terminal_text(message.as_bytes())))
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
                // `termination_reason` has a fixed vocabulary in the schema and
                // both timeouts belong to its `timeout` value; the distinction
                // between them lives in `error_code`.
                Some("cli_timeout" | "cli_inactivity_timeout") => "timeout",
                Some("cli_output_limit" | "cli_output_budget_exceeded") => "output_limit",
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
        if changed > 0 {
            host.record_run_status(&transaction, &current.project_id, run_id, status)?;
        }
        if changed > 0 && terminal && status != TerminalRunStatus::Completed {
            transaction.execute(
                "UPDATE token_usage_records SET quality='partial',updated_at=?1
                 WHERE source_kind='terminal_run' AND source_id=?2 AND quality='complete'",
                params![now, run_id],
            )?;
        }
        if changed > 0
            && terminal
            && let Some(trial_id) = current.observation_trial_id.as_deref()
        {
            transaction.execute(
                    "UPDATE cli_observation_trials
                     SET settled_runs=settled_runs+1,
                         state=CASE WHEN state='closing' AND settled_runs+1 >= admitted_runs THEN 'completed' ELSE state END,
                         completed_at=CASE WHEN state='closing' AND settled_runs+1 >= admitted_runs THEN ?1 ELSE completed_at END
                     WHERE id=?2 AND state IN ('active','closing')",
                params![now, trial_id],
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

    /// Starts one project-scoped observation trial. Existing and historical
    /// runs are never reclassified; only a later automatic Pulsation launch
    /// can reserve one of its fixed twenty slots.
    pub fn start_cli_observation_trial(
        &self,
        project_id: &str,
    ) -> Result<CliObservationTrial, TerminalError> {
        let project_id = required(project_id, "projectId")?;
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let db = self.connection()?;
        let tx = db.unchecked_transaction()?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM cli_observation_trials WHERE project_id=?1 AND state IN ('active','closing'))",
            [project_id], |row| row.get(0),
        )?;
        if exists {
            return Err(TerminalError::Validation(
                "a CLI observation is already open in this project".into(),
            ));
        }
        tx.execute(
            "INSERT INTO cli_observation_trials(id,project_id,state,target_runs,admitted_runs,settled_runs,started_at)
             VALUES(?1,?2,'active',20,0,0,?3)",
            params![id, project_id, now],
        )?;
        tx.commit()?;
        self.get_cli_observation_trial(&id)
    }

    pub fn active_cli_observation_trial(
        &self,
        project_id: &str,
    ) -> Result<Option<CliObservationTrial>, TerminalError> {
        self.connection()?
            .query_row(
                "SELECT id,project_id,state,target_runs,admitted_runs,settled_runs,started_at,closed_at,completed_at
                 FROM cli_observation_trials WHERE project_id=?1 AND state IN ('active','closing') ORDER BY started_at DESC LIMIT 1",
                [required(project_id, "projectId")?], map_cli_observation_trial,
            )
            .optional()
            .map_err(TerminalError::from)
    }

    pub fn latest_cli_observation_report(
        &self,
        project_id: &str,
    ) -> Result<Option<CliObservationReport>, TerminalError> {
        let db = self.connection()?;
        let trial = db.query_row(
            "SELECT id,project_id,state,target_runs,admitted_runs,settled_runs,started_at,closed_at,completed_at
             FROM cli_observation_trials WHERE project_id=?1 ORDER BY started_at DESC LIMIT 1",
            [required(project_id, "projectId")?], map_cli_observation_trial,
        ).optional()?;
        let Some(trial) = trial else {
            return Ok(None);
        };
        observation_report::build(&db, trial).map(Some)
    }

    /// Longest wait between two reads of a finished run's pipes.
    pub fn set_transport_silence(
        &self,
        run_id: &str,
        max_transport_silence_ms: u64,
    ) -> Result<(), TerminalError> {
        self.connection()?.execute(
            "UPDATE terminal_runs SET max_transport_silence_ms=?1 WHERE id=?2",
            params![max_transport_silence_ms as i64, required(run_id, "runId")?],
        )?;
        Ok(())
    }

    pub fn get_cli_observation_trial(
        &self,
        trial_id: &str,
    ) -> Result<CliObservationTrial, TerminalError> {
        self.connection()?
            .query_row(
                "SELECT id,project_id,state,target_runs,admitted_runs,settled_runs,started_at,closed_at,completed_at
                 FROM cli_observation_trials WHERE id=?1",
                [required(trial_id, "trialId")?], map_cli_observation_trial,
            )
            .optional()?
            .ok_or_else(|| TerminalError::NotFound("CLI observation".into()))
    }

    /// Atomically consumes a trial slot. Callers must additionally enforce the
    /// automatic-Pulsation origin and a non-empty pulsation id.
    fn reserve_cli_observation_slot(
        &self,
        project_id: &str,
    ) -> Result<Option<CliObservationTrial>, TerminalError> {
        let db = self.connection()?;
        let tx = db.unchecked_transaction()?;
        let trial: Option<CliObservationTrial> = tx.query_row(
            "SELECT id,project_id,state,target_runs,admitted_runs,settled_runs,started_at,closed_at,completed_at
             FROM cli_observation_trials WHERE project_id=?1 AND state='active' AND admitted_runs < target_runs
             ORDER BY started_at DESC LIMIT 1",
            [required(project_id, "projectId")?], map_cli_observation_trial,
        ).optional()?;
        let Some(trial) = trial else {
            return Ok(None);
        };
        let now = Utc::now().to_rfc3339();
        let changed = tx.execute(
            "UPDATE cli_observation_trials
             SET admitted_runs=admitted_runs+1,
                 state=CASE WHEN admitted_runs+1 >= target_runs THEN 'closing' ELSE state END,
                 closed_at=CASE WHEN admitted_runs+1 >= target_runs THEN ?1 ELSE closed_at END
             WHERE id=?2 AND state='active' AND admitted_runs < target_runs",
            params![now, trial.id],
        )?;
        if changed != 1 {
            return Err(TerminalError::Conflict);
        }
        tx.commit()?;
        self.get_cli_observation_trial(&trial.id).map(Some)
    }

    /// Returns a reservation only when the run record could not be created.
    /// This prevents an unrelated validation/persistence error from consuming
    /// one of the twenty observations. A linked run is never released.
    fn release_cli_observation_reservation(&self, trial_id: &str) -> Result<(), TerminalError> {
        let db = self.connection()?;
        let tx = db.unchecked_transaction()?;
        let changed = tx.execute(
            "UPDATE cli_observation_trials
             SET admitted_runs=admitted_runs-1,
                 state=CASE WHEN state='closing' THEN 'active' ELSE state END,
                 closed_at=CASE WHEN state='closing' THEN NULL ELSE closed_at END
             WHERE id=?1 AND state IN ('active','closing') AND admitted_runs > settled_runs",
            [trial_id],
        )?;
        if changed != 1 {
            return Err(TerminalError::Conflict);
        }
        tx.commit()?;
        Ok(())
    }

    pub fn reserve_cli_observation_for_pulsation(
        &self,
        project_id: &str,
        pulsation_id: Option<&str>,
        origin: ExecutionOrigin,
    ) -> Result<Option<CliObservationTrial>, TerminalError> {
        if pulsation_id.is_none()
            || !matches!(
                origin,
                ExecutionOrigin::Pipeline | ExecutionOrigin::Learning
            )
        {
            return Ok(None);
        }
        self.reserve_cli_observation_slot(project_id)
    }

    pub fn record_cli_observation_sample(
        &self,
        run_id: &str,
        sample: &CliObservationSample,
    ) -> Result<(), TerminalError> {
        self.connection()?.execute(
            "INSERT INTO cli_observation_samples(run_id,observed_at,elapsed_ms,user_cpu_100ns,kernel_cpu_100ns,peak_process_memory_bytes,peak_job_memory_bytes)
             VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![run_id, sample.observed_at, sample.elapsed_ms as i64, sample.user_cpu_100ns.map(|v| v as i64), sample.kernel_cpu_100ns.map(|v| v as i64), sample.peak_process_memory_bytes.map(|v| v as i64), sample.peak_job_memory_bytes.map(|v| v as i64)],
        )?;
        Ok(())
    }

    pub fn set_native_session_id(
        &self,
        run_id: &str,
        session_id: &str,
    ) -> Result<TerminalRun, TerminalError> {
        let host = self.host.as_ref();
        let session_id = required(session_id, "sessionId")?;
        validate_public_cli_identity(host, session_id)?;
        if session_id.len() > 256 {
            return Err(TerminalError::Validation(
                "the CLI session identifier exceeds the limit".into(),
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
        let host = self.host.as_ref();
        let model = required(model, "observedModel")?;
        validate_public_cli_identity(host, model)?;
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

    fn record_model_observation(
        &self,
        run_id: &str,
        ordinal: u64,
        event_sequence: u64,
        model_id: Option<&str>,
        message_id: Option<&str>,
        parent_invocation_id: Option<&str>,
        agent_id: Option<&str>,
        is_generation: bool,
        evidence_source: &str,
    ) -> Result<(), TerminalError> {
        let host = self.host.as_ref();
        for (field, value, max_chars) in [
            ("observedModel", model_id, MAX_MODEL_CHARS),
            ("messageId", message_id, 256),
            ("parentInvocationId", parent_invocation_id, 256),
            ("agentId", agent_id, 256),
        ] {
            if let Some(value) = value {
                validate_public_cli_identity(host, value)?;
            }
            if value.is_some_and(|value| {
                value.trim().is_empty()
                    || value.chars().count() > max_chars
                    || value.chars().any(char::is_control)
            }) {
                return Err(TerminalError::Validation(format!(
                    "identity field {field} is invalid"
                )));
            }
        }
        self.connection()?.execute(
            "INSERT INTO terminal_model_observations(
               run_id,ordinal,event_sequence,model_id,message_id,parent_invocation_id,agent_id,is_generation,created_at,evidence_source
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                required(run_id, "runId")?,
                ordinal,
                event_sequence,
                model_id,
                message_id,
                parent_invocation_id,
                agent_id,
                is_generation,
                Utc::now().to_rfc3339(),
                evidence_source,
            ],
        )?;
        Ok(())
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
        let host = self.host.as_ref();
        // Scan the complete bounded value before display truncation. In particular a
        // private-key footer must not be discarded before the scanner sees it.
        if content.len() > MAX_RUN_OUTPUT_BYTES {
            return Err(TerminalError::OutputLimit);
        }
        let sanitized = sanitize_terminal_text_bounded(content.as_bytes(), MAX_RUN_OUTPUT_BYTES);
        let redacted = host
            .redact_secrets(&sanitized)
            .map_err(TerminalError::Redaction)?;
        self.append_redacted_event(run_id, sequence, kind, stream, redacted)
    }

    fn append_redacted_event(
        &self,
        run_id: &str,
        sequence: u64,
        kind: TerminalEventKind,
        stream: Option<&str>,
        redacted: RedactionResult,
    ) -> Result<TerminalEvent, TerminalError> {
        if sequence == 0 {
            return Err(TerminalError::Validation(
                "sequence must be greater than zero".into(),
            ));
        }
        if !matches!(stream, None | Some("stdout") | Some("stderr")) {
            return Err(TerminalError::Validation("invalid stream".into()));
        }
        let event = TerminalEvent {
            id: Uuid::new_v4().to_string(),
            run_id: required(run_id, "runId")?.to_string(),
            sequence,
            kind,
            stream: stream.map(str::to_owned),
            content: sanitize_terminal_text(redacted.text.as_bytes()),
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

    /// One page of a run's events after `after_sequence`, sanitized.
    ///
    /// ADR-037 M2: a run recorded under a known sanitization policy is read
    /// by indexed range, with up to [`RANGE_CONTEXT_EVENTS`] earlier events
    /// as redaction context, so a missed range costs one page whatever the
    /// run's length. A legacy run still reads its whole history, which its
    /// fragmented-redaction check needs.
    pub fn list_events(
        &self,
        run_id: &str,
        after_sequence: u64,
        limit: usize,
    ) -> Result<Vec<TerminalEvent>, TerminalError> {
        let limit = limit.clamp(1, 500);
        let connection = self.connection()?;
        if !run_has_known_policy(&connection, run_id)? {
            return Ok(self
                .redacted_run_events(run_id)?
                .into_iter()
                .filter(|event| event.sequence > after_sequence)
                .take(limit)
                .collect());
        }
        let start = after_sequence.saturating_sub(RANGE_CONTEXT_EVENTS);
        let mut events = read_run_events_range(
            &connection,
            run_id,
            start,
            limit + (after_sequence - start) as usize,
        )?;
        sanitize_history_events(self.host.as_ref(), &mut events, true)?;
        Ok(events
            .into_iter()
            .filter(|event| event.sequence > after_sequence)
            .take(limit)
            .collect())
    }

    fn redacted_run_events(&self, run_id: &str) -> Result<Vec<TerminalEvent>, TerminalError> {
        let host = self.host.as_ref();
        let connection = self.connection()?;
        let mut events = read_run_history_events(&connection, run_id)?;
        let known_policy = run_has_known_policy(&connection, run_id)?;
        sanitize_history_events(host, &mut events, known_policy)?;
        Ok(events)
    }

    pub fn clear_history(
        &self,
        project_id: &str,
        role: Option<AgentRole>,
    ) -> Result<usize, TerminalError> {
        let mut connection = self.connection()?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let active: bool = if let Some(role) = role {
            transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM terminal_runs WHERE project_id=?1 AND agent_role=?2
                 AND status IN ('queued','starting','running','awaiting_fallback'))",
                params![required(project_id, "projectId")?, role.as_str()],
                |row| row.get(0),
            )?
        } else {
            transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM terminal_runs WHERE project_id=?1
                 AND status IN ('queued','starting','running','awaiting_fallback'))",
                [required(project_id, "projectId")?],
                |row| row.get(0),
            )?
        };
        if active {
            return Err(TerminalError::Validation(
                "the history cannot be cleared while a run or fallback is active".into(),
            ));
        }
        let frozen_for_open_pulsation: bool = if let Some(role) = role {
            transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM terminal_runs r JOIN pulsations p ON p.id=r.pulsation_id
                 WHERE r.project_id=?1 AND r.agent_role=?2 AND p.status NOT IN ('done','failed'))",
                params![project_id, role.as_str()],
                |row| row.get(0),
            )?
        } else {
            transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM terminal_runs r JOIN pulsations p ON p.id=r.pulsation_id
                 WHERE r.project_id=?1 AND p.status NOT IN ('done','failed'))",
                [project_id],
                |row| row.get(0),
            )?
        };
        if frozen_for_open_pulsation {
            return Err(TerminalError::Validation(
                "cannot clear model audit while a Pulsation using it remains open".into(),
            ));
        }
        let affected = if let Some(role) = role {
            transaction.execute(
                "DELETE FROM terminal_runs WHERE project_id=?1 AND agent_role=?2",
                params![required(project_id, "projectId")?, role.as_str()],
            )?
        } else {
            transaction.execute(
                "DELETE FROM terminal_runs WHERE project_id=?1",
                [required(project_id, "projectId")?],
            )?
        };
        transaction.commit()?;
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
                "the history cannot be cleared while the free-form agent is active".into(),
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
        let interrupted = transaction
            .prepare(
                "SELECT id,project_id FROM terminal_runs WHERE status IN ('queued','starting','running')",
            )?
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        let changed = transaction.execute(
            "UPDATE terminal_runs SET status='interrupted',error_code='app_restart',
               error_message='Run interrupted because the app closed.',finished_at=?1,updated_at=?1
             WHERE status IN ('queued','starting','running')",
            [now],
        )?;
        for (run_id, project_id) in &interrupted {
            self.host.record_run_status(
                &transaction,
                project_id,
                run_id,
                TerminalRunStatus::Interrupted,
            )?;
        }
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
        let host = self.host.as_ref();
        let project_id = required(project_id, "projectId")?;
        let run_id = required(run_id, "runId")?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        // Freeze the selected original evidence before deriving either hash or
        // projection. Diagnostic fields normally redacted by map_run must be
        // restored only in this private, in-memory envelope.
        let run = transaction.query_row(
            "SELECT id,project_id,agent_role,origin,execution_mode,backend,model,native_session_id,
                    pulsation_id,job_id,agent_run_id,llm_execution_id,status,exit_code,error_code,
                    error_message,created_at,started_at,finished_at,updated_at,deadline_at,
                    termination_reason,output_bytes,event_count,workspace_snapshot_hash,
                    launch_identity_sha256,detected_cli_version,adapter_contract_version,
                    slot_kind,free_form_slot_id,slot_id,parent_run_id,continuation_reason,
                    session_fingerprint,requested_model_profile,model_source,observed_model,observation_trial_id
             FROM terminal_runs WHERE id=?1",
            [run_id],
            |row| {
                let mut run = map_run(host, row)?;
                run.error_message = row.get(15)?;
                run.termination_reason = row.get(21)?;
                run.detected_cli_version = row.get(26)?;
                run.continuation_reason = row.get(32)?;
                Ok(run)
            },
        ).optional().map_err(|_| TerminalError::Redaction(
            "Terminal export evidence could not be read safely.".into(),
        ))?.ok_or_else(|| TerminalError::NotFound("run".into()))?;
        if run.project_id != project_id {
            return Err(TerminalError::Validation(
                "the run belongs to another project".into(),
            ));
        }
        let mut events = read_run_history_events(&transaction, run_id).map_err(|error| {
            if matches!(error, TerminalError::OutputLimit) {
                error
            } else {
                TerminalError::Redaction(
                    "Terminal export evidence could not be read safely.".into(),
                )
            }
        })?;
        let known_policy: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM secret_sanitization_receipts receipt
             JOIN terminal_runs run ON run.id=?1 AND receipt.project_id=run.project_id
             WHERE receipt.call_id=run.id OR receipt.call_id=run.llm_execution_id)",
            [run_id],
            |row| row.get(0),
        )?;
        transaction.commit()?;
        let mut projection = serde_json::json!({
            "schemaVersion": 1,
            "redacted": true,
            "run": run,
            "events": &events,
        });
        canonicalize_terminal_export(&mut projection);
        let original_evidence_sha256 = lower_hex(&Sha256::digest(terminal_export_bytes(
            &projection,
            false,
            MAX_RUN_OUTPUT_BYTES,
        )?));
        sanitize_history_events(host, &mut events, known_policy)?;
        projection["events"] = serde_json::to_value(events)?;
        redact_json_strings(host, &mut projection)?;
        canonicalize_terminal_export(&mut projection);
        let sanitized_content_sha256 = lower_hex(&Sha256::digest(terminal_export_bytes(
            &projection,
            false,
            MAX_RUN_OUTPUT_BYTES,
        )?));
        projection["sanitization"] = serde_json::json!({
            "policy": SANITIZATION_POLICY_VERSION,
            "originalEvidenceSha256": original_evidence_sha256,
            "sanitizedContentSha256": sanitized_content_sha256,
            "hashScope": "complete schemaVersion/redacted/run/events envelope before sanitization metadata; original: selected original run fields and ordered events; sanitized: derived safe content",
            "hashFormat": "SHA-256 of compact UTF-8 JSON; recursively sorted object keys; arrays in original order; no trailing newline",
        });
        canonicalize_terminal_export(&mut projection);
        let payload = terminal_export_bytes(&projection, true, MAX_RUN_OUTPUT_BYTES)?;
        let workspace = self.project_workspace(project_id)?;
        let directory = workspace.join(".hard-vibe-code").join("terminal-exports");
        fs::create_dir_all(&directory)?;
        let destination = directory.join(format!("terminal-{run_id}.json"));
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

    /// The CLI version the developer last validated through the contract probe, if any.
    /// Machine-wide (the CLI is), stored in `runtime_settings`.
    pub fn validated_cli_version(
        &self,
        backend: CliBackend,
    ) -> Result<Option<String>, TerminalError> {
        self.connection()?
            .query_row(
                "SELECT value FROM runtime_settings WHERE key=?1",
                params![format!(
                    "{VALIDATED_CLI_VERSION_KEY_PREFIX}{}",
                    backend.as_str()
                )],
                |row| row.get(0),
            )
            .optional()
            .map_err(TerminalError::from)
    }

    pub fn record_validated_cli_version(
        &self,
        backend: CliBackend,
        version: &str,
    ) -> Result<(), TerminalError> {
        self.connection()?.execute(
            "INSERT INTO runtime_settings (key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![
                format!("{VALIDATED_CLI_VERSION_KEY_PREFIX}{}", backend.as_str()),
                version,
                Utc::now().to_rfc3339()
            ],
        )?;
        Ok(())
    }

    /// The backend this pulsation elected for the role after an earlier one
    /// failed, when there is one. Scoped to the pulsation so a recovery never
    /// silently rewrites the project's own configuration.
    pub fn backend_override(
        &self,
        pulsation_id: &str,
        role: AgentRole,
    ) -> Result<Option<CliBackend>, TerminalError> {
        let raw: Option<String> = self
            .connection()?
            .query_row(
                "SELECT backend FROM pulse_execution_overrides WHERE pulsation_id=?1 AND agent_role=?2 AND execution_mode='cli_process'",
                params![required(pulsation_id, "pulsationId")?, role.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        Ok(raw.as_deref().and_then(CliBackend::parse))
    }

    /// Optional per-specialty configuration for composed stage workers
    /// (ADR-022). Absent for every project that never configured one; the
    /// stage's own role configuration then applies.
    pub fn specialty_override(
        &self,
        project_id: &str,
        executor_kind: &str,
    ) -> Result<Option<SpecialtyOverride>, TerminalError> {
        self.connection()?
            .query_row(
                "SELECT execution_mode,backend,model,thinking_level FROM composition_specialty_overrides WHERE project_id=?1 AND executor_kind=?2",
                params![required(project_id, "projectId")?, executor_kind],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?
            .map(|(mode, backend, model, thinking)| {
                Ok(SpecialtyOverride {
                    execution_mode: match mode.as_str() {
                        "cli_process" => AgentExecutionMode::CliProcess,
                        _ => AgentExecutionMode::ApiDirect,
                    },
                    backend: backend.as_deref().and_then(CliBackend::parse),
                    model: model.filter(|value| !value.trim().is_empty()),
                    thinking_level: ThinkingLevel::parse(&thinking).unwrap_or(ThinkingLevel::Default),
                })
            })
            .transpose()
    }

    /// The backend this pulsation is silently running on for its *current*
    /// stage, when the ladder in `elect_next_backend` already switched it
    /// away from the project's configured default. `None` means no override
    /// is active right now (either none was ever elected, or it belongs to
    /// a stage the pulsation already left behind).
    ///
    /// `pulse_events` carries no `agent_role` column, so `reason` is the
    /// latest `cli_backend_elected` message for the pulsation as a whole,
    /// not guaranteed to be scoped to this role specifically -- acceptable
    /// because only one stage/role is ever "in flight" per pulsation.
    pub fn backend_override_status(
        &self,
        pulsation_id: &str,
    ) -> Result<Option<BackendOverrideStatus>, TerminalError> {
        let pulsation_id = required(pulsation_id, "pulsationId")?;
        let connection = self.connection()?;

        let stage: Option<String> = connection
            .query_row(
                "SELECT stage FROM pulsations WHERE id=?1",
                [pulsation_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(stage) = stage else {
            return Ok(None);
        };
        let role = AgentRole::for_stage(&stage);

        let override_row: Option<(Option<String>, String)> = connection
            .query_row(
                "SELECT backend, created_at FROM pulse_execution_overrides
                 WHERE pulsation_id=?1 AND agent_role=?2 AND execution_mode='cli_process'",
                params![pulsation_id, role.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((backend_raw, elected_at)) = override_row else {
            return Ok(None);
        };
        let Some(backend) = backend_raw.as_deref().and_then(CliBackend::parse) else {
            return Ok(None);
        };

        let reason: Option<String> = connection
            .query_row(
                "SELECT message FROM pulse_events
                 WHERE pulsation_id=?1 AND event_type='cli_backend_elected'
                 ORDER BY created_at DESC LIMIT 1",
                [pulsation_id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();

        let job_status: Option<String> = connection
            .query_row(
                "SELECT status FROM jobs
                 WHERE pulsation_id=?1
                   AND (kind='run_stage::'||?2 OR (?2='SPECIFICATION' AND kind='run_stage::Specification'))
                 ORDER BY created_at DESC LIMIT 1",
                params![pulsation_id, stage],
                |row| row.get(0),
            )
            .optional()?;

        Ok(Some(BackendOverrideStatus {
            pulsation_id: pulsation_id.to_string(),
            agent_role: role,
            backend,
            elected_at,
            reason,
            job_status,
        }))
    }
}

/// Read-only view of `backend_override_status` for the UI: the backend a
/// pulsation's current stage silently failed over to, why, and whether its
/// job is still `pending` (the only state `clear_execution_override_and_retry`
/// can act on immediately).
#[derive(Debug, Clone, Serialize)]
pub struct BackendOverrideStatus {
    pub pulsation_id: String,
    pub agent_role: AgentRole,
    pub backend: CliBackend,
    pub elected_at: String,
    pub reason: Option<String>,
    pub job_status: Option<String>,
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
    if (matches!(kind.as_str(), "canonical" | "ephemeral")) != role.is_some()
        || !matches!(kind.as_str(), "canonical" | "free_form" | "ephemeral")
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

fn map_run(host: &dyn TerminalHost, row: &rusqlite::Row<'_>) -> rusqlite::Result<TerminalRun> {
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
        ("ephemeral", Some(agent_role), None) => SessionSlot::Ephemeral {
            id: slot_id
                .clone()
                .ok_or_else(|| invalid_column(30, "missing ephemeral slot id".into()))?,
            agent_role,
        },
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
        error_message: redacted_run_text(host, row, 15)?,
        created_at: row.get(16)?,
        started_at: row.get(17)?,
        finished_at: row.get(18)?,
        updated_at: row.get(19)?,
        deadline_at: row.get(20)?,
        termination_reason: redacted_run_text(host, row, 21)?,
        output_bytes: row.get(22)?,
        event_count: row.get(23)?,
        workspace_snapshot_hash: row.get(24)?,
        launch_identity_sha256: row.get(25)?,
        detected_cli_version: redacted_run_text(host, row, 26)?,
        adapter_contract_version: row.get(27)?,
        parent_run_id: row.get(31)?,
        continuation_reason: redacted_run_text(host, row, 32)?,
        session_fingerprint: row.get(33)?,
        observation_trial_id: row.get(37)?,
    })
}

fn redacted_run_text(
    host: &dyn TerminalHost,
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<Option<String>> {
    let text: Option<String> = row.get(index)?;
    Ok(text.map(|text| {
        host.redact_secrets(&text)
            .map(|result| result.text)
            .unwrap_or_else(|_| "[Diagnostic omitted: sanitization failed.]".into())
    }))
}

fn map_cli_observation_trial(row: &rusqlite::Row<'_>) -> rusqlite::Result<CliObservationTrial> {
    Ok(CliObservationTrial {
        id: row.get(0)?,
        project_id: row.get(1)?,
        state: row.get(2)?,
        target_runs: row.get(3)?,
        admitted_runs: row.get(4)?,
        settled_runs: row.get(5)?,
        started_at: row.get(6)?,
        closed_at: row.get(7)?,
        completed_at: row.get(8)?,
    })
}

fn observation_frame_silence_ms(
    db: &Connection,
    run_id: &str,
    started_at: Option<&str>,
    finished_at: Option<&str>,
) -> Result<Option<u64>, TerminalError> {
    let Some(started_at) = started_at else {
        return Ok(None);
    };
    let Ok(mut previous) = chrono::DateTime::parse_from_rfc3339(started_at) else {
        return Ok(None);
    };
    let mut maximum = 0_u64;
    let mut statement = db.prepare(
        "SELECT created_at FROM terminal_events WHERE run_id=?1 AND stream IS NOT NULL ORDER BY sequence"
    )?;
    let frames = statement.query_map([run_id], |row| row.get::<_, String>(0))?;
    for frame in frames {
        let frame = frame?;
        if let Ok(frame_at) = chrono::DateTime::parse_from_rfc3339(&frame) {
            maximum = maximum.max((frame_at - previous).num_milliseconds().max(0) as u64);
            previous = frame_at;
        }
    }
    if let Some(finished_at) =
        finished_at.and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
    {
        maximum = maximum.max((finished_at - previous).num_milliseconds().max(0) as u64);
    }
    Ok(Some(maximum))
}

fn invalid_column(index: usize, value: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid persisted value: {value}"),
        )),
    )
}

fn required<'a>(value: &'a str, field: &str) -> Result<&'a str, TerminalError> {
    let value = value.trim();
    if value.is_empty() {
        Err(TerminalError::Validation(format!("{field} is required")))
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
            "the free-form agent name must have at most 80 visible characters".into(),
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

/// The instruction appended with the schema for CLIs that cannot enforce one.
/// Notes follow the developer's language (ADR-029), so the English clause that
/// holds every other role's artifacts must not reach the Notes Scribe.
fn schema_prompt_suffix(role: AgentRole) -> &'static str {
    if role == AgentRole::NotesScribe {
        "\nReturn only valid JSON that satisfies this schema. Follow the request language contract:\n"
    } else {
        "\nReturn only valid JSON that satisfies this schema. Follow the request language contract; generated artifacts must be English:\n"
    }
}

/// Automatic agent invocations need an explicit, stable model that the host can
/// bind to the CLI command line and later verify against generation events.
fn require_verifiable_cli_selection(
    backend: CliBackend,
    model: Option<&str>,
) -> Result<&str, TerminalError> {
    let model = model
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .ok_or_else(|| TerminalError::ModelUnverified {
            detail: format!("{} CLI default has no fixed model ID", backend.label()),
        })?;
    if model_identity::verify_model(Some(backend.as_str()), Some(model), Some(model))
        != model_identity::ModelVerification::Verified
    {
        return Err(TerminalError::ModelUnverified {
            detail: format!("{} model `{model}` resolves dynamically", backend.label()),
        });
    }
    Ok(model)
}

fn validate_cli_launch_model(
    backend: CliBackend,
    model: Option<&str>,
    spec: &CliProcessSpec,
) -> Result<(), TerminalError> {
    let expected = require_verifiable_cli_selection(backend, model)?;
    if backend == CliBackend::Custom {
        let mut bound = spec
            .environment
            .iter()
            .filter(|(name, _)| name == "HVC_SELECTED_MODEL")
            .map(|(_, value)| value.as_str());
        if bound.next() != Some(expected) || bound.next().is_some() {
            return Err(TerminalError::ModelMismatch {
                expected: expected.to_owned(),
                observed: "Custom CLI launch environment does not bind exactly the selected model"
                    .into(),
            });
        }
        return Ok(());
    }
    let mut positions = spec
        .args
        .iter()
        .enumerate()
        .filter(|(_, arg)| *arg == "--model" || arg.starts_with("--model="))
        .map(|(index, _)| index);
    let first = positions.next();
    if first
        .and_then(|index| spec.args.get(index + 1))
        .map(String::as_str)
        != Some(expected)
        || positions.next().is_some()
        || first.is_some_and(|index| spec.args[index] != "--model")
    {
        return Err(TerminalError::ModelMismatch {
            expected: expected.to_owned(),
            observed: "CLI launch arguments do not pin exactly the selected model".into(),
        });
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
            "Custom accepts at most {MAX_CUSTOM_ARGS} arguments"
        )));
    }
    if custom_args
        .iter()
        .any(|arg| arg.contains('\0') || arg.chars().count() > MAX_CUSTOM_ARG_CHARS)
    {
        return Err(TerminalError::Validation(
            "invalid or overly long Custom argument".into(),
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
                "Custom requires an absolute executable path".into(),
            ))
        } else {
            Ok(())
        };
    };
    if !Path::new(executable).is_absolute() {
        return Err(TerminalError::Validation(
            "Custom requires an absolute executable path".into(),
        ));
    }
    if Path::new(executable)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "cmd" | "bat" | "ps1"))
    {
        return Err(TerminalError::Validation(
            "Custom does not accept shell scripts; choose a direct executable".into(),
        ));
    }
    if execution_mode == AgentExecutionMode::CliProcess && !custom_risk_ack {
        return Err(TerminalError::Validation(
            "confirm the Custom executable risk before enabling it".into(),
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

/// One window of a CLI subscription's usage allowance. Antigravity supplies it through its
/// auth probe; the Connections page also reads Codex windows through the CLI app-server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliQuotaBucket {
    /// The model group the window covers, e.g. "Gemini Models".
    pub group: String,
    /// The window's length as the CLI names it, e.g. "5h" or "weekly".
    pub window: String,
    pub remaining_percent: u8,
    /// RFC 3339 instant at which the window refills.
    pub reset_time: Option<String>,
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
    /// Remaining usage allowance. Empty when the CLI reports none.
    pub quota: Vec<CliQuotaBucket>,
}

/// A gated backend installed above its validated ceiling and not yet validated by the developer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnvalidatedCliVersion {
    pub backend: CliBackend,
    pub version: String,
    pub minimum: String,
    pub maximum_exclusive: String,
}

impl UnvalidatedCliVersion {
    /// The diagnostic stored on the CLI fallback checkpoint that this condition opens.
    pub fn diagnostic(&self) -> String {
        format!(
            "{} v{} is newer than the last validated version (validated range {}–{}). Validate the updated CLI, switch provider or abort the pulsation.",
            self.backend.label(),
            self.version,
            self.minimum,
            self.maximum_exclusive
        )
    }
}

/// Outcome of `TerminalService::validate_cli_contract`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CliValidationReport {
    pub backend: CliBackend,
    pub version: Option<String>,
    pub validated: bool,
    pub diagnostic: String,
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
                find_executable(backend.executable_name(), &path_entries, &extensions)
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
                quota: Vec::new(),
            }
        })
        .collect()
}

/// A synthetic discovery entry for tests in other modules.
#[cfg(test)]
pub(crate) fn test_availability(
    backend: CliBackend,
    state: CliAvailabilityState,
) -> CliAvailability {
    let (install_url, install_command) = install_guidance(backend);
    CliAvailability {
        backend,
        state,
        executable_path: (state != CliAvailabilityState::Missing)
            .then(|| PathBuf::from(format!("fixture-{}.exe", backend.as_str()))),
        version: Some("fixture".into()),
        diagnostic: None,
        install_url,
        install_command,
        capabilities: adapters::capabilities(backend, None),
        quota: Vec::new(),
    }
}

pub async fn discover_clis_with_probes(host: Arc<dyn TerminalHost>) -> Vec<CliAvailability> {
    let mut discovered = stream::iter(discover_clis())
        .map(|item| probe_cli_availability(host.as_ref(), item))
        .buffer_unordered(4)
        .collect::<Vec<_>>()
        .await;
    discovered.sort_by_key(|availability| match availability.backend {
        CliBackend::Codex => 0,
        CliBackend::Claude => 1,
        CliBackend::Gemini => 2,
        CliBackend::Opencode => 3,
        CliBackend::Antigravity => 4,
        CliBackend::Custom => 5,
    });
    discovered
}

/// Applies the adapter's version verdict to a probed CLI. Returns `false` when the CLI must not
/// be used (below the minimum, or no parsable version). Versions newer than the last validated
/// one stay `Available` — the minimum is the only gate — and only carry an advisory diagnostic, so
/// a CLI auto-update never blocks a pulsation while the validated ceiling waits to be bumped.
fn apply_version_verdict(availability: &mut CliAvailability) -> bool {
    availability.capabilities =
        adapters::capabilities(availability.backend, availability.version.as_deref());
    match adapters::version_compatibility(availability.backend, availability.version.as_deref()) {
        adapters::CliVersionCompatibility::Supported => true,
        adapters::CliVersionCompatibility::Unsupported => {
            availability.state = CliAvailabilityState::UnsupportedVersion;
            availability.diagnostic = Some(format!(
                "The detected {} version did not pass the adapter contract.",
                availability.backend.label()
            ));
            false
        }
        adapters::CliVersionCompatibility::UnknownNewer => {
            let (minimum, maximum_exclusive) = adapters::compatibility_range(availability.backend);
            availability.diagnostic = Some(format!(
                "{} v{} is newer than the last validated version (validated range {}–{}); the adapter runs against the minimum version.",
                availability.backend.label(),
                availability.version.as_deref().unwrap_or("unknown"),
                minimum,
                maximum_exclusive
            ));
            true
        }
    }
}

async fn probe_cli_availability(
    host: &dyn TerminalHost,
    mut availability: CliAvailability,
) -> CliAvailability {
    let Some(executable) = availability.executable_path.clone() else {
        return availability;
    };
    match tokio::time::timeout(
        CLI_VERSION_PROBE_TIMEOUT,
        probe_version(host, availability.backend, &executable),
    )
    .await
    {
        Ok(Ok(version)) if !version.is_empty() => availability.version = Some(version),
        Ok(Ok(_)) => {
            availability.state = CliAvailabilityState::ProbeFailed;
            availability.diagnostic = Some("The CLI returned an empty version.".into());
            return availability;
        }
        Ok(Err(error)) => {
            availability.state = CliAvailabilityState::ProbeFailed;
            availability.diagnostic = Some(format!("Could not run --version: {error}"));
            return availability;
        }
        Err(_) => {
            availability.state = CliAvailabilityState::ProbeFailed;
            availability.diagnostic = Some(format!(
                "The version check took longer than {} seconds.",
                CLI_VERSION_PROBE_TIMEOUT.as_secs()
            ));
            return availability;
        }
    }
    if !apply_version_verdict(&mut availability) {
        return availability;
    }
    if availability.capabilities.auth_probe {
        let timeout = auth_probe_timeout(availability.backend);
        match tokio::time::timeout(timeout, probe_auth(availability.backend, &executable)).await {
            Ok(Ok((true, quota))) => availability.quota = quota,
            Ok(Ok((false, _))) => {
                availability.state = CliAvailabilityState::AuthRequired;
                availability.diagnostic = Some(format!(
                    "{} installed, but no confirmed login.",
                    availability.backend.label()
                ));
            }
            Ok(Err(error)) => {
                availability.state = CliAvailabilityState::ProbeFailed;
                availability.diagnostic = Some(format!(
                    "Could not check {} authentication: {error}",
                    availability.backend.label()
                ));
            }
            Err(_) => {
                availability.state = CliAvailabilityState::ProbeFailed;
                availability.diagnostic = Some(format!(
                    "The authentication check took longer than {} seconds.",
                    timeout.as_secs()
                ));
            }
        }
    }
    availability
}

/// `agy` starts a language server before it answers anything, which took 9.7-10.4 s on this
/// machine (2026-10-14) - right at the shared 10 s limit.
fn auth_probe_timeout(backend: CliBackend) -> Duration {
    match backend {
        CliBackend::Antigravity => CLI_SLOW_AUTH_PROBE_TIMEOUT,
        _ => CLI_AUTH_PROBE_TIMEOUT,
    }
}

async fn probe_version(
    host: &dyn TerminalHost,
    backend: CliBackend,
    executable: &Path,
) -> Result<String, std::io::Error> {
    let secured = secure_probe_spec(backend, executable, vec!["--version".into()]).await?;
    let output = run_bounded_probe(secured).await?;
    if !output.status.success() {
        let diagnostic = host
            .redact_secrets(&sanitize_terminal_text(&output.stderr))
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
    let redacted = host
        .redact_secrets(&sanitized)
        .map_err(std::io::Error::other)?;
    Ok(redacted.text.trim().to_string())
}

/// Whether the CLI confirmed a login, plus whatever quota its probe reported on the way.
async fn probe_auth(
    backend: CliBackend,
    executable: &Path,
) -> Result<(bool, Vec<CliQuotaBucket>), std::io::Error> {
    let Some(args) = adapters::auth_probe_args(backend) else {
        return Ok((true, Vec::new()));
    };
    let secured = secure_probe_spec(
        backend,
        executable,
        args.iter().map(|value| (*value).to_string()).collect(),
    )
    .await?;
    let output = run_bounded_probe(secured).await?;
    if !output.status.success() {
        return Ok((false, Vec::new()));
    }
    Ok((
        true,
        adapters::quota_from_auth_probe(backend, &output.stdout),
    ))
}

struct ProbeOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// A real, hardened CLI launch used only by `validate_cli_contract`: the adapter's own args,
/// the prompt on stdin, bounded output, a read-only scratch directory. No run is recorded.
async fn run_contract_probe(
    spec: CliProcessSpec,
    backend: CliBackend,
) -> Result<ProbeOutput, std::io::Error> {
    let secured = tokio::task::spawn_blocking(move || secure_launch_spec(spec, backend))
        .await
        .map_err(std::io::Error::other)?
        .map_err(std::io::Error::other)?;
    let directory = tempfile::tempdir()?;
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
    let process_tree = ProcessTreeJob::assign(&child)?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(&secured.spec.stdin_payload).await?;
        stdin.shutdown().await?;
    }
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("validation stdout unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| std::io::Error::other("validation stderr unavailable"))?;
    let abort = CancellationToken::new();
    let stdout_task = tokio::spawn(read_bounded_stream(
        stdout,
        abort.clone(),
        MAX_COMPANION_CLI_OUTPUT_BYTES,
        None,
        None,
    ));
    let stderr_task = tokio::spawn(read_bounded_stream(
        stderr,
        abort.clone(),
        MAX_PROBE_OUTPUT_BYTES,
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
                "the validation output exceeded the safe limit",
            ));
        }
    };
    let stdout = stdout_task
        .await
        .map_err(|error| std::io::Error::other(format!("leitor interrompido: {error}")))??;
    let stderr = stderr_task
        .await
        .map_err(|error| std::io::Error::other(format!("leitor interrompido: {error}")))??;
    Ok(ProbeOutput {
        status,
        stdout,
        stderr,
    })
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
    for (name, value) in &spec.environment {
        command.env(name, value);
    }
    configure_hidden(&mut command);
    let mut child = command.spawn()?;
    let process_tree = ProcessTreeJob::assign(&child)?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("probe stdout unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| std::io::Error::other("probe stderr unavailable"))?;
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
                "the probe output exceeded the safe limit",
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
    usage: Option<Arc<dyn crate::host::TerminalUsageRecorder>>,
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
                "the probe output exceeded the safe limit",
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
            let local_app_data = PathBuf::from(local_app_data);
            paths.push(local_app_data.join("Microsoft/WinGet/Links"));
            // The Antigravity installer puts `agy.exe` in its own folder.
            paths.push(local_app_data.join("agy/bin"));
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
        CliBackend::Antigravity => (
            "https://antigravity.google/",
            "irm https://antigravity.google/cli/install.ps1 | iex",
        ),
        CliBackend::Custom => ("", "Choose a direct executable"),
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

pub fn build_process_spec(
    host: &dyn TerminalHost,
    request: CliInvocationRequest,
) -> Result<CliProcessSpec, TerminalError> {
    let mut request = request;
    request.prompt = host
        .redact_context(&request.prompt)
        .map_err(TerminalError::Redaction)?
        .text;
    adapters::build_process_spec(request)
}

fn verified_cli_input(
    host: &dyn TerminalHost,
    system: &str,
    user: &str,
    prepared: Option<&PreparedModelInput>,
) -> Result<PreparedModelInput, TerminalError> {
    host.prepare_input(system, user, prepared.cloned())
}

pub fn sanitize_terminal_text(bytes: &[u8]) -> String {
    sanitize_terminal_text_bounded(bytes, MAX_EVENT_CHARS)
}

fn sanitize_cli_frame_text(bytes: &[u8]) -> String {
    sanitize_terminal_text_bounded(bytes, MAX_RUN_OUTPUT_BYTES)
}

pub fn sanitize_terminal_text_bounded(bytes: &[u8], max_chars: usize) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut output = String::with_capacity(text.len().min(max_chars));
    let mut chars = text.chars().peekable();
    let mut written = 0usize;
    while let Some(ch) = chars.next() {
        if written >= max_chars {
            output.push_str("\n[output truncated]");
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

#[derive(Clone)]
pub struct CliCompletion {
    pub terminal_run_id: String,
    pub backend: CliBackend,
    pub model: String,
    pub raw_output: String,
    pub request_payload: Value,
    pub response_payload: Value,
}

impl std::fmt::Debug for CliCompletion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CliCompletion")
            .field("terminal_run_id", &self.terminal_run_id)
            .field("backend", &self.backend)
            .field("operational_output_bytes", &self.raw_output.len())
            .finish_non_exhaustive()
    }
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

#[derive(Debug, Clone)]
pub struct CompanionCliRoundOutput {
    pub envelope_json: String,
    pub observed_model: Option<String>,
    /// `verified` means exact identity; a Claude family alias is weaker evidence.
    pub model_verification: &'static str,
    pub detected_cli_version: Option<String>,
    pub exit_code: Option<i32>,
    pub output_bytes: usize,
    pub redaction_count: u64,
}

struct CliProcessOutcome {
    success: bool,
    exit_code: Option<i32>,
    raw_output: String,
    termination: ProcessTermination,
    /// The provider's stop reason for the final turn, when it reported one.
    finish_reason: Option<String>,
}

impl std::fmt::Debug for CliProcessOutcome {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CliProcessOutcome")
            .field("success", &self.success)
            .field("exit_code", &self.exit_code)
            .field("operational_output_bytes", &self.raw_output.len())
            .finish_non_exhaustive()
    }
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
    OutputBudget,
    Persistence,
    ModelIntegrity,
    ProviderFailure,
}

#[derive(Debug)]
enum ProcessStreamItem {
    Frame {
        stream: &'static str,
        bytes: Vec<u8>,
        received_at: Instant,
    },
    ReadError {
        stream: &'static str,
        message: String,
        received_at: Instant,
    },
}

#[derive(Default)]
struct ProcessEventSummary {
    max_queue_delay_ms: u128,
    max_persist_ms: u128,
    raw_output: String,
    last_stderr: String,
    /// The stop reason the provider reported for the final turn, when it gave
    /// one. Used to tell an output cut off by the token budget apart from a
    /// model that genuinely returned nothing.
    finish_reason: Option<String>,
    model_generation_count: u64,
    verified_model_generation_count: u64,
    model_observation_count: u64,
}

impl std::fmt::Debug for ProcessEventSummary {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProcessEventSummary")
            .field("operational_output_bytes", &self.raw_output.len())
            .field("diagnostic_bytes", &self.last_stderr.len())
            .field("model_generation_count", &self.model_generation_count)
            .field(
                "verified_model_generation_count",
                &self.verified_model_generation_count,
            )
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone)]
struct CliDiscoveryCache {
    discovered_at: Instant,
    items: Vec<CliAvailability>,
}

#[derive(Debug)]
pub struct SafeCliWorkspace {
    directory: tempfile::TempDir,
    manifest_hash: String,
}

#[derive(Debug)]
struct SecuredLaunch {
    spec: CliProcessSpec,
    identity_sha256: String,
}

impl SafeCliWorkspace {
    pub fn from_directory(directory: tempfile::TempDir, manifest_hash: String) -> Self {
        Self {
            directory,
            manifest_hash,
        }
    }
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
    host: Arc<dyn TerminalHost>,
    repository: TerminalRepository,
    model_catalog: CliModelCatalogService,
    runtime: Handle,
    event_sink: Arc<dyn TerminalEventSink>,
    active: Mutex<HashMap<String, CancellationToken>>,
    active_changed: Notify,
    discovery_cache: Arc<Mutex<Option<CliDiscoveryCache>>>,
    /// Single-flight guard of the background rescan `discovery_for_launch`
    /// starts when it serves a stale snapshot.
    discovery_refreshing: Arc<std::sync::atomic::AtomicBool>,
    quota: Arc<crate::quota::QuotaMeter>,
}

impl TerminalService {
    fn record_prepared_cli_input(
        &self,
        project: &str,
        pulse: Option<&str>,
        call_id: &str,
        prepared: &PreparedModelInput,
    ) -> Result<(), TerminalError> {
        self.host
            .record_prepared_input(&self.repository.db_path, project, pulse, call_id, prepared)
    }
    pub fn new(
        db_path: impl Into<PathBuf>,
        runtime: Handle,
        event_sink: Arc<dyn TerminalEventSink>,
        host: Arc<dyn TerminalHost>,
    ) -> Result<Self, TerminalError> {
        let db_path = db_path.into();
        let quota = Arc::new(crate::quota::QuotaMeter::new(db_path.clone()));
        let repository =
            TerminalRepository::new(db_path.clone(), host.clone()).with_quota_meter(quota.clone());
        repository.recover_interrupted_runs()?;
        Ok(Self {
            repository,
            model_catalog: CliModelCatalogService::new(db_path, host.clone()),
            host,
            runtime,
            event_sink,
            active: Mutex::new(HashMap::new()),
            active_changed: Notify::new(),
            discovery_cache: Arc::new(Mutex::new(None)),
            discovery_refreshing: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            quota,
        })
    }

    pub fn repository(&self) -> &TerminalRepository {
        &self.repository
    }

    /// The plan-quota meter (ADR-028).
    pub fn quota_meter(&self) -> &Arc<crate::quota::QuotaMeter> {
        &self.quota
    }

    /// When the cached discovery was probed; Antigravity's quota windows date from then.
    fn discovery_observed_at(&self) -> chrono::DateTime<Utc> {
        self.discovery_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .and_then(|cache| chrono::Duration::from_std(cache.discovered_at.elapsed()).ok())
            .map_or_else(Utc::now, |age| Utc::now() - age)
    }

    /// Plan usage of every metered CLI. Discovery comes from its cache (rescanned after its
    /// TTL); `force` re-probes the CLIs and skips the meter's cache, as the Refresh button does.
    pub async fn quota_overview(&self, force: bool) -> crate::quota::QuotaOverview {
        let discovery = self.discovery(force).await;
        self.quota
            .refresh(&discovery, self.discovery_observed_at(), force)
            .await
    }

    /// Records the quota at a run boundary in the background; never delays or fails the run.
    fn record_quota_boundary(&self, run: &TerminalRun, kind: crate::quota::SampleKind) {
        let Some(backend) = run.backend else {
            return;
        };
        if !crate::quota::METERED_BACKENDS.contains(&backend) {
            return;
        }
        let availability = self
            .cached_discovery()
            .and_then(|(items, _)| items.into_iter().find(|item| item.backend == backend));
        let discovered_at = self.discovery_observed_at();
        let quota = self.quota.clone();
        let run_id = run.id.clone();
        let pulsation_id = run.pulsation_id.clone();
        self.runtime.spawn(async move {
            quota
                .refresh_and_record_boundary(
                    availability,
                    discovered_at,
                    backend,
                    run_id,
                    pulsation_id,
                    kind,
                )
                .await;
        });
    }

    /// Backends that are installed, supported and authenticated right now, in
    /// the fixed order of `CliBackend::ALL`. Used to elect the next backend for
    /// a role whose current one just failed.
    /// `Some` when `backend` is installed at a version above its validated ceiling that the
    /// developer has not validated yet — and the backend is one that gates on that. A pulsation
    /// about to run it parks on a CLI fallback checkpoint instead of launching.
    pub async fn unvalidated_newer_version(
        &self,
        backend: CliBackend,
    ) -> Result<Option<UnvalidatedCliVersion>, TerminalError> {
        if !backend.gates_unvalidated_versions() {
            return Ok(None);
        }
        let Some(availability) = self
            .discovery(false)
            .await
            .into_iter()
            .find(|item| item.backend == backend)
        else {
            return Ok(None);
        };
        let Some(version) = availability.version.clone() else {
            return Ok(None);
        };
        if adapters::version_compatibility(backend, Some(&version))
            != adapters::CliVersionCompatibility::UnknownNewer
        {
            return Ok(None);
        }
        if self.repository.validated_cli_version(backend)?.as_deref() == Some(version.as_str()) {
            return Ok(None);
        }
        let (minimum, maximum_exclusive) = adapters::compatibility_range(backend);
        Ok(Some(UnvalidatedCliVersion {
            backend,
            version,
            minimum,
            maximum_exclusive,
        }))
    }

    /// Runs the CLI for real, with the adapter's own flags and a one-word prompt, and checks
    /// that the stream comes back in the shape the adapter expects (a completion frame with
    /// text). Discovery is forced first so a CLI that updated itself on the previous launch is
    /// probed at its new version. On success the version is remembered as validated.
    pub async fn validate_cli_contract(
        &self,
        backend: CliBackend,
    ) -> Result<CliValidationReport, TerminalError> {
        let host = self.host.as_ref();
        let availability = self
            .discovery(true)
            .await
            .into_iter()
            .find(|item| item.backend == backend)
            .ok_or_else(|| TerminalError::NotFound(format!("{} CLI", backend.label())))?;
        let version = availability.version.clone();
        let Some(executable) = availability.executable_path.clone() else {
            return Ok(CliValidationReport {
                backend,
                version,
                validated: false,
                diagnostic: availability
                    .diagnostic
                    .unwrap_or_else(|| format!("{} was not found on PATH.", backend.label())),
            });
        };
        if availability.state != CliAvailabilityState::Available {
            return Ok(CliValidationReport {
                backend,
                version,
                validated: false,
                diagnostic: availability
                    .diagnostic
                    .unwrap_or_else(|| format!("{} is not ready.", backend.label())),
            });
        }
        let spec = build_process_spec(
            host,
            CliInvocationRequest {
                executable,
                backend,
                model: None,
                model_profile: None,
                thinking_level: ThinkingLevel::Default,
                prompt: CLI_VALIDATION_PROMPT.to_string(),
                schema_file: None,
                schema: None,
                custom_args: Vec::new(),
                resume_session_id: None,
            },
        )?;
        let outcome =
            tokio::time::timeout(CLI_VALIDATION_TIMEOUT, run_contract_probe(spec, backend)).await;
        let (validated, diagnostic) = match outcome {
            Ok(Ok(probe)) => {
                let frames = probe
                    .stdout
                    .split(|byte| *byte == b'\n')
                    .filter(|line| !line.is_empty())
                    .map(|line| adapters::normalize_frame(backend, "stdout", line))
                    .collect::<Vec<_>>();
                let completed = frames
                    .iter()
                    .any(|frame| frame.kind == TerminalEventKind::Completed);
                let text = frames
                    .iter()
                    .any(|frame| frame.is_text && !frame.content.trim().is_empty());
                if probe.status.success() && completed && text {
                    (
                        true,
                        format!(
                            "{} v{} answered the adapter flags ({} frame(s), completion recognized).",
                            backend.label(),
                            version.as_deref().unwrap_or("unknown"),
                            frames.len()
                        ),
                    )
                } else {
                    let stderr = sanitize_terminal_text_bounded(&probe.stderr, 400);
                    (
                        false,
                        format!(
                            "{} v{} did not answer the adapter contract: exit {}, {} frame(s), completion {}, text {}.{}",
                            backend.label(),
                            version.as_deref().unwrap_or("unknown"),
                            probe
                                .status
                                .code()
                                .map_or_else(|| "?".to_string(), |code| code.to_string()),
                            frames.len(),
                            if completed { "yes" } else { "no" },
                            if text { "yes" } else { "no" },
                            if stderr.trim().is_empty() {
                                String::new()
                            } else {
                                format!(" stderr: {}", stderr.trim())
                            }
                        ),
                    )
                }
            }
            Ok(Err(error)) => (
                false,
                format!("{} could not be run: {error}", backend.label()),
            ),
            Err(_) => (
                false,
                format!(
                    "{} did not finish validation within {} seconds.",
                    backend.label(),
                    CLI_VALIDATION_TIMEOUT.as_secs()
                ),
            ),
        };
        if validated && let Some(version) = version.as_deref() {
            self.repository
                .record_validated_cli_version(backend, version)?;
        }
        Ok(CliValidationReport {
            backend,
            version,
            validated,
            diagnostic,
        })
    }

    pub async fn available_backends(&self) -> Vec<CliBackend> {
        self.discovery(false)
            .await
            .into_iter()
            .filter(|item| item.state == CliAvailabilityState::Available)
            .map(|item| item.backend)
            // Custom is user-supplied and carries no read-only guarantee, so it
            // is never elected automatically.
            .filter(|backend| *backend != CliBackend::Custom)
            .collect()
    }

    /// The cached discovery items, but only when still inside
    /// `CLI_DISCOVERY_CACHE_TTL`. `None` means "must (re)scan" — either the
    /// cache is empty or stale. Shared with synchronous cache consumers so
    /// diagnostics and discovery use the same TTL check.
    fn cache_if_fresh(&self) -> Option<Vec<CliAvailability>> {
        self.discovery_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .filter(|cache| cache.discovered_at.elapsed() < CLI_DISCOVERY_CACHE_TTL)
            .map(|cache| cache.items.clone())
    }

    /// The last discovery without probing anything, and whether it is still
    /// inside `CLI_DISCOVERY_CACHE_TTL`. `None` before the first scan.
    pub fn cached_discovery(&self) -> Option<(Vec<CliAvailability>, bool)> {
        self.discovery_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|cache| {
                (
                    cache.items.clone(),
                    cache.discovered_at.elapsed() < CLI_DISCOVERY_CACHE_TTL,
                )
            })
    }

    async fn discovery(&self, force: bool) -> Vec<CliAvailability> {
        self.discovery_with(force, || {
            scan_cli_discovery(self.runtime.clone(), self.host.clone())
        })
        .await
    }

    async fn discovery_with<F>(
        &self,
        force: bool,
        discover: impl FnOnce() -> F,
    ) -> Vec<CliAvailability>
    where
        F: std::future::Future<Output = Vec<CliAvailability>>,
    {
        if !force && let Some(items) = self.cache_if_fresh() {
            return items;
        }
        let items = discover().await;
        store_cli_discovery(&self.discovery_cache, items.clone());
        items
    }

    /// Discovery for an automatic stage launch. A stale snapshot whose entry
    /// for `backend` is still available is served at once and rescanned in
    /// the background, one rescan at a time: re-probing every installed CLI
    /// on the critical path cost 11-16 s at the first stage after a
    /// checkpoint longer than the TTL (`ToDoListWebView`, 2026-09-22). No
    /// snapshot, or the backend missing or unhealthy in it (even inside the
    /// TTL), waits for a fresh scan so signing in or repairing the CLI takes
    /// effect on the next attempt. The launch re-verifies the binary either way.
    async fn discovery_for_launch(&self, backend: CliBackend) -> Vec<CliAvailability> {
        self.discovery_for_launch_with(backend, || {
            scan_cli_discovery(self.runtime.clone(), self.host.clone())
        })
        .await
    }

    async fn discovery_for_launch_with<F>(
        &self,
        backend: CliBackend,
        discover: impl FnOnce() -> F,
    ) -> Vec<CliAvailability>
    where
        F: std::future::Future<Output = Vec<CliAvailability>>,
    {
        let snapshot = self
            .discovery_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|cache| {
                (
                    cache.discovered_at.elapsed() < CLI_DISCOVERY_CACHE_TTL,
                    cache.items.clone(),
                )
            });
        match snapshot {
            Some((fresh, items))
                if items.iter().any(|item| {
                    item.backend == backend && item.state == CliAvailabilityState::Available
                }) =>
            {
                if !fresh {
                    self.refresh_discovery_in_background();
                }
                items
            }
            _ => self.discovery_with(true, discover).await,
        }
    }

    fn invalidate_discovery_after_failure(&self, backend: CliBackend, error: &TerminalError) {
        if matches!(
            error,
            TerminalError::AuthRequired(_)
                | TerminalError::VersionUnsupported(_)
                | TerminalError::NotFound(_)
                | TerminalError::Io(_)
                | TerminalError::Security(_)
        ) && let Some(cache) = self
            .discovery_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_mut()
        {
            cache.items.retain(|item| item.backend != backend);
        }
    }

    fn refresh_discovery_in_background(&self) {
        use std::sync::atomic::Ordering;
        if self.discovery_refreshing.swap(true, Ordering::SeqCst) {
            return;
        }
        let cache = self.discovery_cache.clone();
        let refreshing = self.discovery_refreshing.clone();
        let runtime = self.runtime.clone();
        let host = self.host.clone();
        self.runtime.spawn(async move {
            let items = scan_cli_discovery(runtime, host).await;
            store_cli_discovery(&cache, items);
            refreshing.store(false, Ordering::SeqCst);
        });
    }

    pub async fn refresh_discovery(&self) -> Vec<CliAvailability> {
        self.discovery(true).await
    }

    /// Rechecks unhealthy CLI selections before dispatch's cached stage preflight.
    /// Healthy selections keep the dispatch path free of additional CLI probes.
    pub async fn refresh_stage_discovery(&self, project_id: &str) -> Result<(), TerminalError> {
        self.refresh_stage_discovery_with(project_id, || {
            scan_cli_discovery(self.runtime.clone(), self.host.clone())
        })
        .await
    }

    async fn refresh_stage_discovery_with<F>(
        &self,
        project_id: &str,
        discover: impl FnOnce() -> F,
    ) -> Result<(), TerminalError>
    where
        F: std::future::Future<Output = Vec<CliAvailability>>,
    {
        let mut backends = HashSet::new();
        for (_, _, role) in hvc_contracts::STAGES {
            match self.repository.get_config(project_id, role) {
                Ok(config)
                    if config.execution_mode == AgentExecutionMode::CliProcess
                        && config.backend != CliBackend::Custom =>
                {
                    backends.insert(config.backend);
                }
                Ok(_) | Err(TerminalError::NotFound(_)) => {}
                Err(error) => return Err(error),
            }
        }
        let snapshot = self.cached_discovery();
        let needs_refresh = backends.iter().any(|backend| {
            snapshot.as_ref().is_none_or(|(items, _)| {
                !items.iter().any(|item| {
                    item.backend == *backend && item.state == CliAvailabilityState::Available
                })
            })
        });
        if needs_refresh {
            self.discovery_with(true, discover).await;
        }
        Ok(())
    }

    /// Global CLI connections and subscription usage; does not require an open project.
    pub async fn connections(&self, refresh: bool) -> CliConnectionsSnapshot {
        let discovery = self.discovery(refresh).await;
        let overview = self
            .quota
            .refresh(&discovery, self.discovery_observed_at(), refresh)
            .await;
        plan_usage::connections(discovery, overview)
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
        let host = self.host.as_ref();
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
            .discovery_for_launch(request.backend)
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
        let prepared =
            verified_cli_input(host, "", &request.prompt, request.prepared_input.as_ref())?;
        let mut prompt = prepared.user_prompt().to_owned();
        if !native_schema {
            prompt.push_str(
                "\n\nReturn only one JSON object that satisfies this control envelope schema. Do not wrap it in Markdown:\n",
            );
            prompt.push_str(
                &host
                    .redact_context(&serde_json::to_string(&request.schema)?)
                    .map_err(TerminalError::Redaction)?
                    .text,
            );
        }
        let requested_model = request.model.clone();
        let requested_profile = request.model_profile.clone();
        let mut spec = adapters::build_process_spec(CliInvocationRequest {
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
        self.record_prepared_cli_input(
            &request.project_id,
            request.usage_context.pulsation_id.as_deref(),
            &request.usage_context.source_id,
            &prepared,
        )?;
        validate_companion_launch_selection(
            &spec,
            request.backend,
            requested_model.as_deref(),
            requested_profile.as_deref(),
        )?;
        let result = run_companion_process(
            host,
            spec,
            request.backend,
            cancellation,
            deadline,
            availability.version,
            self.repository.db_path.clone(),
            request.usage_context,
            requested_model,
        )
        .await;
        drop(hardening_guard);
        drop(schema_guard);
        if let Err(error) = &result {
            self.invalidate_discovery_after_failure(request.backend, error);
        }
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

    async fn resolve_launch(
        &self,
        backend: CliBackend,
        custom_executable: Option<&str>,
    ) -> Result<(PathBuf, Option<String>, CliCapabilities), TerminalError> {
        if backend == CliBackend::Custom {
            return Ok((
                custom_executable.map(PathBuf::from).ok_or_else(|| {
                    TerminalError::Validation("configure the Custom executable".into())
                })?,
                None,
                adapters::capabilities(CliBackend::Custom, None),
            ));
        }
        let availability = self
            .discovery_for_launch(backend)
            .await
            .into_iter()
            .find(|item| item.backend == backend)
            .ok_or_else(|| TerminalError::NotFound(format!("{} CLI", backend.label())))?;
        if availability.state != CliAvailabilityState::Available {
            return Err(TerminalError::Process(
                availability
                    .diagnostic
                    .unwrap_or_else(|| format!("{} is not ready to run", backend.label())),
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

    pub async fn start_manual(
        self: &Arc<Self>,
        request: StartManualRunRequest,
    ) -> Result<TerminalRun, TerminalError> {
        let host = self.host.as_ref();
        let prepared_prompt = host.prepare_input("", required(&request.prompt, "prompt")?, None)?;
        let prompt = prepared_prompt.user_prompt().to_owned();
        if self
            .repository
            .has_active_automatic_run(&request.project_id, request.agent_role)?
        {
            return Err(TerminalError::Validation(
                "the agent is busy with an automatic run".into(),
            ));
        }
        if self
            .repository
            .has_active_manual_run(&request.project_id, request.agent_role)?
        {
            return Err(TerminalError::Validation(
                "a manual run is already active for this agent".into(),
            ));
        }
        let config = self
            .repository
            .get_config(&request.project_id, request.agent_role)?;
        if config.execution_mode != AgentExecutionMode::CliProcess {
            return Err(TerminalError::Validation(
                "manual runs are available only for agents in CLI Mode".into(),
            ));
        }
        if cli_mode_disabled() {
            return Err(TerminalError::Validation(
                "CLI Mode is disabled by the local kill switch".into(),
            ));
        }
        let (executable, detected_version, _) = self
            .resolve_launch(config.backend, config.custom_executable.as_deref())
            .await?;
        let workspace = self.repository.project_workspace(&request.project_id)?;
        let spec = build_process_spec(
            host,
            CliInvocationRequest {
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
            },
        )?;
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
        self.record_prepared_cli_input(
            &run.project_id,
            run.pulsation_id.as_deref(),
            &run.id,
            &prepared_prompt,
        )?;
        let cancellation = CancellationToken::new();
        self.active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(run.id.clone(), cancellation.clone());
        let service = Arc::clone(self);
        let run_for_task = run.clone();
        self.runtime.spawn(async move {
            service
                .run_process(run_for_task, spec, workspace, cancellation, Some(deadline))
                .await;
        });
        Ok(run)
    }

    pub async fn start_free_form(
        self: &Arc<Self>,
        request: StartFreeFormRunRequest,
    ) -> Result<TerminalRun, TerminalError> {
        let host = self.host.as_ref();
        let prepared_prompt = host.prepare_input("", required(&request.prompt, "prompt")?, None)?;
        let prompt = prepared_prompt.user_prompt().to_owned();
        if cli_mode_disabled() {
            return Err(TerminalError::Validation(
                "CLI Mode is disabled by the local kill switch".into(),
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
                "a run is already active for this free-form agent".into(),
            ));
        }
        let slot = self.repository.get_slot(&request.project_id, &slot_ref)?;
        if slot.execution_mode != AgentExecutionMode::CliProcess {
            return Err(TerminalError::Validation(
                "manual runs are available only in CLI Mode".into(),
            ));
        }
        let (executable, detected_version, _) = self
            .resolve_launch(slot.backend, slot.custom_executable.as_deref())
            .await?;
        let workspace = self.repository.project_workspace(&request.project_id)?;
        let spec = build_process_spec(
            host,
            CliInvocationRequest {
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
            },
        )?;
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
                observation_trial_id: None,
            })?;
        let run = self
            .repository
            .set_run_deadline(&run.id, &deadline_timestamp(deadline))?;
        let run = self
            .repository
            .set_adapter_metadata(&run.id, detected_version.as_deref())?;
        self.record_prepared_cli_input(
            &run.project_id,
            run.pulsation_id.as_deref(),
            &run.id,
            &prepared_prompt,
        )?;
        let cancellation = CancellationToken::new();
        self.active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(run.id.clone(), cancellation.clone());
        let service = Arc::clone(self);
        let run_for_task = run.clone();
        self.runtime.spawn(async move {
            service
                .run_process(run_for_task, spec, workspace, cancellation, Some(deadline))
                .await;
        });
        Ok(run)
    }

    pub async fn resume_manual(
        self: &Arc<Self>,
        request: ResumeManualRunRequest,
    ) -> Result<TerminalRun, TerminalError> {
        let host = self.host.as_ref();
        let prepared_prompt = host.prepare_input("", required(&request.prompt, "prompt")?, None)?;
        let prompt = prepared_prompt.user_prompt().to_owned();
        if cli_mode_disabled() {
            return Err(TerminalError::Validation(
                "CLI Mode is disabled by the local kill switch".into(),
            ));
        }
        let source = self.repository.get_run(&request.source_run_id)?;
        if source.project_id != request.project_id {
            return Err(TerminalError::Validation(
                "the chosen session belongs to another project".into(),
            ));
        }
        if source.status.is_active() {
            return Err(TerminalError::Validation(
                "the chosen run is still active".into(),
            ));
        }
        if source.workspace_snapshot_hash.is_none() || source.session_fingerprint.is_none() {
            return Err(TerminalError::Validation(
                "the chosen run has no fingerprint compatible with resuming".into(),
            ));
        }
        let native_session_id = source.native_session_id.clone().ok_or_else(|| {
            TerminalError::Validation("the chosen run has no native session".into())
        })?;
        if self
            .repository
            .has_active_slot_run(&request.project_id, &source.slot)?
        {
            return Err(TerminalError::Validation(
                "a run is already active for this slot".into(),
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
                            "the agent is busy with an automatic run".into(),
                        ));
                    }
                    let config = self.repository.get_config(&request.project_id, role)?;
                    if config.execution_mode != AgentExecutionMode::CliProcess {
                        return Err(TerminalError::Validation(
                            "turn on CLI Mode before resuming this session".into(),
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
                            "the free-form agent is not set up for explicit resuming".into(),
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
                "the backend or model changed since the chosen session".into(),
            ));
        }
        let (executable, detected_version, capabilities) = self
            .resolve_launch(backend, custom_executable.as_deref())
            .await?;
        if !capabilities.native_resume {
            return Err(TerminalError::Validation(format!(
                "{} does not offer compatible native resuming in this version",
                backend.label()
            )));
        }
        let workspace = self.repository.project_workspace(&request.project_id)?;
        let spec = build_process_spec(
            host,
            CliInvocationRequest {
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
            },
        )?;
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
                observation_trial_id: None,
            })?;
        let run = self
            .repository
            .set_run_deadline(&run.id, &deadline_timestamp(deadline))?;
        let run = self
            .repository
            .set_adapter_metadata(&run.id, detected_version.as_deref())?;
        self.record_prepared_cli_input(
            &run.project_id,
            run.pulsation_id.as_deref(),
            &run.id,
            &prepared_prompt,
        )?;
        let cancellation = CancellationToken::new();
        self.active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(run.id.clone(), cancellation.clone());
        let service = Arc::clone(self);
        let run_for_task = run.clone();
        self.runtime.spawn(async move {
            service
                .run_process(run_for_task, spec, workspace, cancellation, Some(deadline))
                .await;
        });
        Ok(run)
    }

    pub async fn execute_llm_request(
        self: &Arc<Self>,
        request: &LlmRequest,
        cancellation: &CancellationToken,
    ) -> Result<Option<CliCompletion>, TerminalError> {
        let host = self.host.as_ref();
        let Some(agent_role) = request.target.agent_role() else {
            return Ok(None);
        };
        let api_recovery_override = request.pulsation_id.as_deref().is_some_and(|pulsation_id| {
            self.repository
                .has_api_override(pulsation_id, agent_role)
                .unwrap_or(false)
        });
        let config = match self.repository.get_config(&request.project_id, agent_role) {
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
                    .get_config(&request.project_id, agent_role)?
            }
            Err(error) => return Err(error),
        };
        if api_recovery_override {
            if config.execution_mode != AgentExecutionMode::ApiDirect {
                return Err(TerminalError::ModelMismatch {
                    expected: format!(
                        "{}/{}",
                        config.backend.as_str(),
                        config.model.as_deref().unwrap_or("default")
                    ),
                    observed: "API recovery override changes the selected backend".into(),
                });
            }
            return Ok(None);
        }
        // A composed unit inherits the canonical role's frozen backend/model.
        // Specialty overrides may change effort only; a stale or conflicting
        // transport/model override is a structural host failure.
        let mut config = config;
        if let Some(unit) = &request.dispatch.unit {
            let selected_transport = if config.execution_mode == AgentExecutionMode::CliProcess {
                "cli"
            } else {
                "api"
            };
            if unit.frozen_transport != selected_transport
                || (selected_transport == "cli"
                    && (unit.frozen_backend.as_deref() != Some(config.backend.as_str())
                        || config.model.as_deref() != Some(unit.frozen_model.as_str())))
            {
                return Err(TerminalError::ModelMismatch {
                    expected: format!(
                        "{}/{}/{}",
                        unit.frozen_transport,
                        unit.frozen_backend.as_deref().unwrap_or("api"),
                        unit.frozen_model
                    ),
                    observed: format!(
                        "{}/{}/{}",
                        selected_transport,
                        config.backend.as_str(),
                        config.model.as_deref().unwrap_or("default")
                    ),
                });
            }
        }
        if let Some(unit) = &request.dispatch.unit
            && let Some(specialty) = self
                .repository
                .specialty_override(&request.project_id, &unit.executor_kind)?
        {
            if specialty.execution_mode != config.execution_mode
                || specialty
                    .backend
                    .is_some_and(|backend| backend != config.backend)
            {
                return Err(TerminalError::ModelMismatch {
                    expected: format!(
                        "{}/{}",
                        config.backend.as_str(),
                        config.model.as_deref().unwrap_or("default")
                    ),
                    observed: format!(
                        "{}/{}",
                        specialty.backend.unwrap_or(config.backend).as_str(),
                        specialty.model.as_deref().unwrap_or("inherited")
                    ),
                });
            }
            if let Some(specialty_model) = specialty.model.as_deref() {
                match model_identity::verify_model(
                    Some(config.backend.as_str()),
                    config.model.as_deref(),
                    Some(specialty_model),
                ) {
                    model_identity::ModelVerification::Verified => {}
                    model_identity::ModelVerification::Mismatch => {
                        return Err(TerminalError::ModelMismatch {
                            expected: config.model.clone().unwrap_or_else(|| "default".into()),
                            observed: specialty_model.to_owned(),
                        });
                    }
                    model_identity::ModelVerification::Unverified => {
                        return Err(TerminalError::ModelUnverified {
                            detail: format!(
                                "specialty model `{specialty_model}` cannot be tied to the selected role model"
                            ),
                        });
                    }
                }
            }
            config.thinking_level = specialty.thinking_level;
        }
        if config.execution_mode == AgentExecutionMode::ApiDirect {
            if let Some(pulsation_id) = request.pulsation_id.as_deref()
                && self.repository.has_cli_role_run(pulsation_id, agent_role)?
            {
                return Err(TerminalError::ModelMismatch {
                    expected: "frozen CLI role selection".into(),
                    observed: "API direct transport".into(),
                });
            }
            return Ok(None);
        }
        require_verifiable_cli_selection(config.backend, config.model.as_deref())?;
        // A specialty's level, or a catalog refreshed since the role was saved,
        // must still be one the model offers; refuse before anything is spawned.
        validate_catalog_effort(
            &self.repository.connection()?,
            &request.project_id,
            config.backend,
            config.model.as_deref(),
            config.model_profile.as_deref(),
            config.thinking_level,
        )?;
        // A pulsation that already burned a backend on this role carries the
        // one the host elected next. Project configuration is left untouched.
        if let Some(pulsation_id) = request.pulsation_id.as_deref()
            && let Some(elected) = self.repository.backend_override(pulsation_id, agent_role)?
            && elected != config.backend
        {
            return Err(TerminalError::ModelMismatch {
                expected: format!(
                    "{}/{}",
                    config.backend.as_str(),
                    config.model.as_deref().unwrap_or("default")
                ),
                observed: format!("{}/fallback", elected.as_str()),
            });
        }
        let feature_discovery =
            request.pulsation_id.is_none() && request.job_chain.starts_with("feature:");
        let notes_scribe = agent_role == AgentRole::NotesScribe;
        let reconciler = agent_role == AgentRole::DocumentationReconciler;
        let host_context_only = cli_context_scope(request)?;
        // All of them read only host-selected text, so all get the Companion's restrictions.
        let companion_scoped = feature_discovery || notes_scribe || reconciler || host_context_only;
        if companion_scoped && config.backend == CliBackend::Custom {
            return Err(TerminalError::Validation(if notes_scribe {
                "The Notes Scribe requires an official read-only CLI adapter or API transport"
                    .into()
            } else if reconciler {
                "The Documentation Reconciler requires an official read-only CLI adapter or API transport"
                    .into()
            } else if host_context_only {
                "A composed unit confined to host-selected context requires an official read-only CLI adapter or API transport"
                    .into()
            } else {
                "Feature discovery requires an official read-only CLI adapter or API transport"
                    .into()
            }));
        }
        if cli_mode_disabled() {
            return Err(TerminalError::Validation(
                "CLI Mode is disabled by the local kill switch".into(),
            ));
        }
        if self
            .repository
            .has_active_manual_run(&request.project_id, agent_role)?
        {
            return Err(TerminalError::Process(
                "the agent has an active manual run".into(),
            ));
        }
        let (executable, detected_version) = match config.backend {
            CliBackend::Custom => (
                config
                    .custom_executable
                    .as_deref()
                    .map(PathBuf::from)
                    .ok_or_else(|| {
                        TerminalError::Validation("configure the Custom executable".into())
                    })?,
                None,
            ),
            backend => {
                let discovery = self.discovery_for_launch(backend).await;
                let availability = discovery
                    .into_iter()
                    .find(|item| item.backend == backend)
                    .ok_or_else(|| TerminalError::NotFound(format!("{} CLI", backend.label())))?;
                if availability.state != CliAvailabilityState::Available {
                    return Err(TerminalError::Process(
                        availability
                            .diagnostic
                            .unwrap_or_else(|| format!("{} is not ready", backend.label())),
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
            ResponseFormat::JsonSchema { schema, .. } => {
                Some(match (&request.dispatch.strict_schema, config.backend) {
                    // Codex enforces OpenAI structured-output strictness.
                    (Some(strict), CliBackend::Codex) => strict.clone(),
                    _ => schema.clone(),
                })
            }
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
        let prepared = verified_cli_input(
            host,
            &request.system_prompt,
            &request.user_prompt,
            request.dispatch.prepared_input.as_ref(),
        )?;
        let mut prompt = format!(
            "<system>\n{}\n</system>\n<user>\n{}\n</user>",
            prepared.system_prompt(),
            prepared.user_prompt()
        );
        if matches!(
            config.backend,
            CliBackend::Gemini | CliBackend::Opencode | CliBackend::Custom
        ) && let Some(schema) = schema.as_ref()
        {
            prompt.push_str(schema_prompt_suffix(agent_role));
            prompt.push_str(
                &host
                    .redact_context(&serde_json::to_string(schema)?)
                    .map_err(TerminalError::Redaction)?
                    .text,
            );
        }
        // Discovery and the Scribe receive only host-selected context, with the
        // Companion's tool restrictions.
        let discovery_directory = companion_scoped.then(tempfile::tempdir).transpose()?;
        let workspace = match &discovery_directory {
            Some(directory) => directory.path().to_path_buf(),
            None => self.repository.project_workspace(&request.project_id)?,
        };
        let recorded_prompt = clip_text(host, &prompt, CLI_REQUEST_PROMPT_MAX_CHARS);
        let recorded_prompt_chars = prompt.chars().count();
        // The proof was verified above. Re-scanning this projection without its
        // source provenance would discard trusted, content-bound exceptions.
        let mut spec = adapters::build_process_spec(CliInvocationRequest {
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
            if companion_scoped {
                Hardening::Companion
            } else {
                Hardening::Pipeline
            },
            &mut spec,
        )?;
        let origin = if notes_scribe {
            ExecutionOrigin::Notes
        } else if reconciler {
            ExecutionOrigin::Reconciliation
        } else if feature_discovery {
            ExecutionOrigin::Feature
        } else if agent_role == AgentRole::LearningAgent {
            ExecutionOrigin::Learning
        } else if agent_role == AgentRole::Visualizer {
            ExecutionOrigin::Review
        } else {
            ExecutionOrigin::Pipeline
        };
        let slot = match &request.target {
            hvc_contracts::AgentExecutionTarget::Ephemeral {
                slot_id,
                agent_role,
            } => {
                self.repository
                    .ensure_ephemeral_slot(&request.project_id, slot_id, *agent_role)?;
                SessionSlot::Ephemeral {
                    id: slot_id.clone(),
                    agent_role: *agent_role,
                }
            }
            hvc_contracts::AgentExecutionTarget::Canonical { agent_role } => {
                SessionSlot::Canonical {
                    agent_role: *agent_role,
                }
            }
            hvc_contracts::AgentExecutionTarget::FreeForm { .. } => return Ok(None),
        };
        let observation_trial = self.repository.reserve_cli_observation_for_pulsation(
            &request.project_id,
            request.pulsation_id.as_deref(),
            origin,
        )?;
        let run = match self.repository.create_session_run(CreateSessionRunRequest {
            project_id: request.project_id.clone(),
            slot,
            origin,
            execution_mode: AgentExecutionMode::CliProcess,
            backend: Some(config.backend),
            model: config.model.clone(),
            pulsation_id: request.pulsation_id.clone(),
            job_id: request.job_id.clone(),
            agent_run_id: request.agent_run_id.clone(),
            llm_execution_id: Some(request.execution_id.0.clone()),
            parent_run_id: None,
            continuation_reason: None,
            observation_trial_id: observation_trial.as_ref().map(|trial| trial.id.clone()),
        }) {
            Ok(run) => run,
            Err(error) => {
                if let Some(trial) = observation_trial.as_ref() {
                    let _ = self
                        .repository
                        .release_cli_observation_reservation(&trial.id);
                }
                return Err(error);
            }
        };
        let deadline = observation_trial.as_ref().map(|_| None).unwrap_or_else(|| {
            Some(
                request
                    .dispatch
                    .cli_timeout
                    .map_or(request.deadline, |budget| budget.deadline),
            )
        });
        let run = if let Some(deadline) = deadline {
            self.repository
                .set_run_deadline(&run.id, &deadline_timestamp(deadline))?
        } else {
            run
        };
        let run = self
            .repository
            .set_adapter_metadata(&run.id, detected_version.as_deref())?;
        self.record_prepared_cli_input(
            &run.project_id,
            run.pulsation_id.as_deref(),
            run.llm_execution_id.as_deref().unwrap_or(&run.id),
            &prepared,
        )?;
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
            .run_process_inner(
                &run,
                spec,
                workspace,
                &combined,
                deadline,
                request.dispatch.cli_timeout.map(|budget| budget.policy),
                // An observation trial exists to watch a run without limits.
                observation_trial
                    .is_none()
                    .then(|| cli_output_ceiling(request.max_output_tokens)),
            )
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
            ProcessTermination::TimedOut(TimeoutReason::Inactivity) => {
                return Err(TerminalError::InactivityTimeout);
            }
            ProcessTermination::TimedOut(TimeoutReason::MaxCeiling) => {
                return Err(TerminalError::Timeout);
            }
            ProcessTermination::Exited => {}
        }
        if !outcome.success {
            let message = format!(
                "{} exited with code {:?}",
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
        if let Some(reason) = outcome
            .finish_reason
            .as_deref()
            .filter(|reason| adapters::is_output_truncation(reason))
        {
            return Err(TerminalError::OutputTruncated {
                backend: config.backend.label().into(),
                reason: reason.to_string(),
                produced_chars: outcome.raw_output.chars().count(),
            });
        }
        if outcome.raw_output.trim().is_empty() {
            return Err(TerminalError::Process(format!(
                "{} produced no final content",
                config.backend.label()
            )));
        }
        let observed_model = self
            .repository
            .get_run(&run.id)?
            .observed_model
            .ok_or_else(|| TerminalError::ModelUnverified {
                detail: "completed CLI run has no persisted generation model".into(),
            })?;
        Ok(Some(CliCompletion {
            terminal_run_id: run.id.clone(),
            backend: config.backend,
            model: observed_model.clone(),
            raw_output: outcome.raw_output,
            request_payload: serde_json::json!({
                "mode": "cli_process",
                "backend": config.backend.as_str(),
                "thinking": config.thinking_level.as_str(),
                "promptChars": recorded_prompt_chars,
                "promptTruncated": recorded_prompt_chars > CLI_REQUEST_PROMPT_MAX_CHARS,
                "prompt": recorded_prompt,
            }),
            response_payload: serde_json::json!({"terminalRunId":run.id,"exitCode":outcome.exit_code,"requestedModel":config.model,"observedModel":observed_model}),
        }))
    }

    async fn run_process(
        self: Arc<Self>,
        run: TerminalRun,
        spec: CliProcessSpec,
        workspace: PathBuf,
        cancellation: CancellationToken,
        deadline: Option<Instant>,
    ) {
        let mut spec = spec;
        let result = match self.isolate_resumable_antigravity(&run, &mut spec) {
            Ok(()) => {
                self.run_process_inner(&run, spec, workspace, &cancellation, deadline, None, None)
                    .await
            }
            Err(error) => Err(error),
        };
        if let Err(error) = result {
            self.record_process_failure(&run, &error);
        }
        self.active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&run.id);
        self.active_changed.notify_waiters();
    }

    /// Manual and `FreeForm` runs can be resumed, and `agy` keeps a conversation inside its
    /// config root, so these runs share one private root beside the database instead of a
    /// fresh one per launch. Same rules as a pipeline stage: reads only.
    fn isolate_resumable_antigravity(
        &self,
        run: &TerminalRun,
        spec: &mut CliProcessSpec,
    ) -> Result<(), TerminalError> {
        if spec_backend(run) != CliBackend::Antigravity {
            return Ok(());
        }
        let directory = self
            .repository
            .db_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("antigravity-cli-home");
        isolate_antigravity(&directory, Hardening::Pipeline, spec)
    }

    async fn run_process_inner(
        &self,
        run: &TerminalRun,
        spec: CliProcessSpec,
        workspace: PathBuf,
        cancellation: &CancellationToken,
        deadline: Option<Instant>,
        policy_override: Option<AgentTimeoutPolicy>,
        output_ceiling: Option<u64>,
    ) -> Result<CliProcessOutcome, TerminalError> {
        if cancellation.is_cancelled() {
            return Err(TerminalError::Cancelled);
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(TerminalError::Timeout);
        }
        let backend = spec_backend(run);
        let sanitization_db_path = self.repository.db_path.clone();
        let sanitization_run = run.clone();
        let host = self.host.clone();
        let (safe_workspace, secured_launch) = tokio::task::spawn_blocking(move || {
            Ok::<_, TerminalError>((
                host.prepare_workspace(&sanitization_db_path, &workspace, &sanitization_run)?,
                secure_launch_spec(spec, backend)?,
            ))
        })
        .await
        .map_err(|error| {
            TerminalError::Security(format!("the snapshot worker was interrupted: {error}"))
        })??;
        if cancellation.is_cancelled() {
            return Err(TerminalError::Cancelled);
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(TerminalError::Timeout);
        }
        let launch_identity_sha256 = secured_launch.identity_sha256;
        let spec = secured_launch.spec;
        if run.origin != ExecutionOrigin::Manual {
            validate_cli_launch_model(backend, run.model.as_deref(), &spec)?;
        }
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
            "Run starting.",
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
        let read_clock = Arc::new(TransportReadClock::new());
        let process_tree = Arc::new(ProcessTreeJob::assign(&child)?);
        let observation_stop = CancellationToken::new();
        let observation_started = Instant::now();
        let observation_task = if run.observation_trial_id.is_some() {
            let repository = self.repository.clone();
            let process_tree = Arc::clone(&process_tree);
            let stop = observation_stop.clone();
            let run_id = run.id.clone();
            let started = observation_started;
            Some(tokio::spawn(async move {
                loop {
                    let sample = match process_tree.observation() {
                        Ok((
                            user_cpu_100ns,
                            kernel_cpu_100ns,
                            peak_process_memory_bytes,
                            peak_job_memory_bytes,
                        )) => CliObservationSample {
                            observed_at: Utc::now().to_rfc3339(),
                            elapsed_ms: started.elapsed().as_millis() as u64,
                            user_cpu_100ns: Some(user_cpu_100ns),
                            kernel_cpu_100ns: Some(kernel_cpu_100ns),
                            peak_process_memory_bytes: Some(peak_process_memory_bytes),
                            peak_job_memory_bytes: Some(peak_job_memory_bytes),
                        },
                        Err(_) => CliObservationSample {
                            observed_at: Utc::now().to_rfc3339(),
                            elapsed_ms: started.elapsed().as_millis() as u64,
                            user_cpu_100ns: None,
                            kernel_cpu_100ns: None,
                            peak_process_memory_bytes: None,
                            peak_job_memory_bytes: None,
                        },
                    };
                    let repository_for_write = repository.clone();
                    let run_for_write = run_id.clone();
                    let _ = tokio::task::spawn_blocking(move || {
                        repository_for_write.record_cli_observation_sample(&run_for_write, &sample)
                    })
                    .await;
                    tokio::select! { _ = stop.cancelled() => break, _ = tokio::time::sleep(Duration::from_secs(5)) => {} }
                }
            }))
        } else {
            None
        };
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| TerminalError::Validation("CLI process stdin unavailable".into()))?;
        stdin.write_all(&spec.stdin_payload).await?;
        stdin.write_all(b"\n").await?;
        stdin.shutdown().await?;
        drop(stdin);
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| TerminalError::Validation("CLI process stdout unavailable".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| TerminalError::Validation("CLI process stderr unavailable".into()))?;
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
            "Run in progress.",
        )?;
        self.record_quota_boundary(run, crate::quota::SampleKind::RunStart);
        let (stream_sender, stream_receiver) = mpsc::channel(PROCESS_EVENT_CHANNEL_CAPACITY);
        let abort_token = CancellationToken::new();
        let abort_reason = Arc::new(Mutex::new(None));
        let caller_remaining =
            deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()));
        let mut timeout_policy = policy_override.unwrap_or_else(|| match run.slot {
            SessionSlot::FreeForm { .. } => AgentTimeoutPolicy::manual(),
            SessionSlot::Canonical { agent_role } | SessionSlot::Ephemeral { agent_role, .. } => {
                AgentTimeoutPolicy::for_role(agent_role)
            }
        });
        if caller_remaining.is_some_and(|duration| duration.is_zero()) {
            process_tree.terminate();
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(TerminalError::Timeout);
        }
        if let Some(caller_remaining) = caller_remaining {
            timeout_policy.max_timeout_ms = timeout_policy
                .max_timeout_ms
                .min(caller_remaining.as_millis() as u64);
            timeout_policy.base_timeout_ms = timeout_policy
                .base_timeout_ms
                .min(timeout_policy.max_timeout_ms);
        }
        let watchdog = deadline.map(|_| ProgressWatchdog::new(timeout_policy));
        let output_completion = Arc::new(Mutex::new(None));
        let coordinator_task = tokio::spawn(coordinate_process_events_with_completion(
            self.repository.clone(),
            self.event_sink.clone(),
            running,
            spec_backend(run),
            stream_receiver,
            abort_token.clone(),
            Arc::clone(&abort_reason),
            watchdog
                .clone()
                .unwrap_or_else(|| ProgressWatchdog::new(AgentTimeoutPolicy::manual())),
            output_ceiling,
            Some(Arc::clone(&output_completion)),
        ));
        let stdout_task = tokio::spawn(read_process_stream(
            "stdout",
            stdout,
            stream_sender.clone(),
            watchdog.clone(),
            spec_backend(run),
            Arc::clone(&read_clock),
        ));
        let stderr_task = tokio::spawn(read_process_stream(
            "stderr",
            stderr,
            stream_sender.clone(),
            watchdog.clone(),
            spec_backend(run),
            Arc::clone(&read_clock),
        ));
        let (status, termination) = if let Some(watchdog) = watchdog.as_ref() {
            tokio::select! {
                status=child.wait()=>(status?,ProcessTermination::Exited),
                _=cancellation.cancelled()=>{ process_tree.terminate(); let _=child.kill().await; (child.wait().await?,ProcessTermination::Cancelled) }
                reason=watchdog.wait_for_timeout()=>{ process_tree.terminate(); let _=child.kill().await; (child.wait().await?,ProcessTermination::TimedOut(reason)) }
                _=abort_token.cancelled()=>{ process_tree.terminate(); let _=child.kill().await; (child.wait().await?,ProcessTermination::Exited) }
            }
        } else {
            tokio::select! {
                status=child.wait()=>(status?,ProcessTermination::Exited),
                _=cancellation.cancelled()=>{ process_tree.terminate(); let _=child.kill().await; (child.wait().await?,ProcessTermination::Cancelled) }
                _=abort_token.cancelled()=>{ process_tree.terminate(); let _=child.kill().await; (child.wait().await?,ProcessTermination::Exited) }
            }
        };
        let transport_silence_ms = read_clock.max_silence_ms(Instant::now());
        observation_stop.cancel();
        if let Some(task) = observation_task {
            let _ = task.await;
        }
        if run.observation_trial_id.is_some() {
            let sample = match process_tree.observation() {
                Ok((
                    user_cpu_100ns,
                    kernel_cpu_100ns,
                    peak_process_memory_bytes,
                    peak_job_memory_bytes,
                )) => CliObservationSample {
                    observed_at: Utc::now().to_rfc3339(),
                    elapsed_ms: observation_started.elapsed().as_millis() as u64,
                    user_cpu_100ns: Some(user_cpu_100ns),
                    kernel_cpu_100ns: Some(kernel_cpu_100ns),
                    peak_process_memory_bytes: Some(peak_process_memory_bytes),
                    peak_job_memory_bytes: Some(peak_job_memory_bytes),
                },
                Err(_) => CliObservationSample {
                    observed_at: Utc::now().to_rfc3339(),
                    elapsed_ms: observation_started.elapsed().as_millis() as u64,
                    user_cpu_100ns: None,
                    kernel_cpu_100ns: None,
                    peak_process_memory_bytes: None,
                    peak_job_memory_bytes: None,
                },
            };
            let _ = self
                .repository
                .record_cli_observation_sample(&run.id, &sample);
        }
        let timing_at_termination = watchdog
            .as_ref()
            .map(ProgressWatchdog::diagnostic)
            .unwrap_or_else(|| "unbounded observation trial".into());
        stdout_task.await.map_err(|error| {
            TerminalError::Process(format!("leitor stdout interrompido: {error}"))
        })?;
        stderr_task.await.map_err(|error| {
            TerminalError::Process(format!("leitor stderr interrompido: {error}"))
        })?;
        *output_completion
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(termination == ProcessTermination::Exited && status.success());
        drop(stream_sender);
        let process_events = coordinator_task.await.map_err(|error| {
            TerminalError::Process(format!("coordenador de eventos interrompido: {error}"))
        })??;
        if let Some(reason) = *abort_reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            return Err(match reason {
                ProcessAbortReason::OutputLimit => TerminalError::OutputLimit,
                // The coordinator's own error, with the observed count, wins
                // above; this arm only keeps the match total.
                ProcessAbortReason::OutputBudget => TerminalError::OutputBudgetExceeded {
                    observed: output_ceiling.unwrap_or(0),
                    ceiling: output_ceiling.unwrap_or(0),
                },
                ProcessAbortReason::Persistence => {
                    TerminalError::Process("stream persistence was interrupted".into())
                }
                ProcessAbortReason::ModelIntegrity => TerminalError::ModelUnverified {
                    detail: "stream stopped by the model identity guard".into(),
                },
                ProcessAbortReason::ProviderFailure => {
                    TerminalError::Process("stream stopped after a provider error".into())
                }
            });
        }
        let _ = self
            .repository
            .set_transport_silence(&run.id, transport_silence_ms);
        self.emit(
            run, self.repository.next_event_sequence(&run.id)?, TerminalEventKind::System,
            None, &format!("CLI timing: {}; max_transport_silence_ms={transport_silence_ms} max_queue_delay_ms={} max_persist_ms={}; termination={termination:?}", timing_at_termination, process_events.max_queue_delay_ms, process_events.max_persist_ms),
        )?;
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
        if termination == ProcessTermination::Exited && !status.success() {
            let setup_error = match failure_kind {
                adapters::CliFailureKind::AuthenticationRequired => Some(
                    TerminalError::AuthRequired(spec_backend(run).label().into()),
                ),
                adapters::CliFailureKind::VersionUnsupported => Some(
                    TerminalError::VersionUnsupported(spec_backend(run).label().into()),
                ),
                _ => None,
            };
            if let Some(error) = setup_error {
                self.invalidate_discovery_after_failure(spec_backend(run), &error);
            }
        }
        let (terminal_status, error_code, error_message, event_kind, final_message) =
            match termination {
                ProcessTermination::Cancelled => (
                    TerminalRunStatus::Cancelled,
                    Some(cancellation_code),
                    Some("The run was cancelled."),
                    TerminalEventKind::Warning,
                    "Run cancelled.",
                ),
                ProcessTermination::TimedOut(reason) => (
                    TerminalRunStatus::Failed,
                    // Keep the two apart. Collapsing them into `cli_timeout`
                    // told the developer at the checkpoint that a run had
                    // exceeded its four-minute ceiling when it had actually
                    // gone quiet for ninety seconds while the model reasoned -
                    // the same word for opposite problems.
                    Some(reason.as_str()),
                    Some(reason.description()),
                    TerminalEventKind::Error,
                    match reason {
                        TimeoutReason::Inactivity => "Run stopped for inactivity.",
                        TimeoutReason::MaxCeiling => "Run stopped at the timeout.",
                    },
                ),
                ProcessTermination::Exited if status.success() => (
                    TerminalRunStatus::Completed,
                    None,
                    None,
                    TerminalEventKind::Completed,
                    "Run completed.",
                ),
                ProcessTermination::Exited => (
                    TerminalRunStatus::Failed,
                    Some(failure_kind.code()),
                    Some("The CLI exited with a non-zero code."),
                    TerminalEventKind::Error,
                    "Run ended with a failure.",
                ),
            };
        self.repository.set_run_status(
            &run.id,
            terminal_status,
            status.code(),
            error_code,
            error_message,
        )?;
        self.record_quota_boundary(run, crate::quota::SampleKind::RunEnd);
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
            finish_reason: process_events.finish_reason,
        })
    }

    fn record_process_failure(&self, run: &TerminalRun, error: &TerminalError) {
        if let Some(backend) = run.backend {
            self.invalidate_discovery_after_failure(backend, error);
        }
        let current = self.repository.get_run(&run.id).ok();
        if current.is_some_and(|current| current.status.is_active()) {
            // Only a run that recorded a start sample gets an end sample.
            self.record_quota_boundary(run, crate::quota::SampleKind::RunEnd);
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

/// Longest wait between two successful reads of a CLI's pipes (stdout or
/// stderr), from spawn to exit - the signal the silence watchdog acts on.
/// Audited frame timestamps only approximate it: a frame is persisted once a
/// whole line arrives. Kept for every run, so trial runs (which have no
/// watchdog) are measured the same way.
#[derive(Debug)]
struct TransportReadClock {
    state: Mutex<(Instant, u64)>,
}

impl TransportReadClock {
    fn new() -> Self {
        Self {
            state: Mutex::new((Instant::now(), 0)),
        }
    }

    fn record_read(&self) {
        let now = Instant::now();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.1 = state.1.max(now.duration_since(state.0).as_millis() as u64);
        state.0 = now;
    }

    /// Includes the tail from the last read to `until` (the process exit).
    fn max_silence_ms(&self, until: Instant) -> u64 {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .1
            .max(until.saturating_duration_since(state.0).as_millis() as u64)
    }
}

async fn read_process_stream<R: AsyncRead + Unpin>(
    stream: &'static str,
    mut reader: R,
    sender: mpsc::Sender<ProcessStreamItem>,
    watchdog: Option<ProgressWatchdog>,
    backend: CliBackend,
    read_clock: Arc<TransportReadClock>,
) {
    let mut buffer = [0u8; 8192];
    let mut pending = Vec::new();
    let mut received_bytes = 0usize;
    loop {
        let read = match reader.read(&mut buffer).await {
            Ok(value) => value,
            Err(error) => {
                let _ = sender
                    .send(ProcessStreamItem::ReadError {
                        stream,
                        message: error.to_string(),
                        received_at: Instant::now(),
                    })
                    .await;
                break;
            }
        };
        if read == 0 {
            if !pending.is_empty() {
                if let Some(watchdog) = watchdog.as_ref() {
                    observe_received_frame(watchdog, backend, stream, &pending);
                }
                let _ = sender
                    .send(ProcessStreamItem::Frame {
                        stream,
                        received_at: Instant::now(),
                        bytes: pending,
                    })
                    .await;
            }
            break;
        }
        received_bytes = received_bytes.saturating_add(read);
        if received_bytes > MAX_RUN_OUTPUT_BYTES {
            let _ = sender
                .send(ProcessStreamItem::ReadError {
                    stream,
                    message: "CLI output exceeded the safe complete-stream limit".into(),
                    received_at: Instant::now(),
                })
                .await;
            return;
        }
        read_clock.record_read();
        if let Some(watchdog) = watchdog.as_ref() {
            watchdog.record_activity();
        }
        pending.extend_from_slice(&buffer[..read]);
        while let Some(index) = pending.iter().position(|byte| *byte == b'\n') {
            let frame = pending.drain(..=index).collect::<Vec<_>>();
            if let Some(watchdog) = watchdog.as_ref() {
                observe_received_frame(watchdog, backend, stream, &frame);
            }
            if sender
                .send(ProcessStreamItem::Frame {
                    stream,
                    bytes: frame,
                    received_at: Instant::now(),
                })
                .await
                .is_err()
            {
                return;
            }
        }
        if pending.len() > MAX_PENDING_STREAM_BYTES {
            let _ = sender
                .send(ProcessStreamItem::ReadError {
                    stream,
                    message: "CLI frame exceeded the safe complete-frame limit".into(),
                    received_at: Instant::now(),
                })
                .await;
            return;
        }
    }
}

/// Classify before the bounded queue. Stderr and unrecognized bookkeeping
/// frames cannot turn a reasoning phase into an ordinary silence window.
/// Exception: Gemini CLI prints 503/backoff messages to stderr while it
/// sleeps between retries. Treating those as a reasoning signal widens the
/// silence window from 90 s to 270 s, preventing a premature
/// `cli_inactivity_timeout` during the backoff sleep.
fn observe_received_frame(
    watchdog: &ProgressWatchdog,
    backend: CliBackend,
    stream: &str,
    bytes: &[u8],
) {
    if stream != "stdout" {
        if backend == CliBackend::Gemini
            && let Ok(text) = std::str::from_utf8(bytes)
        {
            let lower = text.to_ascii_lowercase();
            if lower.contains("503")
                || lower.contains("service unavailable")
                || lower.contains("overloaded")
                || lower.contains("retrying")
            {
                watchdog.set_reasoning(true);
            }
        }
        return;
    }
    let frame = adapters::normalize_frame(backend, stream, bytes);
    if frame.is_reasoning {
        watchdog.set_reasoning(true);
    } else if frame.is_text
        || matches!(
            frame.kind,
            TerminalEventKind::ToolUse
                | TerminalEventKind::ToolResult
                | TerminalEventKind::Completed
        )
    {
        watchdog.set_reasoning(false);
    }
}

#[cfg(test)]
async fn coordinate_process_events(
    repository: TerminalRepository,
    event_sink: Arc<dyn TerminalEventSink>,
    run: TerminalRun,
    backend: CliBackend,
    receiver: mpsc::Receiver<ProcessStreamItem>,
    abort_token: CancellationToken,
    abort_reason: Arc<Mutex<Option<ProcessAbortReason>>>,
    watchdog: ProgressWatchdog,
    output_ceiling: Option<u64>,
) -> Result<ProcessEventSummary, TerminalError> {
    coordinate_process_events_with_completion(
        repository,
        event_sink,
        run,
        backend,
        receiver,
        abort_token,
        abort_reason,
        watchdog,
        output_ceiling,
        None,
    )
    .await
}

#[cfg(test)]
fn coordinate_process_events_blocking(
    repository: TerminalRepository,
    event_sink: Arc<dyn TerminalEventSink>,
    run: TerminalRun,
    backend: CliBackend,
    receiver: mpsc::Receiver<ProcessStreamItem>,
    abort_token: CancellationToken,
    abort_reason: Arc<Mutex<Option<ProcessAbortReason>>>,
    codex_sessions_root: Option<PathBuf>,
    output_ceiling: Option<u64>,
) -> Result<ProcessEventSummary, TerminalError> {
    coordinate_process_events_blocking_inner(
        repository,
        event_sink,
        run,
        backend,
        receiver,
        abort_token,
        abort_reason,
        codex_sessions_root,
        output_ceiling,
        None,
    )
}

async fn coordinate_process_events_with_completion(
    repository: TerminalRepository,
    event_sink: Arc<dyn TerminalEventSink>,
    run: TerminalRun,
    backend: CliBackend,
    receiver: mpsc::Receiver<ProcessStreamItem>,
    abort_token: CancellationToken,
    abort_reason: Arc<Mutex<Option<ProcessAbortReason>>>,
    _watchdog: ProgressWatchdog,
    output_ceiling: Option<u64>,
    output_completion: Option<Arc<Mutex<Option<bool>>>>,
) -> Result<ProcessEventSummary, TerminalError> {
    let failure_token = abort_token.clone();
    let failure_reason = abort_reason.clone();
    let result = tokio::task::spawn_blocking(move || {
        coordinate_process_events_blocking_inner(
            repository,
            event_sink,
            run,
            backend,
            receiver,
            abort_token,
            abort_reason,
            None,
            output_ceiling,
            output_completion,
        )
    })
    .await
    .map_err(|error| TerminalError::Process(format!("CLI audit worker interrupted: {error}")))
    .and_then(|result| result);
    if result.is_err() {
        signal_process_abort(
            &failure_token,
            &failure_reason,
            ProcessAbortReason::Persistence,
        );
    }
    result
}

fn coordinate_process_events_blocking_inner(
    repository: TerminalRepository,
    event_sink: Arc<dyn TerminalEventSink>,
    run: TerminalRun,
    backend: CliBackend,
    mut receiver: mpsc::Receiver<ProcessStreamItem>,
    abort_token: CancellationToken,
    abort_reason: Arc<Mutex<Option<ProcessAbortReason>>>,
    codex_sessions_root: Option<PathBuf>,
    output_ceiling: Option<u64>,
    output_completion: Option<Arc<Mutex<Option<bool>>>>,
) -> Result<ProcessEventSummary, TerminalError> {
    let host_owner = repository.host.clone();
    let host = host_owner.as_ref();
    let mut writer = AuditWriter {
        repository: &repository,
        event_sink: &event_sink,
        run: &run,
        sequence: repository.next_event_sequence(&run.id)?,
        raw_output_delta_active: false,
        pending: None,
        held: Vec::new(),
    };
    let mut event_count = 0usize;
    let mut observed_bytes = 0usize;
    let mut summary = ProcessEventSummary::default();
    let mut incomplete_stream = false;
    let mut codex_session_id: Option<String> = None;
    let mut persisted_session_id: Option<String> = None;
    let usage = host
        .create_usage_recorder(
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
    loop {
        let item = if writer.pending.is_none() {
            match receiver.blocking_recv() {
                Some(item) => item,
                None => break,
            }
        } else {
            // Buffered deltas reach the database once the stream pauses for a
            // window, so a quiet model never leaves its last words unsaved.
            match receiver.try_recv() {
                Ok(item) => item,
                Err(mpsc::error::TryRecvError::Empty) => {
                    if writer.pending_expired() {
                        if let Err(error) = writer.flush(&mut summary) {
                            signal_process_abort(
                                &abort_token,
                                &abort_reason,
                                ProcessAbortReason::Persistence,
                            );
                            return Err(error);
                        }
                    } else {
                        std::thread::sleep(DELTA_IDLE_POLL);
                    }
                    continue;
                }
                Err(mpsc::error::TryRecvError::Disconnected) => break,
            }
        };
        let received_at = match &item {
            ProcessStreamItem::Frame { received_at, .. }
            | ProcessStreamItem::ReadError { received_at, .. } => *received_at,
        };
        summary.max_queue_delay_ms = summary
            .max_queue_delay_ms
            .max(received_at.elapsed().as_millis());
        let persistence_started = Instant::now();
        let (frame, stream, input_bytes) = match item {
            ProcessStreamItem::Frame { stream, bytes, .. } => {
                let input_bytes = bytes.len();
                if stream == "stdout"
                    && backend == CliBackend::Claude
                    && let Some(quota) = &repository.quota
                {
                    quota.observe_claude_frame(&bytes);
                }
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
                if stream == "stdout"
                    && let Some(ceiling) = output_ceiling
                {
                    let observed = usage.output_tokens_so_far();
                    if observed > ceiling {
                        // Stop first: every further token is billed. The
                        // buffered deltas and the partial usage record are
                        // best-effort, the abort is not.
                        signal_process_abort(
                            &abort_token,
                            &abort_reason,
                            ProcessAbortReason::OutputBudget,
                        );
                        let _ = writer.flush(&mut summary);
                        let _ = usage.record_live_partial();
                        return Err(TerminalError::OutputBudgetExceeded { observed, ceiling });
                    }
                }
                (
                    adapters::normalize_frame(backend, stream, &bytes),
                    stream,
                    input_bytes,
                )
            }
            ProcessStreamItem::ReadError {
                stream, message, ..
            } => {
                incomplete_stream = true;
                let input_bytes = message.len();
                (
                    adapters::NormalizedCliFrame {
                        kind: TerminalEventKind::Error,
                        content: message,
                        native_session_id: None,
                        observed_model: None,
                        message_id: None,
                        parent_invocation_id: None,
                        agent_id: None,
                        is_model_generation: false,
                        is_text: false,
                        is_delta: false,
                        finish_reason: None,
                        is_reasoning: false,
                    },
                    stream,
                    input_bytes,
                )
            }
        };
        let adapters::NormalizedCliFrame {
            kind,
            content,
            native_session_id,
            mut observed_model,
            message_id,
            parent_invocation_id,
            agent_id,
            is_model_generation,
            is_text,
            is_delta,
            finish_reason,
            is_reasoning: _,
        } = frame;
        // A frame that names a model opens a generation: it and everything
        // after it wait for identity checks, so it is never buffered and the
        // deltas before it are written first.
        let coalescible = is_delta
            && kind == TerminalEventKind::Message
            && !is_model_generation
            && observed_model.is_none();
        if writer
            .pending
            .as_ref()
            .is_some_and(|pending| !coalescible || !pending.accepts(stream, is_text))
            && let Err(error) = writer.flush(&mut summary)
        {
            signal_process_abort(&abort_token, &abort_reason, ProcessAbortReason::Persistence);
            return Err(error);
        }
        // `agy` names only the model it resolved at start-up; recorded apart so
        // an audit can tell it from a model reported per generation.
        let mut model_evidence_source = if backend == CliBackend::Antigravity {
            "cli_init"
        } else {
            "stream_event"
        };
        let mut codex_turn_models = None;
        // The last reason the provider gave wins: a turn that ends on `length`
        // has been cut off even if earlier turns ended normally.
        if let Some(reason) = finish_reason {
            summary.finish_reason = Some(reason);
        }
        if let Some(session_id) = native_session_id {
            // Claude repeats its session id on every frame; one write is enough.
            if persisted_session_id.as_deref() != Some(session_id.as_str()) {
                if let Err(error) = repository.set_native_session_id(&run.id, &session_id) {
                    signal_process_abort(
                        &abort_token,
                        &abort_reason,
                        ProcessAbortReason::Persistence,
                    );
                    return Err(error);
                }
                persisted_session_id = Some(session_id.clone());
            }
            if backend == CliBackend::Codex {
                codex_session_id = Some(session_id);
            }
        }
        if backend == CliBackend::Codex
            && is_model_generation
            && observed_model.is_none()
            && let Some(session_id) = codex_session_id.as_deref()
        {
            for probe in 0..20 {
                let evidence = if let Some(root) = codex_sessions_root.as_deref() {
                    codex_model_provenance::session_model_from_root(
                        root,
                        session_id,
                        &run.created_at,
                    )
                } else {
                    codex_model_provenance::current_session_model(session_id, &run.created_at)
                };
                if let Some(evidence) = evidence {
                    observed_model = Some(evidence.latest_model);
                    codex_turn_models = Some(evidence.turn_models);
                    model_evidence_source = "codex_turn_context";
                    break;
                }
                if probe < 19 {
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
        if observed_model.is_some() || is_model_generation {
            summary.model_observation_count = summary.model_observation_count.saturating_add(1);
            if let Err(error) = repository.record_model_observation(
                &run.id,
                summary.model_observation_count,
                writer.sequence,
                observed_model.as_deref(),
                message_id.as_deref(),
                parent_invocation_id.as_deref(),
                agent_id.as_deref(),
                is_model_generation,
                model_evidence_source,
            ) {
                signal_process_abort(&abort_token, &abort_reason, ProcessAbortReason::Persistence);
                return Err(error);
            }
        }
        if let Some(turn_models) = codex_turn_models {
            for model in turn_models {
                let error = match model_identity::verify_model(
                    Some(backend.as_str()),
                    run.model.as_deref(),
                    Some(&model),
                ) {
                    model_identity::ModelVerification::Verified => continue,
                    model_identity::ModelVerification::Mismatch => TerminalError::ModelMismatch {
                        expected: run.model.clone().unwrap_or_else(|| "unselected".into()),
                        observed: model,
                    },
                    model_identity::ModelVerification::Unverified => {
                        TerminalError::ModelUnverified {
                            detail: format!(
                                "Codex turn context reported an unverifiable model: {model}"
                            ),
                        }
                    }
                };
                signal_process_abort(
                    &abort_token,
                    &abort_reason,
                    ProcessAbortReason::ModelIntegrity,
                );
                return Err(error);
            }
        }
        if is_model_generation {
            summary.model_generation_count = summary.model_generation_count.saturating_add(1);
            let Some(model) = observed_model.as_deref() else {
                signal_process_abort(
                    &abort_token,
                    &abort_reason,
                    ProcessAbortReason::ModelIntegrity,
                );
                return Err(TerminalError::ModelUnverified {
                    detail: format!(
                        "backend={}, messageId={}, agentId={}",
                        backend.as_str(),
                        message_id.as_deref().unwrap_or("unknown"),
                        agent_id.as_deref().unwrap_or("unknown")
                    ),
                });
            };
            if let Err(error) = repository.set_observed_model(&run.id, model) {
                signal_process_abort(&abort_token, &abort_reason, ProcessAbortReason::Persistence);
                return Err(error);
            }
            match model_identity::verify_model(
                Some(backend.as_str()),
                run.model.as_deref(),
                Some(model),
            ) {
                model_identity::ModelVerification::Verified => {
                    summary.verified_model_generation_count =
                        summary.verified_model_generation_count.saturating_add(1);
                }
                model_identity::ModelVerification::Mismatch => {
                    signal_process_abort(
                        &abort_token,
                        &abort_reason,
                        ProcessAbortReason::ModelIntegrity,
                    );
                    return Err(TerminalError::ModelMismatch {
                        expected: run.model.clone().unwrap_or_else(|| "unselected".into()),
                        observed: model.to_owned(),
                    });
                }
                model_identity::ModelVerification::Unverified => {
                    signal_process_abort(
                        &abort_token,
                        &abort_reason,
                        ProcessAbortReason::ModelIntegrity,
                    );
                    return Err(TerminalError::ModelUnverified {
                        detail: format!(
                            "backend={}, requested={}, observed={}",
                            backend.as_str(),
                            run.model.as_deref().unwrap_or("default"),
                            model
                        ),
                    });
                }
            }
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
        let written = if coalescible {
            writer.buffer(stream, is_text, &content, &mut summary)
        } else {
            writer.write(kind, stream, is_text, is_delta, &content, &mut summary)
        };
        if let Err(error) = written {
            signal_process_abort(&abort_token, &abort_reason, ProcessAbortReason::Persistence);
            return Err(error);
        }
        if backend == CliBackend::Claude && kind == TerminalEventKind::Error {
            // An error terminates a possibly incomplete stream. Discard every
            // held fragment; only a host-owned classification may leave memory.
            signal_process_abort(
                &abort_token,
                &abort_reason,
                ProcessAbortReason::ProviderFailure,
            );
            let error = match adapters::classify_failure(backend, None, &content) {
                adapters::CliFailureKind::AuthenticationRequired => {
                    TerminalError::AuthRequired(backend.label().into())
                }
                adapters::CliFailureKind::VersionUnsupported => {
                    TerminalError::VersionUnsupported(backend.label().into())
                }
                adapters::CliFailureKind::RateLimited => {
                    TerminalError::RateLimited(backend.label().into())
                }
                adapters::CliFailureKind::ProcessFailed => TerminalError::Process(
                    "The CLI reported a provider failure; incomplete output was withheld.".into(),
                ),
            };
            let safe_diagnostic = error.to_string();
            persist_and_emit(
                &repository,
                &event_sink,
                &run,
                repository.next_event_sequence(&run.id)?,
                TerminalEventKind::Error,
                Some(stream),
                &safe_diagnostic,
            )?;
            return Err(error);
        }
        summary.max_persist_ms = summary
            .max_persist_ms
            .max(persistence_started.elapsed().as_millis());
    }
    if let Err(error) = writer.flush(&mut summary) {
        signal_process_abort(&abort_token, &abort_reason, ProcessAbortReason::Persistence);
        return Err(error);
    }
    if output_completion.as_ref().is_some_and(|completion| {
        *completion
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            != Some(true)
    }) {
        let failure = adapters::classify_failure(backend, None, &summary.last_stderr);
        summary.raw_output.clear();
        summary.last_stderr = match failure {
            adapters::CliFailureKind::AuthenticationRequired => "authentication required",
            adapters::CliFailureKind::VersionUnsupported => "unsupported version",
            adapters::CliFailureKind::RateLimited => "rate limit reached",
            adapters::CliFailureKind::ProcessFailed => {
                "CLI output withheld after incomplete execution"
            }
        }
        .into();
        usage.mark_partial()?;
        return Ok(summary);
    }
    if !summary.raw_output.trim().is_empty() && summary.model_generation_count == 0 {
        signal_process_abort(
            &abort_token,
            &abort_reason,
            ProcessAbortReason::ModelIntegrity,
        );
        return Err(TerminalError::ModelUnverified {
            detail: format!(
                "backend={} emitted output without a generation identity",
                backend.as_str()
            ),
        });
    }
    if summary.model_generation_count != summary.verified_model_generation_count {
        signal_process_abort(
            &abort_token,
            &abort_reason,
            ProcessAbortReason::ModelIntegrity,
        );
        return Err(TerminalError::ModelUnverified {
            detail: format!(
                "backend={} has an unverified model generation",
                backend.as_str()
            ),
        });
    }
    if incomplete_stream {
        usage.mark_partial()?;
        return Err(TerminalError::Process(
            "CLI output was incomplete; content was withheld.".into(),
        ));
    }
    if let Some(reason) = summary
        .finish_reason
        .as_ref()
        .filter(|reason| adapters::is_output_truncation(reason))
    {
        usage.mark_partial()?;
        return Err(TerminalError::OutputTruncated {
            backend: backend.label().into(),
            reason: reason.clone(),
            produced_chars: summary.raw_output.chars().count(),
        });
    }
    writer.finish(&mut summary)?;
    Ok(summary)
}

fn signal_process_abort(
    abort_token: &CancellationToken,
    abort_reason: &Mutex<Option<ProcessAbortReason>>,
    reason: ProcessAbortReason,
) {
    abort_reason
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get_or_insert(reason);
    abort_token.cancel();
}

/// Stream deltas are persisted together: one row per window of stream instead
/// of one per frame. One Claude Test run wrote 12,166 rows, a write
/// transaction and three connections each, and its end trailed the process by
/// 7.8 s while the queue drained ("Haiku CLI teste 1", 2026-09-26).
const DELTA_COALESCE_WINDOW: Duration = Duration::from_millis(250);
/// Upper bound of one coalesced row, well under `MAX_RUN_OUTPUT_BYTES`.
const DELTA_COALESCE_MAX_BYTES: usize = 16 * 1024;
/// Poll interval while deltas wait for the window to close.
const DELTA_IDLE_POLL: Duration = Duration::from_millis(10);

/// Consecutive deltas of one stream and one kind of content.
struct PendingDeltas {
    stream: &'static str,
    is_text: bool,
    content: String,
    started: Instant,
}

impl PendingDeltas {
    fn accepts(&self, stream: &str, is_text: bool) -> bool {
        self.stream == stream
            && self.is_text == is_text
            && self.content.len() < DELTA_COALESCE_MAX_BYTES
    }
}

struct HeldAuditEvent {
    kind: TerminalEventKind,
    stream: String,
    is_text: bool,
    content: String,
}

/// Coalesces content in memory. Publication waits for a complete successful
/// stream, so a secret split over arbitrary frames or timer windows is scanned
/// before any fragment reaches SQLite or subscribers.
struct AuditWriter<'a> {
    repository: &'a TerminalRepository,
    event_sink: &'a Arc<dyn TerminalEventSink>,
    run: &'a TerminalRun,
    /// The sequence the next persisted event receives.
    sequence: u64,
    raw_output_delta_active: bool,
    pending: Option<PendingDeltas>,
    held: Vec<HeldAuditEvent>,
}

impl AuditWriter<'_> {
    fn pending_expired(&self) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|pending| pending.started.elapsed() >= DELTA_COALESCE_WINDOW)
    }

    fn buffer(
        &mut self,
        stream: &'static str,
        is_text: bool,
        content: &str,
        summary: &mut ProcessEventSummary,
    ) -> Result<(), TerminalError> {
        let pending = self.pending.get_or_insert_with(|| PendingDeltas {
            stream,
            is_text,
            content: String::new(),
            started: Instant::now(),
        });
        // Text deltas continue one another; raw frames stay one per line.
        if !is_text && !pending.content.is_empty() {
            pending.content.push('\n');
        }
        pending.content.push_str(content);
        if pending.content.len() >= DELTA_COALESCE_MAX_BYTES || self.pending_expired() {
            self.flush(summary)?;
        }
        Ok(())
    }

    fn flush(&mut self, summary: &mut ProcessEventSummary) -> Result<(), TerminalError> {
        match self.pending.take() {
            Some(pending) => self.write(
                TerminalEventKind::Message,
                pending.stream,
                pending.is_text,
                true,
                &pending.content,
                summary,
            ),
            None => Ok(()),
        }
    }

    fn write(
        &mut self,
        kind: TerminalEventKind,
        stream: &str,
        is_text: bool,
        is_delta: bool,
        content: &str,
        summary: &mut ProcessEventSummary,
    ) -> Result<(), TerminalError> {
        let metadata;
        let content = if !is_text && stream == "stdout" {
            metadata = format!("CLI {} received.", kind.as_str());
            metadata.as_str()
        } else {
            content
        };
        self.held.push(HeldAuditEvent {
            kind,
            stream: stream.into(),
            is_text,
            content: content.into(),
        });
        if is_text
            && matches!(
                kind,
                TerminalEventKind::Completed
                    | TerminalEventKind::Message
                    | TerminalEventKind::Stdout
            )
        {
            let delta = is_delta && kind == TerminalEventKind::Message;
            if delta && self.raw_output_delta_active {
                summary.raw_output.push_str(content);
            } else {
                summary.raw_output = content.into();
                self.raw_output_delta_active = delta;
            }
        }
        if stream == "stderr" {
            summary.last_stderr = content.into();
        }
        self.sequence = self.sequence.saturating_add(1);
        Ok(())
    }

    fn finish(&mut self, summary: &mut ProcessEventSummary) -> Result<(), TerminalError> {
        let segments = self
            .held
            .iter()
            .map(|event| {
                (
                    if event.stream == "stdout" && !event.is_text {
                        "stdout-metadata"
                    } else {
                        event.stream.as_str()
                    },
                    event.content.as_str(),
                )
            })
            .collect::<Vec<_>>();
        let sanitized = redact_continuous_segments(self.repository.host.as_ref(), &segments)?;
        let mut sequence = self.sequence.saturating_sub(self.held.len() as u64);
        summary.last_stderr.clear();
        // raw_output is the operational response assembled from the original
        // decoded segments. Only the observer projection below is persisted
        // and emitted; replacing raw_output here would corrupt edit literals.
        for (held, redacted) in self.held.iter().zip(sanitized) {
            let event = self.repository.append_redacted_event(
                &self.run.id,
                sequence,
                held.kind,
                Some(&held.stream),
                redacted,
            )?;
            let run = self.repository.get_run(&self.run.id)?;
            self.event_sink.emit(&TerminalEventEnvelope {
                protocol_version: 2,
                project_id: run.project_id.clone(),
                agent_role: run.agent_role,
                run_id: run.id.clone(),
                sequence,
                run,
                event: event.clone(),
            });
            if held.stream == "stderr" {
                summary.last_stderr.clone_from(&event.content);
            }
            sequence = sequence.saturating_add(1);
        }
        self.held.clear();
        Ok(())
    }
}

/// Redacts each stream as a continuous value and projects the safe text back
/// onto its original event boundaries. A marker belongs to the event where the
/// occurrence began; continuation events never retain the secret's suffix.
/// Decoded JSON can change offsets: its complete safe projection belongs to
/// the first segment, while later segments of that stream become empty.
fn redact_continuous_segments(
    host: &dyn TerminalHost,
    segments: &[(&str, &str)],
) -> Result<Vec<RedactionResult>, TerminalError> {
    let mut streams: HashMap<&str, String> = HashMap::new();
    let mut ranges = Vec::with_capacity(segments.len());
    let mut total = 0usize;
    for (stream, content) in segments {
        total = total.saturating_add(content.len());
        if total > MAX_RUN_OUTPUT_BYTES {
            return Err(TerminalError::OutputLimit);
        }
        let text = streams.entry(stream).or_default();
        let start = text.len();
        text.push_str(content);
        ranges.push((start, text.len()));
    }
    let redactions = streams
        .iter()
        .map(|(stream, text)| {
            if serde_json::from_str::<Value>(text).is_ok() {
                host.sanitize_model_text_counted(text)
                    .map(|redacted| (*stream, (Some(redacted), Vec::new())))
                    .map_err(|_| TerminalError::Redaction("CLI JSON sanitization failed.".into()))
            } else {
                host.redact_context(text)
                    .map(|redacted| (*stream, (None, redacted.spans)))
                    .map_err(TerminalError::Redaction)
            }
        })
        .collect::<Result<HashMap<_, _>, _>>()?;
    let occurrence_count = redactions.values().fold(0u64, |count, (json, spans)| {
        count.saturating_add(json.as_ref().map_or(spans.len() as u64, |json| json.count))
    });
    if occurrence_count > MAX_REDACTION_SPANS as u64 {
        return Err(TerminalError::Redaction(
            "CLI sanitization occurrence limit exceeded.".into(),
        ));
    }
    let mut projected_json_streams = HashSet::new();
    segments
        .iter()
        .zip(ranges)
        .map(|((stream, _), (start, end))| {
            let original = &streams[stream];
            let (json, spans) = &redactions[stream];
            if let Some(json) = json {
                return Ok(if projected_json_streams.insert(*stream) {
                    json.clone()
                } else {
                    RedactionResult {
                        text: String::new(),
                        count: 0,
                        categories: Vec::new(),
                    }
                });
            }
            let mut cursor = start;
            let mut text = String::new();
            let mut count = 0u64;
            let mut categories = Vec::new();
            for span in spans
                .iter()
                .filter(|span| span.start_byte < end && span.end_byte > start)
            {
                if cursor < span.start_byte {
                    text.push_str(&original[cursor..span.start_byte.min(end)]);
                }
                if span.start_byte >= start {
                    text.push_str(&span.marker);
                    count = count.saturating_add(1);
                    if !categories.contains(&span.category) {
                        categories.push(span.category.clone());
                    }
                }
                text.extend(
                    original[span.start_byte.max(start)..span.end_byte.min(end)]
                        .chars()
                        .filter(|character| matches!(*character, '\r' | '\n')),
                );
                cursor = cursor.max(span.end_byte.min(end));
            }
            if cursor < end {
                text.push_str(&original[cursor..end]);
            }
            Ok(RedactionResult {
                text,
                count,
                categories,
            })
        })
        .collect()
}

/// Earlier events a ranged read sanitizes along with its page, so a secret
/// split across the page boundary is still redacted.
pub const RANGE_CONTEXT_EVENTS: u64 = 64;

/// Whether the run was recorded under a known sanitization policy.
fn run_has_known_policy(connection: &Connection, run_id: &str) -> Result<bool, TerminalError> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM secret_sanitization_receipts receipt
         JOIN terminal_runs run ON run.id=?1 AND receipt.project_id=run.project_id
         WHERE receipt.call_id=run.id OR receipt.call_id=run.llm_execution_id)",
        [run_id],
        |row| row.get(0),
    )?)
}

/// Events after `after_sequence`, in order, through the run/sequence index.
fn read_run_events_range(
    connection: &Connection,
    run_id: &str,
    after_sequence: u64,
    limit: usize,
) -> Result<Vec<TerminalEvent>, TerminalError> {
    let mut statement = connection.prepare_cached(
        "SELECT id,run_id,sequence,kind,stream,content,created_at,
                redaction_count,redaction_categories_json FROM terminal_events
         WHERE run_id=?1 AND sequence>?2 ORDER BY sequence LIMIT ?3",
    )?;
    let rows = statement.query_map(
        params![required(run_id, "runId")?, after_sequence, limit as i64],
        map_terminal_event,
    )?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(TerminalError::from)
}

fn read_run_history_events(
    connection: &Connection,
    run_id: &str,
) -> Result<Vec<TerminalEvent>, TerminalError> {
    let mut statement = connection.prepare(
        "SELECT id,run_id,sequence,kind,stream,content,created_at,
                redaction_count,redaction_categories_json FROM terminal_events
         WHERE run_id=?1 ORDER BY sequence LIMIT ?2",
    )?;
    let rows = statement.query_map(
        params![required(run_id, "runId")?, MAX_RUN_EVENTS + 1],
        map_terminal_event,
    )?;
    let events = rows.collect::<Result<Vec<_>, _>>()?;
    if events.len() > MAX_RUN_EVENTS {
        return Err(TerminalError::OutputLimit);
    }
    Ok(events)
}

fn map_terminal_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<TerminalEvent> {
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
}

/// Both export digests cover the full v1 envelope without the `sanitization`
/// member. Sort recursively so verification does not depend on `serde_json`'s
/// map implementation; arrays retain their evidence order.
fn canonicalize_terminal_export(value: &mut Value) {
    match value {
        Value::Object(object) => {
            let mut entries = std::mem::take(object).into_iter().collect::<Vec<_>>();
            entries.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
            for (key, mut child) in entries {
                canonicalize_terminal_export(&mut child);
                object.insert(key, child);
            }
        }
        Value::Array(items) => {
            for item in items {
                canonicalize_terminal_export(item);
            }
        }
        _ => {}
    }
}

/// Bound encoded bytes too: JSON escaping and pretty-printing can exceed the
/// limit even when the underlying text fits. No partial payload is published.
fn terminal_export_bytes(
    value: &Value,
    pretty: bool,
    limit: usize,
) -> Result<Vec<u8>, TerminalError> {
    struct BoundedExport {
        bytes: Vec<u8>,
        limit: usize,
        exceeded: bool,
    }
    impl std::io::Write for BoundedExport {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
                self.exceeded = true;
                return Err(std::io::Error::other("terminal export exceeded byte limit"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = BoundedExport {
        bytes: Vec::new(),
        limit,
        exceeded: false,
    };
    let result = if pretty {
        serde_json::to_writer_pretty(&mut writer, value)
    } else {
        serde_json::to_writer(&mut writer, value)
    };
    if writer.exceeded {
        return Err(TerminalError::OutputLimit);
    }
    result.map_err(|_| TerminalError::Redaction("Terminal export serialization failed.".into()))?;
    Ok(writer.bytes)
}

fn sanitize_history_events(
    host: &dyn TerminalHost,
    events: &mut [TerminalEvent],
    known_policy: bool,
) -> Result<(), TerminalError> {
    if !known_policy {
        let mut legacy_streams = HashSet::new();
        for event in events
            .iter()
            .filter(|event| event.content.contains("[REDACTED"))
        {
            if let Some(stream) = event.stream.as_deref()
                && events
                    .iter()
                    .filter(|candidate| {
                        candidate.stream.as_deref() == Some(stream) && !candidate.content.is_empty()
                    })
                    .count()
                    > 1
            {
                legacy_streams.insert(stream.to_owned());
            }
        }
        for event in events.iter_mut() {
            if event
                .stream
                .as_deref()
                .is_some_and(|stream| legacy_streams.contains(stream))
            {
                event.content =
                    "[Legacy stream omitted: fragmented redaction cannot be reconstructed safely.]"
                        .into();
            }
        }
    }
    for event in events.iter_mut() {
        if event.stream.as_deref() == Some("stdout")
            && event.content.lines().any(|line| {
                [
                    "Codex: ",
                    "Claude: ",
                    "Gemini: ",
                    "OpenCode: ",
                    "Antigravity: ",
                ]
                .iter()
                .any(|prefix| {
                    line.strip_prefix(prefix)
                        .is_some_and(|value| serde_json::from_str::<Value>(value).is_ok())
                })
            })
        {
            event.content = format!(
                "CLI {} received; historical protocol payload omitted.",
                event.kind.as_str()
            );
        }
    }
    let segments = events
        .iter()
        .map(|event| {
            (
                if event.stream.as_deref() == Some("stdout")
                    && event.content.starts_with("CLI ")
                    && event.content.contains("received")
                {
                    "stdout-metadata"
                } else {
                    event.stream.as_deref().unwrap_or("system")
                },
                event.content.as_str(),
            )
        })
        .collect::<Vec<_>>();
    let sanitized = redact_continuous_segments(host, &segments)?;
    for (event, redacted) in events.iter_mut().zip(sanitized) {
        event.content = redacted.text;
        event.redaction_count = event.redaction_count.saturating_add(redacted.count);
        for category in redacted.categories {
            if !event.redaction_categories.contains(&category) {
                event.redaction_categories.push(category);
            }
        }
    }
    Ok(())
}

fn validate_public_cli_identity(host: &dyn TerminalHost, value: &str) -> Result<(), TerminalError> {
    if !host
        .redact_context(value)
        .map_err(TerminalError::Redaction)?
        .spans
        .is_empty()
    {
        return Err(TerminalError::Validation(
            "CLI identity contained sensitive content".into(),
        ));
    }
    Ok(())
}

fn redact_json_strings(host: &dyn TerminalHost, value: &mut Value) -> Result<(), TerminalError> {
    redact_json_strings_count(host, value).map(|_| ())
}

fn redact_json_strings_count(
    host: &dyn TerminalHost,
    value: &mut Value,
) -> Result<u64, TerminalError> {
    let original = terminal_export_bytes(value, false, MAX_RUN_OUTPUT_BYTES)?;
    let original = std::str::from_utf8(&original)
        .map_err(|_| TerminalError::Redaction("CLI JSON sanitization failed.".into()))?;
    let redacted = host
        .sanitize_model_text_counted(original)
        .map_err(|_| TerminalError::Redaction("CLI JSON sanitization failed.".into()))?;
    if redacted.text.len() > MAX_RUN_OUTPUT_BYTES {
        return Err(TerminalError::OutputLimit);
    }
    *value = serde_json::from_str(&redacted.text)
        .map_err(|_| TerminalError::Redaction("CLI JSON sanitization failed.".into()))?;
    Ok(redacted.count)
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

/// The only host variables a spawned CLI inherits.
pub const SAFE_ENVIRONMENT: [&str; 12] = [
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
];

fn copy_safe_environment(command: &mut Command) {
    for name in SAFE_ENVIRONMENT {
        if let Some(value) = env::var_os(name) {
            command.env(name, value);
        }
    }
}

/// Whether a composed unit must read only the host-selected context of its
/// prompt (ADR-032). Each specialty has exactly one admissible scope
/// (`UnitContextScope::required_for`, ADR-034 T1): a request that names a
/// specialty with any other scope, or names no known specialty, is refused,
/// so a stale or forged dispatch can neither hand a confined worker the
/// workspace snapshot back nor change what any other worker reads.
pub(crate) fn cli_context_scope(request: &LlmRequest) -> Result<bool, TerminalError> {
    let Some(unit) = request.dispatch.unit.as_ref() else {
        return Ok(false);
    };
    let Some(required) = UnitContextScope::required_for(&unit.executor_kind) else {
        return Err(TerminalError::Validation(format!(
            "`{}` is not a composed specialty this transport can scope",
            unit.executor_kind
        )));
    };
    if unit.context_scope != required {
        return Err(TerminalError::Validation(
            if required == UnitContextScope::HostContextOnly {
                format!(
                    "The composed `{}` must run with host-selected context only",
                    unit.executor_kind
                )
            } else {
                format!(
                    "The composed `{}` runs with the workspace snapshot; the dispatch asked for another scope",
                    unit.executor_kind
                )
            },
        ));
    }
    Ok(required == UnitContextScope::HostContextOnly)
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

/// Keeps a hardening artefact on disk for as long as the process it configures runs.
#[derive(Debug)]
enum HardeningGuard {
    SettingsFile(#[allow(dead_code)] tempfile::NamedTempFile),
    ConfigDirectory(#[allow(dead_code)] tempfile::TempDir),
}

fn harden_spec(
    backend: CliBackend,
    mode: Hardening,
    spec: &mut CliProcessSpec,
) -> Result<Option<HardeningGuard>, TerminalError> {
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
            let selected_model = spec
                .args
                .windows(2)
                .find(|pair| pair[0] == "--model")
                .map(|pair| pair[1].clone());
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
            // `--setting-sources ""` leaves Claude's auto-memory on: ~3,000
            // prompt tokens a call (2,549 vs 5,553 cached, 2026-09-26) and a
            // Test worker that spent a turn looking for `memory/MEMORY.md`.
            // Model parameters (effort, thinking budget) belong to the
            // adapter, not to this isolation step.
            spec.environment
                .push(("CLAUDE_CODE_DISABLE_AUTO_MEMORY".into(), "1".into()));
            spec.args.push("--strict-mcp-config".into());
            spec.args.push("--no-chrome".into());
            spec.args.push("--disable-slash-commands".into());
            if let Some(selected_model) = selected_model {
                // Keep Claude's own model picker and fallback chain confined
                // to the selected model; generation events remain authoritative.
                spec.args.extend([
                    "--settings".into(),
                    serde_json::json!({
                        "availableModels": [selected_model],
                        "enforceAvailableModels": true,
                        "fallbackModel": []
                    })
                    .to_string(),
                ]);
            }
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
            settings_guard = Some(HardeningGuard::SettingsFile(settings));
        }
        CliBackend::Antigravity => {
            // `agy` reads MCP servers, plugins, rules and permission grants from
            // its `--gemini_dir`. A private one per launch keeps the developer's
            // configuration out; the login lives in the OS keyring and survives
            // (verified with 1.3.5, 2026-10-14). The deny rules matter more than
            // the isolation: a headless `agy` refuses shell commands by default
            // but writes files without asking.
            let directory = tempfile::Builder::new().prefix("hvc-agy-").tempdir()?;
            isolate_antigravity(directory.path(), mode, spec)?;
            settings_guard = Some(HardeningGuard::ConfigDirectory(directory));
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

/// Points `agy` at `directory` as its whole config root and writes the permission rules the
/// mode allows. Shell commands, URL fetches and every file write are denied; the Companion
/// loses file reads as well.
fn isolate_antigravity(
    directory: &Path,
    mode: Hardening,
    spec: &mut CliProcessSpec,
) -> Result<(), TerminalError> {
    let cli_directory = directory.join("antigravity-cli");
    fs::create_dir_all(&cli_directory)?;
    let mut deny = vec![
        "write_file(*)",
        "command(*)",
        "unsandboxed(*)",
        "execute_url(*)",
        "search_web(*)",
    ];
    if mode == Hardening::Companion {
        deny.push("read_file(*)");
    }
    fs::write(
        cli_directory.join("settings.json"),
        serde_json::to_vec(&serde_json::json!({
            "permissions": { "allow": [], "deny": deny }
        }))?,
    )?;
    spec.args
        .insert(0, format!("--gemini_dir={}", directory.to_string_lossy()));
    spec.args.push("--disable-slash-commands".into());
    Ok(())
}

fn validate_companion_launch_selection(
    spec: &CliProcessSpec,
    backend: CliBackend,
    model: Option<&str>,
    profile: Option<&str>,
) -> Result<(), TerminalError> {
    let values = |flag: &str| {
        spec.args
            .windows(2)
            .filter(|pair| pair[0] == flag)
            .map(|pair| pair[1].as_str())
            .collect::<Vec<_>>()
    };
    let models = values("--model");
    if models.as_slice() != model.map_or_else(Vec::new, |model| vec![model]).as_slice() {
        return Err(TerminalError::ModelMismatch {
            expected: model.unwrap_or("CLI default").into(),
            observed: "Companion launch arguments did not bind the selected model".into(),
        });
    }
    let profiles = match backend {
        CliBackend::Codex => values("-c")
            .into_iter()
            .filter_map(|value| {
                value
                    .strip_prefix("model_reasoning_effort=\"")?
                    .strip_suffix('"')
            })
            .collect::<Vec<_>>(),
        CliBackend::Claude | CliBackend::Antigravity => values("--effort"),
        CliBackend::Opencode => values("--variant"),
        CliBackend::Gemini | CliBackend::Custom => Vec::new(),
    };
    if profiles.as_slice()
        != profile
            .map_or_else(Vec::new, |profile| vec![profile])
            .as_slice()
    {
        return Err(TerminalError::Validation(
            "Companion launch arguments did not bind the selected profile".into(),
        ));
    }
    Ok(())
}

fn companion_model_evidence(
    host: &dyn TerminalHost,
    backend: CliBackend,
    requested: Option<&str>,
    stdout: &str,
) -> Result<(Option<String>, &'static str), TerminalError> {
    let mut observed = None;
    for line in stdout.lines() {
        let frame = adapters::normalize_frame(backend, "stdout", line.as_bytes());
        if !frame.is_model_generation {
            continue;
        }
        let Some(model) = frame.observed_model else {
            continue;
        };
        validate_public_cli_identity(host, &model)?;
        if let Some(requested) = requested {
            match model_identity::verify_model(
                Some(backend.as_str()),
                Some(requested),
                Some(&model),
            ) {
                model_identity::ModelVerification::Verified => {}
                model_identity::ModelVerification::Mismatch => {
                    return Err(TerminalError::ModelMismatch {
                        expected: requested.into(),
                        observed: model,
                    });
                }
                model_identity::ModelVerification::Unverified => continue,
            }
        }
        observed = Some(model);
    }
    let verification = if observed.is_some() && requested.is_some() {
        if backend == CliBackend::Claude && matches!(requested, Some("haiku" | "sonnet" | "opus")) {
            "family_verified"
        } else {
            "verified"
        }
    } else {
        "unverified"
    };
    Ok((observed, verification))
}

async fn run_companion_process(
    host: &dyn TerminalHost,
    spec: CliProcessSpec,
    backend: CliBackend,
    cancellation: &CancellationToken,
    deadline: Instant,
    detected_cli_version: Option<String>,
    usage_path: PathBuf,
    usage_context: crate::usage::UsageContext,
    requested_model: Option<String>,
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
    let usage = host.create_usage_recorder(&usage_path, usage_context, backend)?;
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
    // Decode/reassemble the complete control envelope before redaction. Scanning
    // raw JSONL cannot recognize a token split over escaped delta strings.
    let stdout = sanitize_cli_frame_text(&stdout);
    let sanitized_stderr = sanitize_cli_frame_text(&stderr);
    let stderr = host
        .redact_secrets(&sanitized_stderr)
        .map_err(TerminalError::Redaction)?;
    let claude_error = (backend == CliBackend::Claude)
        .then(|| claude_companion_error(&stdout))
        .flatten();
    if !status.success() || claude_error.is_some() {
        // Codex reports provider failures in JSONL stdout, often with empty stderr.
        // Decode only error events; never expose reasoning or the control stream.
        let diagnostic = match backend {
            CliBackend::Codex => {
                codex_companion_error(&stdout).unwrap_or_else(|| stderr.text.clone())
            }
            // Claude reports API failures in its stream-json `result` event
            // with an empty stderr; without this the developer saw only
            // "Claude CLI exited with status Some(1)".
            CliBackend::Claude => claude_error.unwrap_or_else(|| stderr.text.clone()),
            _ => stderr.text.clone(),
        };
        // Decode before the final redaction, including JSON-escaped credentials.
        let diagnostic = host
            .redact_secrets(&sanitize_cli_frame_text(diagnostic.as_bytes()))
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
    let envelope_json = extract_companion_envelope_json(&stdout).ok_or_else(|| {
        TerminalError::Process(format!(
            "Companion CLI returned an invalid control envelope ({})",
            companion_envelope_miss(backend, &stdout)
        ))
    })?;
    let mut envelope: Value = serde_json::from_str(&envelope_json).map_err(|_| {
        TerminalError::Process("Companion CLI returned an invalid control envelope".into())
    })?;
    let redaction_count = redact_json_strings_count(host, &mut envelope)?;
    let envelope_json = serde_json::to_string(&envelope)?;
    let (observed_model, model_verification) =
        companion_model_evidence(host, backend, requested_model.as_deref(), &stdout)?;
    Ok(CompanionCliRoundOutput {
        envelope_json,
        observed_model,
        model_verification,
        detected_cli_version,
        exit_code: status.code(),
        output_bytes: stdout.len().saturating_add(stderr.text.len()),
        redaction_count: redaction_count.saturating_add(stderr.count),
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

/// Why a stream carried no envelope, as a short stable code. The model called
/// host operations as native tools (`unavailable_tool`), the CLI rejected its
/// structured output (`schema_rejected`), it wrote only prose (`text_only`),
/// or nothing came back (`no_output`). Only Claude reports these events.
fn companion_envelope_miss(backend: CliBackend, stdout: &str) -> &'static str {
    if backend != CliBackend::Claude {
        return "no_envelope";
    }
    if stdout.trim().is_empty() {
        return "no_output";
    }
    let (mut unavailable, mut rejected, mut text) = (false, false, false);
    for event in stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
    {
        let blocks = event
            .pointer("/message/content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten();
        match event.get("type").and_then(Value::as_str) {
            Some("user") => {
                for block in blocks.filter(|block| block["type"] == "tool_result") {
                    let content = block["content"].to_string();
                    unavailable |= content.contains("No such tool available");
                    rejected |= content.contains("does not match required schema")
                        || content.contains("could not be parsed as JSON");
                }
            }
            Some("assistant") => {
                text |= blocks.into_iter().any(|block| {
                    block["type"] == "text"
                        && block["text"]
                            .as_str()
                            .is_some_and(|text| !text.trim().is_empty())
                });
            }
            _ => {}
        }
    }
    if rejected {
        "schema_rejected"
    } else if unavailable {
        "unavailable_tool"
    } else if text {
        "text_only"
    } else {
        "no_control_envelope"
    }
}

/// The `result` text of Claude's final stream-json event when it reports an
/// error (for example `API Error: 400 ...`).
fn claude_companion_error(stdout: &str) -> Option<String> {
    stdout.lines().rev().find_map(|line| {
        let value: Value = serde_json::from_str(line.trim()).ok()?;
        adapters::claude_error_message(&value)
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

pub fn extract_companion_envelope_json(output: &str) -> Option<String> {
    fn find(value: &Value, depth: usize) -> Option<Value> {
        if depth > 12 {
            return None;
        }
        if value
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|kind| matches!(kind, "final" | "final_with_canvas_offer" | "tool_calls"))
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

    // Claude may omit the whole assistant/result envelope after streaming a
    // complete StructuredOutput tool input. Reassemble only deltas belonging
    // to that named tool and then pass them through the same envelope parser.
    fn streamed_structured_output(output: &str) -> Option<Value> {
        let mut inputs = HashMap::<u64, String>::new();
        let mut last_complete = None;
        for frame in output
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
            .filter(|frame| frame["type"] == "stream_event")
        {
            let event = &frame["event"];
            let Some(index) = event["index"].as_u64() else {
                continue;
            };
            match event["type"].as_str() {
                Some("content_block_start") => {
                    if event["content_block"]["type"] == "tool_use"
                        && event["content_block"]["name"] == "StructuredOutput"
                    {
                        inputs.insert(index, String::new());
                    } else {
                        inputs.remove(&index);
                    }
                }
                Some("content_block_delta") if event["delta"]["type"] == "input_json_delta" => {
                    if let (Some(input), Some(fragment)) = (
                        inputs.get_mut(&index),
                        event["delta"]["partial_json"].as_str(),
                    ) {
                        input.push_str(fragment);
                    }
                }
                Some("content_block_stop") => {
                    if let Some(input) = inputs.remove(&index)
                        && let Ok(value) = serde_json::from_str::<Value>(&input)
                        && let Some(envelope) = find(&value, 0)
                    {
                        last_complete = Some(envelope);
                    }
                }
                _ => {}
            }
        }
        last_complete.or_else(|| {
            inputs
                .values()
                .filter_map(|input| serde_json::from_str::<Value>(input).ok())
                .filter_map(|value| find(&value, 0))
                .next()
        })
    }

    // JSONL transports carry the envelope in one of their last whole events,
    // after one frame per streamed delta. A long Claude answer emits hundreds of
    // `stream_event` lines whose braces used up the bounded scan below before it
    // reached the answer ("invalid control envelope", Companion, 2026-09-26).
    // Read whole events newest first, prefer Claude's `structured_output`, and
    // skip deltas, which only ever carry fragments.
    output
        .lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .filter(|event| event.get("type").and_then(Value::as_str) != Some("stream_event"))
        .find_map(|event| {
            event
                .get("structured_output")
                .and_then(|structured| find(structured, 0))
                .or_else(|| find(&event, 0))
        })
        .or_else(|| streamed_structured_output(output))
        .or_else(|| {
            let jsonl_transport = output.lines().any(|line| {
                serde_json::from_str::<Value>(line.trim())
                    .ok()
                    .and_then(|event| event.get("type").and_then(Value::as_str).map(str::to_owned))
                    .is_some()
            });
            (!jsonl_transport)
                .then(|| parse_candidate(output, 0))
                .flatten()
        })
        .and_then(|value| serde_json::to_string(&value).ok())
}

fn cli_mode_disabled() -> bool {
    crate::quota::env_flag("HVC_DISABLE_CLI_MODE")
}

/// One full probe of every CLI, with the local kill switch applied.
async fn scan_cli_discovery(runtime: Handle, host: Arc<dyn TerminalHost>) -> Vec<CliAvailability> {
    let mut items = match runtime.spawn(discover_clis_with_probes(host.clone())).await {
        Ok(items) => items,
        Err(_) => discover_clis(),
    };
    if cli_mode_disabled() {
        for availability in &mut items {
            if availability.backend != CliBackend::Custom {
                availability.state = CliAvailabilityState::Unsupported;
                availability.diagnostic =
                    Some("CLI Mode is disabled by the local kill switch.".into());
            }
        }
    }
    items
}

fn store_cli_discovery(cache: &Mutex<Option<CliDiscoveryCache>>, items: Vec<CliAvailability>) {
    *cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(CliDiscoveryCache {
        discovered_at: Instant::now(),
        items,
    });
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
        SANITIZATION_POLICY_VERSION,
        run.project_id.as_str(),
        run.slot_id.as_str(),
        run.backend.map(CliBackend::as_str).unwrap_or_default(),
        run.model.as_deref().unwrap_or_default(),
        workspace_snapshot_hash,
    ] {
        digest.update(part.as_bytes());
        digest.update([0]);
    }
    lower_hex(&digest.finalize())
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
            "the chosen session belongs to another project, slot, backend or model".into(),
        ));
    }
    if parent.status.is_active() {
        return Err(TerminalError::Validation(
            "a session whose run is still active cannot be resumed".into(),
        ));
    }
    if parent.workspace_snapshot_hash.as_deref() != Some(workspace_snapshot_hash) {
        return Err(TerminalError::Validation(
            "the workspace changed since the chosen session; start a new session".into(),
        ));
    }
    let native_session_id = parent.native_session_id.as_deref().ok_or_else(|| {
        TerminalError::Validation("the chosen run has no native session ID".into())
    })?;
    if requested_session_id != Some(native_session_id) {
        return Err(TerminalError::Validation(
            "the resume ID does not match the chosen session".into(),
        ));
    }
    let expected = session_fingerprint(run, workspace_snapshot_hash);
    if parent.session_fingerprint.as_deref() != Some(expected.as_str()) {
        return Err(TerminalError::Validation(
            "the session fingerprint is not compatible with this run".into(),
        ));
    }
    Ok(())
}

fn secure_launch_spec(
    mut spec: CliProcessSpec,
    backend: CliBackend,
) -> Result<SecuredLaunch, TerminalError> {
    let executable = spec.executable.canonicalize().map_err(|error| {
        TerminalError::Security(format!(
            "the CLI executable could not be canonicalized: {error}"
        ))
    })?;
    if !executable.is_file() {
        return Err(TerminalError::Security(
            "the CLI launch target is not a file".into(),
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
                "Custom cannot run shell wrappers".into(),
            ));
        }
        return resolve_trusted_npm_shim(spec, backend, &executable);
    }

    if matches!(extension.as_str(), "cmd" | "bat" | "ps1" | "lnk") {
        return Err(TerminalError::Security(
            "a shell wrapper or shortcut is not an allowed launch target".into(),
        ));
    }
    let identity_sha256 = hash_launch_files(&[&executable])?;
    spec.executable = executable;
    if backend == CliBackend::Antigravity {
        // `agy` otherwise spawns a background self-update on launch, which can swap the
        // binary between two calls of the same pulsation. Only `true` is honoured; `1` is
        // ignored (verified against 1.3.5's own log, 2026-10-14).
        spec.environment
            .push(("AGY_CLI_DISABLE_AUTO_UPDATE".into(), "true".into()));
    }
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
        .map_err(|error| TerminalError::Security(format!("invalid npm wrapper: {error}")))?;
    let normalized_shim = shim_text.replace('\\', "/").to_ascii_lowercase();
    if !normalized_shim.contains("%*") {
        return Err(TerminalError::Security(
            "the npm wrapper does not have the expected format".into(),
        ));
    }
    let shim_directory = shim
        .parent()
        .ok_or_else(|| TerminalError::Security("the npm wrapper has no parent directory".into()))?;
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
        // `agy` ships as a native executable, never through npm.
        CliBackend::Antigravity | CliBackend::Custom => &[],
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
                "the {} npm wrapper does not point to the expected package",
                backend.label()
            ))
        })?;
    if marker.is_empty() {
        return Err(TerminalError::Security("invalid npm entrypoint".into()));
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
        .ok_or_else(|| TerminalError::Security("node.exe not found for the npm wrapper".into()))?
        .canonicalize()
        .map_err(TerminalError::Io)
}

/// Metadata a launch identity is cached under. Any change in path, length,
/// modification or creation time recomputes the digest.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct LaunchFileStamp {
    path: PathBuf,
    len: u64,
    modified: std::time::SystemTime,
    created: Option<std::time::SystemTime>,
}

const MAX_CACHED_LAUNCH_IDENTITIES: usize = 64;

static LAUNCH_IDENTITY_CACHE: std::sync::OnceLock<Mutex<HashMap<Vec<LaunchFileStamp>, String>>> =
    std::sync::OnceLock::new();

fn launch_file_stamps(paths: &[&Path]) -> Option<Vec<LaunchFileStamp>> {
    paths
        .iter()
        .map(|path| {
            let metadata = fs::metadata(path).ok()?;
            Some(LaunchFileStamp {
                path: path.to_path_buf(),
                len: metadata.len(),
                modified: metadata.modified().ok()?,
                created: metadata.created().ok(),
            })
        })
        .collect()
}

/// Hashing a 170-290 MB CLI binary (plus node.exe) on every launch and every
/// discovery probe cost 3-8 s per run in a debug build (`ToDoListWebView`,
/// 2026-09-22). The digest is reused while the files' metadata is unchanged;
/// a file rewritten while it was being hashed is never cached.
fn cached_launch_identity(
    paths: &[&Path],
    compute: impl FnOnce() -> Result<String, TerminalError>,
) -> Result<String, TerminalError> {
    let Some(key) = launch_file_stamps(paths) else {
        return compute();
    };
    let cache = LAUNCH_IDENTITY_CACHE.get_or_init(Default::default);
    if let Some(hit) = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&key)
    {
        return Ok(hit.clone());
    }
    let digest = compute()?;
    if launch_file_stamps(paths).as_ref() == Some(&key) {
        let mut entries = cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if entries.len() >= MAX_CACHED_LAUNCH_IDENTITIES {
            entries.clear();
        }
        entries.insert(key, digest.clone());
    }
    Ok(digest)
}

pub fn hash_launch_files(paths: &[&Path]) -> Result<String, TerminalError> {
    cached_launch_identity(paths, || hash_launch_files_uncached(paths))
}

fn hash_launch_files_uncached(paths: &[&Path]) -> Result<String, TerminalError> {
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
    Ok(lower_hex(&digest.finalize()))
}

pub fn configure_hidden(command: &mut Command) {
    #[cfg(windows)]
    {
        command.creation_flags(0x0800_0000);
    }
}

fn clip_text(host: &dyn TerminalHost, content: &str, max_chars: usize) -> String {
    let content = host
        .sanitize_model_text_counted(content)
        .map(|projection| projection.text)
        .unwrap_or_else(|_| "[content unavailable after sanitization]".into());
    match content.char_indices().nth(max_chars) {
        None => content,
        Some((offset, _)) => format!("{}\n...[truncated]", &content[..offset]),
    }
}

pub(crate) fn materialize_safe_workspace<H: TerminalHost + ?Sized>(
    host: &H,
    db_path: &Path,
    workspace: &Path,
    run: &TerminalRun,
) -> Result<SafeCliWorkspace, TerminalError> {
    let started = Instant::now();
    let snapshot = host.prepare_snapshot(
        db_path,
        workspace,
        run,
        MAX_SAFE_SNAPSHOT_FILES,
        MAX_SAFE_SNAPSHOT_BYTES,
    )?;
    let directory = tempfile::Builder::new()
        .prefix("hvc-cli-workspace-")
        .tempdir()?;
    let mut digest = Sha256::new();
    digest.update(SANITIZATION_POLICY_VERSION.as_bytes());
    digest.update([0]);
    digest.update(snapshot.policy_identity.as_bytes());
    digest.update([0]);
    let mut manifest_files = Vec::with_capacity(snapshot.files.len());
    for file in &snapshot.files {
        let relative = &file.relative_path;
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(TerminalError::Security(
                "snapshot tentou materializar path fora da raiz".into(),
            ));
        }
        let relative_text = relative.to_string_lossy();
        digest.update(relative_text.as_bytes());
        digest.update([0]);
        digest.update(file.original_sha256);
        digest.update([0]);
        digest.update(file.projected_content.as_bytes());
        digest.update([0]);
        let destination = directory.path().join(relative);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(destination, file.projected_content.as_bytes())?;
        manifest_files.push(relative_text.into_owned());
    }
    let manifest_hash = lower_hex(&digest.finalize());
    fs::write(
        directory.path().join(".hvc-cli-snapshot.json"),
        serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 1, "sanitizationVersion": SANITIZATION_POLICY_VERSION,
            "manifestSha256": manifest_hash, "files": manifest_files,
        }))?,
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
    let safe_workspace = SafeCliWorkspace::from_directory(directory, manifest_hash);
    host.record_snapshot(
        db_path,
        run,
        &snapshot,
        started.elapsed().as_millis().min(u64::MAX as u128) as u64,
    )?;
    Ok(safe_workspace)
}

#[cfg(test)]
mod recovery_tests;
#[cfg(test)]
mod snapshot_tests;
#[cfg(test)]
mod tests;
