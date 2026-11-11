//! The in-toto Statement v1 with the `PulsationAttestationV1` predicate.
//!
//! Every field is a host fact: an identifier, a digest, an enumerated state,
//! a relative path or an integer. No file content, prompt, stage document,
//! reason text, feature or task name, absolute path, account or credential
//! enters (ADR-036 D6). Lists are sorted by the deriver, and the canonical
//! form (sorted keys, no floating point) is what gets signed.

use serde::{Deserialize, Serialize};

use crate::orchestration::ContentSha256;
use crate::orchestration::composition::canonical_json;
use crate::orchestration::mandate::TaskRevisionRef;

pub const STATEMENT_TYPE: &str = "https://in-toto.io/Statement/v1";
pub const PREDICATE_TYPE: &str = "urn:hvc:pulsation-attestation:v1";
/// The version of the deriver that builds the predicate from rows. A newer
/// deriver gets a new number; old ones stay so old statements re-derive.
pub const DERIVER_VERSION: u32 = 1;
/// A statement above this is recorded as `too_large`, never truncated
/// (hypothesis, ADR-036).
pub const MAX_STATEMENT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatementV1 {
    #[serde(rename = "_type")]
    pub statement_type: String,
    pub subject: Vec<SubjectV1>,
    #[serde(rename = "predicateType")]
    pub predicate_type: String,
    pub predicate: PulsationAttestationV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubjectV1 {
    pub name: String,
    pub digest: SubjectDigest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubjectDigest {
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PulsationAttestationV1 {
    pub pulsation: PulsationBlock,
    pub builder: BuilderBlock,
    pub intent: IntentBlock,
    pub authority: AuthorityBlock,
    pub execution: ExecutionBlock,
    pub evidence: EvidenceBlock,
    pub sanitization: SanitizationBlock,
    pub effects: Vec<EffectV1>,
    pub conformance: ConformanceBlock,
    pub usage: UsageBlock,
    pub sealing: SealingBlock,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PulsationBlock {
    pub id: String,
    pub project_id: String,
    pub pipeline_version: u32,
    /// `done`, `failed`, `rejected` or `aborted`.
    pub outcome: String,
    pub terminal_reason: Option<String>,
    pub created_at: String,
    /// When the pulsation reached its terminal state.
    pub ended_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuilderBlock {
    pub hvc_version: String,
    pub build_id: Option<String>,
    pub os: String,
    pub arch: String,
    /// The key active when the statement was sealed; `None` when none was
    /// available. The envelope names the key that actually signed.
    pub key_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreflightFindingV1 {
    pub feature_revision: u64,
    pub decisions_sha256: String,
    /// `none`, `pending`, `adjusted`, `adr_proposed` or `dismissed`.
    pub choice: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntentBlock {
    pub task_revisions: Vec<TaskRevisionRef>,
    /// Digest of the accepted Specification artifacts of the final revision.
    pub specification_sha256: Option<String>,
    pub contract_sha256: Option<String>,
    pub review_policy_snapshot_sha256: Option<String>,
    pub preflight_findings: Vec<PreflightFindingV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MandateRefV1 {
    pub revision: u32,
    pub cause: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRefV1 {
    pub checkpoint_id: String,
    pub purpose: String,
    pub stage: String,
    pub decision: String,
    pub receipt_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorityBlock {
    pub mandates: Vec<MandateRefV1>,
    /// In decision order.
    pub approvals: Vec<ApprovalRefV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StageBudgetRefV1 {
    pub initial_invocations: u32,
    pub extra_invocations: u32,
    pub output_tokens: u64,
    pub prompt_chars: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttemptRefV1 {
    pub executor_kind: String,
    pub transport: String,
    pub backend: Option<String>,
    pub effective_model: Option<String>,
    pub observed_model: Option<String>,
    pub dispatch_sha256: Option<String>,
    pub input_sha256: Option<String>,
    pub result_sha256: Option<String>,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StageRunV1 {
    pub stage: String,
    pub generation: u64,
    pub plan_sha256: String,
    pub recipe_version: u32,
    pub policy_snapshot_sha256: Option<String>,
    pub status: String,
    pub budget: Option<StageBudgetRefV1>,
    pub attempts: Vec<AttemptRefV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionBlock {
    /// Composed stage executions (pipeline v4), in creation order.
    pub stages: Vec<StageRunV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptRefV1 {
    pub check_ref: String,
    pub status: String,
    pub exit_code: Option<i64>,
    pub candidate_sha256: String,
    pub profile_sha256: String,
    pub toolchain_sha256: String,
    pub environment_sha256: String,
    pub receipt_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntegratorManifestRefV1 {
    pub sha256: String,
    pub gap_count: u32,
    pub review_policy_state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageRefV1 {
    pub level: String,
    pub executed: u32,
    pub model_only: u32,
    pub unverified: u32,
    pub not_run: u32,
    pub matrix_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RiskRefV1 {
    pub stage: String,
    pub input_revision: u64,
    pub level: String,
    pub policy_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcknowledgementRefV1 {
    /// `risk` or `convergence`.
    pub kind: String,
    pub reason_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceBlock {
    pub receipts: Vec<ReceiptRefV1>,
    pub integrator_manifest: Option<IntegratorManifestRefV1>,
    pub coverage: Option<CoverageRefV1>,
    pub risk: Vec<RiskRefV1>,
    pub acknowledgements: Vec<AcknowledgementRefV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryCountV1 {
    pub category: String,
    pub count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SanitizationBlock {
    /// The policies the pulsation's calls were sanitized under.
    pub policy_identities: Vec<String>,
    pub redactions: u64,
    /// How many calls redacted each category; never a value.
    pub categories: Vec<CategoryCountV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectV1 {
    pub path: String,
    /// `absent`, `present` or `present_unstored`; `None` when no base was
    /// captured (pipelines before v4).
    pub base_state: Option<String>,
    pub base_sha256: Option<String>,
    pub applied_sha256: String,
    pub artifact_id: String,
    pub producer_stage: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConformanceBlock {
    /// `None` when the pulsation ran without a mandate (pipelines v1–v3).
    pub within_mandate: Option<bool>,
    pub enforcement_events: u32,
    pub scope_grants: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsageV1 {
    pub provider: String,
    pub observed_model: Option<String>,
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub reasoning: Option<u64>,
    pub cache_read: Option<u64>,
    pub cache_write: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageBlock {
    pub models: Vec<ModelUsageV1>,
    /// `complete`, `partial` or `unavailable`.
    pub completeness: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SealingBlock {
    pub sealed_at: String,
    /// Always `local`: the time comes from this machine's clock.
    pub clock: String,
    pub deriver_version: u32,
}

/// The predicate blocks in statement order, for naming the first one that
/// differs.
const BLOCKS: [&str; 11] = [
    "pulsation",
    "builder",
    "intent",
    "authority",
    "execution",
    "evidence",
    "sanitization",
    "effects",
    "conformance",
    "usage",
    "sealing",
];

impl StatementV1 {
    pub fn new(subject: Vec<SubjectV1>, predicate: PulsationAttestationV1) -> Self {
        Self {
            statement_type: STATEMENT_TYPE.to_owned(),
            subject,
            predicate_type: PREDICATE_TYPE.to_owned(),
            predicate,
        }
    }

    /// The bytes that are signed: canonical JSON, keys sorted, no floats.
    pub fn canonical_bytes(&self) -> Result<String, String> {
        let value = serde_json::to_value(self).map_err(|error| error.to_string())?;
        canonical_json(&value).map_err(|error| error.message)
    }

    pub fn sha256(&self) -> Result<String, String> {
        Ok(ContentSha256::of(self.canonical_bytes()?.as_bytes())
            .as_str()
            .to_owned())
    }

    /// The first part that differs from `other`: `subject` or a predicate
    /// block name.
    pub fn first_difference(&self, other: &Self) -> Option<&'static str> {
        if self.statement_type != other.statement_type
            || self.predicate_type != other.predicate_type
        {
            return Some("statement");
        }
        if self.subject != other.subject {
            return Some("subject");
        }
        let left = serde_json::to_value(&self.predicate).ok()?;
        let right = serde_json::to_value(&other.predicate).ok()?;
        BLOCKS
            .into_iter()
            .find(|block| left.get(block) != right.get(block))
    }
}
