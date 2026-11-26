//! Feature 25 (M4): stage verifiers attached to default pipeline stages,
//! behind Settings › Labs (`stage_verifiers_enabled`).
//!
//! Verifiers are read-only agents (E1, E2) that inspect a stage's accepted
//! result on the automation lane before the default pipeline advances or opens
//! a review checkpoint.

pub(crate) mod prompt;

use serde::{Deserialize, Serialize};

pub use ikore_contracts::stage_verifier::*;

use crate::persistence_service::{Pulsation, PulsationStage};

/// A verifier configured for a project's default stage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StageVerifierView {
    pub id: String,
    pub project_id: String,
    pub stage: String,
    pub label: String,
    pub position: i64,
    pub enabled: bool,
    pub reject_limit: u8,
    pub agent: StepAgent,
    pub instructions: String,
    pub version: i64,
    pub created_at: String,
    pub updated_at: String,
    pub run_count: u64,
}

impl StageVerifierView {
    /// Advisory verifiers (`reject_limit == 0`) report findings without
    /// re-running the stage or opening a failure checkpoint (P19).
    pub const fn is_advisory(&self) -> bool {
        self.reject_limit == 0
    }
}

/// Persisted execution record for one verifier invocation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StageVerifierRunView {
    pub id: String,
    pub verifier_id: String,
    pub verifier_label: String,
    pub stage: String,
    pub pulsation_id: String,
    pub stage_job_id: String,
    pub stage_generation: u64,
    pub attempt: u64,
    pub recovery_count: u64,
    pub status: String,
    pub reject_limit: u8,
    pub advisory: bool,
    pub terminal_run_id: Option<String>,
    pub llm_execution_id: Option<String>,
    pub verdict: Option<StageVerdictV1>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub error: Option<String>,
}

/// Snapshot of a project's stage verifiers and recent runs for the UI.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StageVerifiersSnapshot {
    pub project_id: String,
    pub enabled_by_labs: bool,
    pub verifiers: Vec<StageVerifierView>,
    pub runs: Vec<StageVerifierRunView>,
}

/// Outcome of creating or updating a stage verifier.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "outcome")]
pub enum SaveStageVerifierOutcome {
    Saved {
        verifier: Box<StageVerifierView>,
    },
    Invalid {
        errors: Vec<PipelineValidationError>,
    },
}

/// Human resolution for a `verification_error` checkpoint (P17, P25).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationErrorDecision {
    RetryVerifier,
    SkipVerifier,
    Abort,
}

impl VerificationErrorDecision {
    pub const fn as_db_value(self) -> &'static str {
        match self {
            Self::RetryVerifier => "retry_verifier",
            Self::SkipVerifier => "skip_verifier",
            Self::Abort => "abort",
        }
    }
}

/// An open technical verification-error checkpoint waiting for the developer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationErrorCheckpoint {
    pub checkpoint_id: String,
    pub pulsation_id: String,
    pub project_id: String,
    pub feature_name: String,
    pub verifier_run_id: String,
    pub verifier_id: String,
    pub verifier_label: String,
    pub stage: PulsationStage,
    pub error_message: String,
    pub attempt: u64,
    pub state_version: u64,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveVerificationErrorRequest {
    pub checkpoint_id: String,
    pub expected_state_version: u64,
    pub decision: VerificationErrorDecision,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveVerificationErrorResponse {
    pub pulsation: Pulsation,
}

/// Findings from a rejected verifier generation carried into a default stage
/// re-run prompt (`## Verification findings`, P24).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StageVerificationFinding {
    pub verifier_label: String,
    pub summary: String,
    pub findings: Vec<VerdictFinding>,
}

/// Terminal slot identifier for a stage verifier: `verifier:<project_id>:<verifier_id>`.
pub fn verifier_slot_id(project_id: &str, verifier_id: &str) -> String {
    format!("verifier:{project_id}:{verifier_id}")
}

/// Cancellation key for an in-flight verifier run.
pub(crate) fn verifier_cancellation_key(run_id: &str) -> String {
    format!("verifier-run:{run_id}")
}

/// Dock tab and terminal slot label: `{Stage} · {verifier}`.
pub fn verifier_tab_label(stage: &str, label: &str) -> String {
    format!(
        "{} \u{b7} {}",
        crate::custom_pipelines::trigger_stage_label(stage),
        label.trim()
    )
}
