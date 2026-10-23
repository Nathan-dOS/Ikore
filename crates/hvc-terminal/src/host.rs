//! Host capabilities. Authorization proofs remain opaque and are validated by their issuer.

use crate::terminals::{CliBackend, SafeCliWorkspace, TerminalError, TerminalRun};
use hvc_contracts::{
    AgentExecutionTarget, CliTimeoutBudget, ResponseFormat, TokenCounts, UnitDispatch, UsageContext,
};
use std::{
    any::Any,
    fmt::Debug,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

pub use hvc_contracts::redaction::{
    ContextRedaction, MAX_REDACTION_SPANS, RedactionResult, RedactionSpan,
    SANITIZATION_POLICY_VERSION,
};

/// An opaque host-issued proof. Candidates must be checked by `TerminalHost::prepare_input`.
pub trait InputProof: Any + Debug + Send + Sync {
    fn as_any(&self) -> &dyn Any;
    fn system_prompt(&self) -> &str;
    fn user_prompt(&self) -> &str;
    fn policy_identity(&self) -> &str;
    fn redaction_count(&self) -> u64;
    fn categories(&self) -> &[String];
    fn original_bytes(&self) -> usize;
}

pub type PreparedModelInput = Arc<dyn InputProof>;

pub trait TerminalUsageRecorder: Send + Sync {
    fn observe_line(&self, line: &[u8]) -> rusqlite::Result<()>;
    fn totals(&self) -> TokenCounts;
    fn output_tokens_so_far(&self) -> u64;
    fn record_live_partial(&self) -> rusqlite::Result<()>;
    fn mark_partial(&self) -> rusqlite::Result<()>;
}

/// Application services required by the CLI subsystem; no callback runs with raw output in logs.
pub trait TerminalHost: Debug + Send + Sync {
    fn redact_context(&self, text: &str) -> Result<ContextRedaction, String>;
    fn redact_secrets(&self, text: &str) -> Result<RedactionResult, String>;
    fn sanitize_model_text_counted(&self, text: &str) -> Result<RedactionResult, String>;
    fn prepare_input(
        &self,
        system: &str,
        user: &str,
        candidate: Option<PreparedModelInput>,
    ) -> Result<PreparedModelInput, TerminalError>;
    fn record_prepared_input(
        &self,
        db_path: &Path,
        project: &str,
        pulse: Option<&str>,
        call_id: &str,
        proof: &PreparedModelInput,
    ) -> Result<(), TerminalError>;
    fn prepare_workspace(
        &self,
        db_path: &Path,
        workspace: &Path,
        run: &TerminalRun,
    ) -> Result<SafeCliWorkspace, TerminalError> {
        crate::terminals::materialize_safe_workspace(self, db_path, workspace, run)
    }
    fn prepare_snapshot(
        &self,
        _db_path: &Path,
        _workspace: &Path,
        _run: &TerminalRun,
        _max_files: usize,
        _max_bytes: u64,
    ) -> Result<SnapshotPreparation, TerminalError> {
        Err(TerminalError::Security(
            "snapshot source capability is unavailable".into(),
        ))
    }
    fn record_snapshot(
        &self,
        db_path: &Path,
        run: &TerminalRun,
        snapshot: &SnapshotPreparation,
        duration_ms: u64,
    ) -> Result<(), TerminalError>;
    fn create_usage_recorder(
        &self,
        db_path: &Path,
        context: UsageContext,
        backend: CliBackend,
    ) -> rusqlite::Result<Arc<dyn TerminalUsageRecorder>>;
}

#[derive(Debug, Clone)]
pub struct ExecutionId(pub String);

#[derive(Debug, Clone, Default)]
pub struct TerminalDispatchPolicy {
    pub prepared_input: Option<PreparedModelInput>,
    pub cli_timeout: Option<CliTimeoutBudget>,
    pub unit: Option<UnitDispatch>,
    pub strict_schema: Option<serde_json::Value>,
}

/// Only transport inputs; routing, provider selection and request ownership remain with the host.
#[derive(Debug, Clone)]
pub struct LlmRequest {
    pub execution_id: ExecutionId,
    pub project_id: String,
    pub pulsation_id: Option<String>,
    pub job_id: Option<String>,
    pub agent_run_id: Option<String>,
    pub job_chain: String,
    pub target: AgentExecutionTarget,
    pub stage: String,
    pub system_prompt: String,
    pub user_prompt: String,
    pub response_format: ResponseFormat,
    pub max_output_tokens: u32,
    pub deadline: Instant,
    pub dispatch: TerminalDispatchPolicy,
}

/// A safe source selected and projected by the host, ready for runtime materialization.
#[derive(Clone)]
pub struct SnapshotFile {
    pub relative_path: PathBuf,
    pub original_sha256: [u8; 32],
    pub projected_content: String,
}

#[derive(Clone)]
pub struct SnapshotPreparation {
    pub policy_identity: String,
    pub files: Vec<SnapshotFile>,
    pub redaction_count: u64,
    pub categories: Vec<String>,
    pub applied_exceptions: Vec<String>,
    pub original_bytes: usize,
}
