//! `ExecutionMandateV1` and `ApprovalReceiptV1` (ADR-035 M1): what the
//! developer authorized by approving the Specification, and what each
//! checkpoint decision covered. Both are host records built from typed
//! payloads and durable rows. Their bytes are canonical JSON (sorted keys,
//! integers only), so the same inputs always give the same digest.

use serde::{Deserialize, Serialize};

use crate::orchestration::ContentSha256;
use crate::orchestration::composition::policy::{EditOp, ToolPolicy};
use crate::orchestration::composition::{CheckKind, ExecutorKind, SourceRole, canonical_json};

pub const MANDATE_SCHEMA_VERSION: u32 = 1;
pub const RECEIPT_SCHEMA_VERSION: u32 = 1;
/// Version of the rules that turn the frozen batch and the approved
/// Specification into write grants (`derive.rs`).
pub const WRITE_CLASS_POLICY_VERSION: u32 = 1;
/// Version of the built-in protected-name rules of `publication.rs`.
pub const PROTECTED_RULES_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MandateCause {
    /// First approval of the Specification.
    Issued,
    /// A later approval of the Specification, after a revision.
    Reissued,
    /// A scope request granted at a failure checkpoint (ADR-035 M4).
    ScopeGrant,
    /// An approved CLI fallback changed a role's transport.
    TransportFallback,
}

impl MandateCause {
    pub const fn as_db_value(self) -> &'static str {
        match self {
            Self::Issued => "issued",
            Self::Reissued => "reissued",
            Self::ScopeGrant => "scope_grant",
            Self::TransportFallback => "transport_fallback",
        }
    }

    pub fn from_db_value(value: &str) -> Option<Self> {
        match value {
            "issued" => Some(Self::Issued),
            "reissued" => Some(Self::Reissued),
            "scope_grant" => Some(Self::ScopeGrant),
            "transport_fallback" => Some(Self::TransportFallback),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Enforcement {
    /// Pipeline v4: writes, dispatches and budgets are checked against it.
    EnforcedV4,
    /// Pipelines v1 to v3 record the approval but have no typed destinations
    /// to check (ADR-035 D1).
    NotAppliedLegacy,
}

impl Enforcement {
    pub const fn as_db_value(self) -> &'static str {
        match self {
            Self::EnforcedV4 => "enforced_v4",
            Self::NotAppliedLegacy => "not_applied_legacy",
        }
    }

    pub const fn is_enforced(self) -> bool {
        matches!(self, Self::EnforcedV4)
    }
}

/// The checkpoint decision that produced a revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssuedBy {
    pub checkpoint_id: String,
    pub stage: String,
    pub state_version: u64,
    pub input_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskRevisionRef {
    pub task_id: String,
    pub revision: u64,
    /// Canonical digest of the frozen task draft.
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MandateIntent {
    /// Sorted by task id.
    pub task_revisions: Vec<TaskRevisionRef>,
    /// Digest of the sorted content digests of the artifacts the approval
    /// accepted.
    pub specification_sha256: String,
    pub review_policy_snapshot_sha256: Option<String>,
}

/// Why a path is in the write envelope. A path keeps every origin that names
/// it, in this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteClass {
    /// A source of an approved feature brief in the frozen batch.
    BriefSource,
    /// A path a frozen task names in its title or description.
    TaskPath,
    /// A path a clarification answer planned or named as modified.
    ClarificationPath,
    /// A file the approved Specification cites, plans or proposes to write.
    SpecificationReference,
    /// A note the host renders for a stage (`vault_note_path`).
    StageDocument,
    /// The test file the host derives when nothing planned one.
    TestDestination,
    /// A path granted to one unit at a failure checkpoint (ADR-035 M4).
    Amendment,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteGrant {
    /// Workspace-relative, `/`-separated, as the host validated it.
    pub path: String,
    pub origins: Vec<WriteClass>,
    /// The unit an amendment is for; `None` for every other origin.
    pub unit_key: Option<String>,
    /// The revision that added an amendment; `None` for every other origin.
    pub amendment_revision: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProtectedRules {
    pub builtin_version: u32,
    /// Digest of the project's sensitive-path additions (ADR-034), when any.
    pub project_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MandateScope {
    /// Sorted by the path's Windows identity (ASCII lowercase).
    pub grants: Vec<WriteGrant>,
    pub write_class_policy_version: u32,
    pub protected_rules: ProtectedRules,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutorCapability {
    pub kind: ExecutorKind,
    pub edit_operations: Vec<EditOp>,
    pub writable_roles: Vec<SourceRole>,
    pub tool_policy: ToolPolicy,
    pub context_scope: ikore_contracts::UnitContextScope,
}

/// A canonical role's transport as resolved when the mandate was issued.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransportCapability {
    pub role: String,
    /// `api` or `cli`.
    pub transport: String,
    pub backend: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckCapability {
    pub check_ref: String,
    pub kind: CheckKind,
    pub profile_sha256: String,
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MandateCapabilities {
    /// One per executor kind, sorted by kind.
    pub executors: Vec<ExecutorCapability>,
    /// One per role of the stages after the Specification, sorted by role.
    pub transports: Vec<TransportCapability>,
    pub recipe_policy_sha256: String,
    /// Enabled validation profiles, sorted by check handle.
    pub checks: Vec<CheckCapability>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StageOutputLimit {
    pub stage: String,
    pub output_tokens: u32,
}

/// The parameters `derive_budget` receives, frozen for the rest of the
/// pulsation. Per-stage invocation counts come from each stage's units and
/// do not exist yet when the Specification is approved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetParameters {
    pub stage_output_limits: Vec<StageOutputLimit>,
    pub context_chars_limit: u64,
    pub extra_invocation_limit: u32,
    pub per_unit_extra_invocation_limit: u32,
    pub max_in_flight_units: u32,
}

impl BudgetParameters {
    pub fn output_limit_for(&self, stage: &str) -> Option<u32> {
        self.stage_output_limits
            .iter()
            .find(|limit| limit.stage.eq_ignore_ascii_case(stage))
            .map(|limit| limit.output_tokens)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionMandateV1 {
    pub schema_version: u32,
    pub pulsation_id: String,
    pub project_id: String,
    pub revision: u32,
    pub previous_sha256: Option<String>,
    pub cause: MandateCause,
    pub issued_by: IssuedBy,
    pub enforcement: Enforcement,
    pub intent: MandateIntent,
    pub scope: MandateScope,
    pub capabilities: MandateCapabilities,
    pub budget: BudgetParameters,
    /// Reserved; always `None` in schema 1 (ADR-035 D9).
    pub expires_at: Option<String>,
}

impl ExecutionMandateV1 {
    /// Canonical JSON: sorted keys, no whitespace, integers only.
    pub fn canonical_bytes(&self) -> Result<String, String> {
        canonical_of(self)
    }

    pub fn sha256(&self) -> Result<ContentSha256, String> {
        Ok(ContentSha256::of(self.canonical_bytes()?.as_bytes()))
    }

    /// Whether `path` is in the write envelope, by Windows identity.
    pub fn grants_path(&self, path: &str) -> bool {
        let key = path_key(path);
        self.scope
            .grants
            .iter()
            .any(|grant| path_key(&grant.path) == key)
    }

    pub fn transport_for(&self, role: &str) -> Option<&TransportCapability> {
        self.capabilities
            .transports
            .iter()
            .find(|transport| transport.role == role)
    }

    pub fn executor(&self, kind: ExecutorKind) -> Option<&ExecutorCapability> {
        self.capabilities
            .executors
            .iter()
            .find(|executor| executor.kind == kind)
    }
}

/// One acknowledgement the decision carried, by digest only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptAcknowledgement {
    pub kind: AcknowledgementKind,
    pub reason_sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcknowledgementKind {
    Risk,
    Convergence,
    PreflightDismissal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewedArtifact {
    pub artifact_id: String,
    pub sha256: String,
}

/// What one checkpoint decision covered (ADR-035 M1). Free text appears only
/// as a digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalReceiptV1 {
    pub schema_version: u32,
    pub checkpoint_id: String,
    pub pulsation_id: String,
    pub purpose: String,
    pub stage: String,
    pub decision: String,
    pub state_version: u64,
    pub input_revision: u64,
    pub decided_at: String,
    /// Sorted by artifact id.
    pub reviewed_artifacts: Vec<ReviewedArtifact>,
    /// Paths a path-confirmation decision confirmed, sorted.
    pub confirmed_paths: Vec<String>,
    /// The mandate revision in force when the decision was taken.
    pub mandate_revision: Option<u32>,
    pub acknowledgements: Vec<ReceiptAcknowledgement>,
    pub feedback_sha256: Option<String>,
}

impl ApprovalReceiptV1 {
    pub fn canonical_bytes(&self) -> Result<String, String> {
        canonical_of(self)
    }
}

/// The Windows identity of a relative path: `/` separators, ASCII lowercase.
pub fn path_key(path: &str) -> String {
    path.replace('\\', "/").to_ascii_lowercase()
}

/// sha256 of free text, for the fields that keep only a digest.
pub fn text_sha256(text: &str) -> String {
    ContentSha256::of(text.as_bytes()).as_str().to_owned()
}

fn canonical_of<T: Serialize>(value: &T) -> Result<String, String> {
    let json = serde_json::to_value(value).map_err(|error| error.to_string())?;
    canonical_json(&json).map_err(|error| error.message)
}
