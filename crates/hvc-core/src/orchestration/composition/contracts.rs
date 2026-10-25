//! Typed `stage-composition-v1` contracts.
//!
//! Every type here mirrors one `$defs` entry of `fixtures/stage-composition-v1.schema.json`
//! field by field. Serde enforces the closed shape (unknown fields, tagged unions,
//! integer-only numbers); [`Validate`] enforces the remaining schema constraints
//! (lengths, ranges, uniqueness) so a value built in Rust is held to the same
//! rules as one parsed from a model. The conformance tests at the bottom prove
//! parity with the documented registry for every published example.
//!
//! Naming: Rust fields are `snake_case` and serialize as camelCase; enum values
//! serialize as `snake_case` exactly as the schema enumerates them.

use std::collections::HashSet;
use std::fmt;
use std::hash::Hash;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{CompositionError, ErrorCode, Invariant};
use crate::orchestration::{ContentSha256, SchemaV1};
use crate::persistence_service::PulsationStage;

pub const CONTRACT_VERSION: u8 = 1;
pub const RECIPE_VERSION: u8 = 1;

/// Default admission/output limits from the contract package.
pub const MAX_UNITS_PER_PLAN: usize = 16;
pub const MAX_AGGREGATE_ARTIFACTS: usize = 20;
pub const MAX_EDITS_PER_PAYLOAD: usize = 64;
pub const MAX_ARTIFACT_BYTES: usize = 1_048_576;
pub const MAX_REPLACE_FILE_BYTES: usize = 8_192;
pub const MAX_TEXT_CHARS: usize = 4_000;
pub const MAX_SIGNATURE_CHARS: usize = 16_000;
pub const MAX_QUESTIONS: usize = 8;
pub const MAX_LOG_BYTES: usize = 1_048_576;
pub const MAX_INT32: u32 = 2_147_483_647;

// ---------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------

/// Schema constraints that serde cannot express.
pub trait Validate {
    fn validate(&self) -> Result<(), CompositionError>;
}

pub(crate) fn scalar_len(text: &str) -> usize {
    text.chars().count()
}

fn shape(message: String) -> CompositionError {
    CompositionError::new(Invariant::Sc001, ErrorCode::InvalidShape, message)
}

pub(crate) fn check_text(
    field: &str,
    text: &str,
    min: usize,
    max: usize,
) -> Result<(), CompositionError> {
    let len = scalar_len(text);
    if len < min || len > max {
        return Err(shape(format!(
            "`{field}` must have between {min} and {max} characters; got {len}."
        )));
    }
    Ok(())
}

pub(crate) fn check_items<T>(
    field: &str,
    items: &[T],
    min: usize,
    max: usize,
) -> Result<(), CompositionError> {
    let len = items.len();
    if len < min || len > max {
        return Err(shape(format!(
            "`{field}` must have between {min} and {max} items; got {len}."
        )));
    }
    Ok(())
}

pub(crate) fn check_unique<T: Eq + Hash>(field: &str, items: &[T]) -> Result<(), CompositionError> {
    let mut seen = HashSet::with_capacity(items.len());
    if items.iter().any(|item| !seen.insert(item)) {
        return Err(shape(format!("`{field}` must not contain duplicates.")));
    }
    Ok(())
}

pub(crate) fn check_unique_items<T: Eq + Hash>(
    field: &str,
    items: &[T],
    min: usize,
    max: usize,
) -> Result<(), CompositionError> {
    check_items(field, items, min, max)?;
    check_unique(field, items)
}

pub(crate) fn check_max_int(field: &str, value: u32, max: u32) -> Result<(), CompositionError> {
    if value > max {
        return Err(shape(format!(
            "`{field}` must be at most {max}; got {value}."
        )));
    }
    Ok(())
}

pub(crate) fn check_min_int(field: &str, value: u32, min: u32) -> Result<(), CompositionError> {
    if value < min {
        return Err(shape(format!(
            "`{field}` must be at least {min}; got {value}."
        )));
    }
    Ok(())
}

fn validate_all<T: Validate>(items: &[T]) -> Result<(), CompositionError> {
    items.iter().try_for_each(Validate::validate)
}

/// Explicitly nullable contract fields must be present with `null`; a missing
/// key is a shape violation, unlike serde's default `Option` leniency.
fn nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

fn check_texts(
    field: &str,
    items: &[String],
    min: usize,
    max: usize,
) -> Result<(), CompositionError> {
    check_items(field, items, min, max)?;
    items
        .iter()
        .try_for_each(|item| check_text(field, item, 1, MAX_TEXT_CHARS))
}

// ---------------------------------------------------------------------------
// Primitives
// ---------------------------------------------------------------------------

/// Model-local key or handle: `^[a-z][a-z0-9_]{0,63}$`. Scoped to one delivered
/// reference table or one response; never resolved globally.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Handle(String);

impl Handle {
    pub fn new(value: impl Into<String>) -> Result<Self, CompositionError> {
        let value = value.into();
        let mut bytes = value.bytes();
        let valid = value.len() <= 64
            && bytes.next().is_some_and(|first| first.is_ascii_lowercase())
            && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
        if !valid {
            return Err(CompositionError::identity(format!(
                "Handle `{value}` must match ^[a-z][a-z0-9_]{{0,63}}$."
            )));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Handle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for Handle {
    type Error = CompositionError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Handle> for String {
    fn from(value: Handle) -> Self {
        value.0
    }
}

/// Host-generated UUID in the canonical lowercase hyphenated form.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Uuid(String);

impl Uuid {
    pub fn new(value: impl Into<String>) -> Result<Self, CompositionError> {
        let value = value.into();
        let groups = [8usize, 4, 4, 4, 12];
        let parts = value.split('-').collect::<Vec<_>>();
        let valid = parts.len() == 5
            && parts.iter().zip(groups).all(|(part, len)| {
                part.len() == len
                    && part
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            });
        if !valid {
            return Err(CompositionError::identity(format!(
                "`{value}` is not a lowercase hyphenated UUID."
            )));
        }
        Ok(Self(value))
    }

    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for Uuid {
    type Error = CompositionError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Uuid> for String {
    fn from(value: Uuid) -> Self {
        value.0
    }
}

/// 64 lowercase hexadecimal characters over exact bytes. Uppercase is rejected
/// (the legacy `ContentSha256` normalizes it; this contract does not).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Sha256Hex(String);

impl Sha256Hex {
    pub fn new(value: impl Into<String>) -> Result<Self, CompositionError> {
        let value = value.into();
        let valid = value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        if !valid {
            return Err(CompositionError::identity(
                "SHA-256 must be 64 lowercase hexadecimal characters.",
            ));
        }
        Ok(Self(value))
    }

    pub fn of(bytes: &[u8]) -> Self {
        Self(crate::lower_hex(&Sha256::digest(bytes)))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Sha256Hex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for Sha256Hex {
    type Error = CompositionError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Sha256Hex> for String {
    fn from(value: Sha256Hex) -> Self {
        value.0
    }
}

impl From<ContentSha256> for Sha256Hex {
    fn from(value: ContentSha256) -> Self {
        // ContentSha256 lowercases on construction.
        Self(value.as_str().to_owned())
    }
}

impl From<&Sha256Hex> for ContentSha256 {
    fn from(value: &Sha256Hex) -> Self {
        ContentSha256::try_from(value.0.clone()).expect("lowercase hex is a valid ContentSha256")
    }
}

const WINDOWS_DEVICE_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Workspace-relative path with `/` separators. Rejects roots, drive/UNC
/// prefixes, `.`/`..` segments, empty segments, trailing dots/spaces, control
/// characters, `\ : < > " | ? *` and Windows reserved device names. Canonical
/// containment and reparse-point policy remain the publication service's job.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RelPath(String);

impl RelPath {
    pub fn new(value: impl Into<String>) -> Result<Self, CompositionError> {
        let value = value.into();
        let reject = |reason: &str| {
            Err(CompositionError::identity(format!(
                "Path `{value}` is not an admissible relative path: {reason}."
            )))
        };
        if value.is_empty() || scalar_len(&value) > 1024 {
            return reject("length must be 1-1024 characters");
        }
        if value.starts_with('/') {
            return reject("leading separator");
        }
        if value
            .chars()
            .any(|ch| ch.is_control() || "\\:<>\"|?*".contains(ch))
        {
            return reject("forbidden character");
        }
        for segment in value.split('/') {
            if segment.is_empty() {
                return reject("empty segment");
            }
            if matches!(segment, "." | "..") {
                return reject("dot segment");
            }
            if segment.ends_with(['.', ' ']) {
                return reject("segment ends with a dot or space");
            }
            let stem = segment.split('.').next().unwrap_or(segment);
            if WINDOWS_DEVICE_NAMES
                .iter()
                .any(|device| stem.eq_ignore_ascii_case(device))
            {
                return reject("Windows reserved device name");
            }
        }
        Ok(Self(value))
    }

    /// Lexically-validated path that the JSON Schema alone would accept, i.e.
    /// without the host-only device-name rule. Used by conformance tests to
    /// distinguish the two layers.
    pub fn schema_shape_only(value: &str) -> bool {
        !value.is_empty()
            && scalar_len(value) <= 1024
            && !value.starts_with('/')
            && !value
                .chars()
                .any(|ch| ch.is_control() || "\\:<>\"|?*".contains(ch))
            && value.split('/').all(|segment| {
                !segment.is_empty()
                    && !matches!(segment, "." | "..")
                    && !segment.ends_with(['.', ' '])
            })
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Ordinal case-insensitive identity used for Windows destination equality.
    pub fn canonical_key(&self) -> String {
        self.0.to_ascii_lowercase()
    }
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for RelPath {
    type Error = CompositionError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<RelPath> for String {
    fn from(value: RelPath) -> Self {
        value.0
    }
}

/// RFC 3339 UTC timestamp string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Timestamp(String);

impl Timestamp {
    pub fn new(value: impl Into<String>) -> Result<Self, CompositionError> {
        let value = value.into();
        chrono::DateTime::parse_from_rfc3339(&value)
            .map_err(|_| CompositionError::identity(format!("`{value}` is not RFC 3339.")))?;
        Ok(Self(value))
    }

    pub fn now() -> Self {
        Self(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn parsed(&self) -> chrono::DateTime<chrono::FixedOffset> {
        chrono::DateTime::parse_from_rfc3339(&self.0).expect("validated on construction")
    }
}

impl TryFrom<String> for Timestamp {
    type Error = CompositionError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Timestamp> for String {
    fn from(value: Timestamp) -> Self {
        value.0
    }
}

// ---------------------------------------------------------------------------
// Enumerations
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Intake,
    Specification,
    Contract,
    Code,
    Test,
    Integrator,
}

impl Stage {
    pub const ALL: [Self; 6] = [
        Self::Intake,
        Self::Specification,
        Self::Contract,
        Self::Code,
        Self::Test,
        Self::Integrator,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Intake => "intake",
            Self::Specification => "specification",
            Self::Contract => "contract",
            Self::Code => "code",
            Self::Test => "test",
            Self::Integrator => "integrator",
        }
    }

    pub const fn from_pulsation(stage: PulsationStage) -> Self {
        match stage {
            PulsationStage::Intake => Self::Intake,
            PulsationStage::Specification => Self::Specification,
            PulsationStage::Contract => Self::Contract,
            PulsationStage::Code => Self::Code,
            PulsationStage::Test => Self::Test,
            PulsationStage::Integrator => Self::Integrator,
        }
    }

    pub const fn to_pulsation(self) -> PulsationStage {
        match self {
            Self::Intake => PulsationStage::Intake,
            Self::Specification => PulsationStage::Specification,
            Self::Contract => PulsationStage::Contract,
            Self::Code => PulsationStage::Code,
            Self::Test => PulsationStage::Test,
            Self::Integrator => PulsationStage::Integrator,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutorKind {
    IntakeClassifier,
    SpecificationAuthor,
    ContractAuthor,
    CodeImplementer,
    CodeRepairer,
    TestAuthor,
    TestRepairer,
    IntegrationReviewer,
}

impl ExecutorKind {
    pub const ALL: [Self; 8] = [
        Self::IntakeClassifier,
        Self::SpecificationAuthor,
        Self::ContractAuthor,
        Self::CodeImplementer,
        Self::CodeRepairer,
        Self::TestAuthor,
        Self::TestRepairer,
        Self::IntegrationReviewer,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::IntakeClassifier => "intake_classifier",
            Self::SpecificationAuthor => "specification_author",
            Self::ContractAuthor => "contract_author",
            Self::CodeImplementer => "code_implementer",
            Self::CodeRepairer => "code_repairer",
            Self::TestAuthor => "test_author",
            Self::TestRepairer => "test_repairer",
            Self::IntegrationReviewer => "integration_reviewer",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == value)
    }

    pub const fn stage(self) -> Stage {
        match self {
            Self::IntakeClassifier => Stage::Intake,
            Self::SpecificationAuthor => Stage::Specification,
            Self::ContractAuthor => Stage::Contract,
            Self::CodeImplementer | Self::CodeRepairer => Stage::Code,
            Self::TestAuthor | Self::TestRepairer => Stage::Test,
            Self::IntegrationReviewer => Stage::Integrator,
        }
    }

    pub const fn is_repair(self) -> bool {
        matches!(self, Self::CodeRepairer | Self::TestRepairer)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceRole {
    Source,
    Test,
    Document,
    Configuration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableReason {
    Missing,
    Ambiguous,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskClassification {
    Feature,
    Bugfix,
    Refactor,
    Test,
    Documentation,
    Maintenance,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    Low,
    Medium,
    High,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationMode {
    ExecutedCheck,
    DocumentCheck,
    NotApplicable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterfaceIntent {
    Preserve,
    Change,
    New,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupIntent {
    Preserve,
    Change,
    New,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalKind {
    Code,
    Test,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageKind {
    Authored,
    Existing,
    NotApplicable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Supported,
    Contradicted,
    Unverified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerFailureCode {
    ContractConflict,
    ScopeInsufficient,
    ImplementationMismatch,
    UnsupportedOperation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationState {
    Validated,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Intake,
    Specification,
    Contract,
    Code,
    Test,
    IntegrationReport,
    Documentation,
    ValidationReport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptStatus {
    Passed,
    Failed,
    TimedOut,
    Cancelled,
    Unavailable,
    NotRun,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckKind {
    Compile,
    Test,
    Lint,
    Document,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptState {
    Reserved,
    Running,
    Succeeded,
    Rejected,
    Interrupted,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnitState {
    Pending,
    Running,
    Validated,
    Failed,
    Cancelled,
    Invalidated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Pending,
    Running,
    Passed,
    Failed,
    TimedOut,
    Cancelled,
    Unavailable,
    NotRun,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    Planned,
    Running,
    Validating,
    Ready,
    Accepted,
    Parked,
    Cancelled,
    Superseded,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageCompleteness {
    Complete,
    Partial,
    Unavailable,
}

// ---------------------------------------------------------------------------
// Reference table and plan
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ReferenceEntryV1 {
    ExistingSource {
        #[serde(rename = "ref")]
        handle: Handle,
        relative_path: RelPath,
        content_sha256: Sha256Hex,
        source_role: SourceRole,
    },
    PlannedDestination {
        #[serde(rename = "ref")]
        handle: Handle,
        relative_path: RelPath,
        source_role: SourceRole,
    },
    CandidateSource {
        #[serde(rename = "ref")]
        handle: Handle,
        relative_path: RelPath,
        content_sha256: Sha256Hex,
        producer_unit_id: Uuid,
        source_role: SourceRole,
    },
    Unavailable {
        #[serde(rename = "ref")]
        handle: Handle,
        reason: UnavailableReason,
        hint: String,
    },
}

impl ReferenceEntryV1 {
    pub fn handle(&self) -> &Handle {
        match self {
            Self::ExistingSource { handle, .. }
            | Self::PlannedDestination { handle, .. }
            | Self::CandidateSource { handle, .. }
            | Self::Unavailable { handle, .. } => handle,
        }
    }

    pub fn relative_path(&self) -> Option<&RelPath> {
        match self {
            Self::ExistingSource { relative_path, .. }
            | Self::PlannedDestination { relative_path, .. }
            | Self::CandidateSource { relative_path, .. } => Some(relative_path),
            Self::Unavailable { .. } => None,
        }
    }

    pub fn source_role(&self) -> Option<SourceRole> {
        match self {
            Self::ExistingSource { source_role, .. }
            | Self::PlannedDestination { source_role, .. }
            | Self::CandidateSource { source_role, .. } => Some(*source_role),
            Self::Unavailable { .. } => None,
        }
    }

    pub fn content_sha256(&self) -> Option<&Sha256Hex> {
        match self {
            Self::ExistingSource { content_sha256, .. }
            | Self::CandidateSource { content_sha256, .. } => Some(content_sha256),
            Self::PlannedDestination { .. } | Self::Unavailable { .. } => None,
        }
    }

    pub const fn is_writable_destination(&self) -> bool {
        matches!(
            self,
            Self::ExistingSource { .. }
                | Self::PlannedDestination { .. }
                | Self::CandidateSource { .. }
        )
    }

    pub const fn is_readable_input(&self) -> bool {
        matches!(
            self,
            Self::ExistingSource { .. } | Self::CandidateSource { .. }
        )
    }
}

impl Validate for ReferenceEntryV1 {
    fn validate(&self) -> Result<(), CompositionError> {
        if let Self::Unavailable { hint, .. } = self {
            check_text("hint", hint, 1, MAX_TEXT_CHARS)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskScopeV1 {
    pub task_ref: Handle,
    pub criterion_refs: Vec<Handle>,
}

impl Validate for TaskScopeV1 {
    fn validate(&self) -> Result<(), CompositionError> {
        check_unique_items("criterionRefs", &self.criterion_refs, 1, 64)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AnchorBindingV1 {
    pub anchor_ref: Handle,
    pub source_ref: Handle,
    pub source_sha256: Sha256Hex,
    pub start_byte: u32,
    pub end_byte: u32,
    pub region_sha256: Sha256Hex,
}

impl Validate for AnchorBindingV1 {
    fn validate(&self) -> Result<(), CompositionError> {
        check_max_int("startByte", self.start_byte, MAX_INT32)?;
        check_max_int("endByte", self.end_byte, MAX_INT32)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ParentFenceV1 {
    pub project_id: Uuid,
    pub pulsation_id: Uuid,
    pub input_revision: u32,
    pub job_id: Uuid,
    pub generation: u32,
    pub state_version: u32,
}

impl Validate for ParentFenceV1 {
    fn validate(&self) -> Result<(), CompositionError> {
        for (field, value) in [
            ("inputRevision", self.input_revision),
            ("generation", self.generation),
            ("stateVersion", self.state_version),
        ] {
            check_min_int(field, value, 1)?;
            check_max_int(field, value, MAX_INT32)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StageBudgetV1 {
    pub initial_invocation_limit: u32,
    pub extra_invocation_limit: u32,
    pub per_unit_extra_invocation_limit: u32,
    pub initial_output_token_limit: u32,
    pub extra_output_token_limit: u32,
    pub initial_prompt_character_limit: u32,
    pub extra_prompt_character_limit: u32,
    pub max_in_flight_units: u32,
}

impl Validate for StageBudgetV1 {
    fn validate(&self) -> Result<(), CompositionError> {
        check_max_int("initialInvocationLimit", self.initial_invocation_limit, 16)?;
        check_max_int("extraInvocationLimit", self.extra_invocation_limit, 2)?;
        if self.per_unit_extra_invocation_limit != 1 {
            return Err(shape(
                "`perUnitExtraInvocationLimit` must be exactly 1.".into(),
            ));
        }
        check_min_int(
            "initialOutputTokenLimit",
            self.initial_output_token_limit,
            1,
        )?;
        check_max_int(
            "initialOutputTokenLimit",
            self.initial_output_token_limit,
            MAX_INT32,
        )?;
        check_max_int(
            "extraOutputTokenLimit",
            self.extra_output_token_limit,
            MAX_INT32,
        )?;
        check_min_int(
            "initialPromptCharacterLimit",
            self.initial_prompt_character_limit,
            1,
        )?;
        check_max_int(
            "initialPromptCharacterLimit",
            self.initial_prompt_character_limit,
            MAX_INT32,
        )?;
        check_max_int(
            "extraPromptCharacterLimit",
            self.extra_prompt_character_limit,
            MAX_INT32,
        )?;
        check_min_int("maxInFlightUnits", self.max_in_flight_units, 1)?;
        check_max_int("maxInFlightUnits", self.max_in_flight_units, 2)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkUnitSpecV1 {
    pub schema_version: SchemaV1,
    pub unit_id: Uuid,
    pub logical_unit_key: Handle,
    pub stage: Stage,
    pub executor_kind: ExecutorKind,
    pub task_refs: Vec<Handle>,
    pub criterion_refs: Vec<Handle>,
    pub dependencies: Vec<Uuid>,
    pub input_refs: Vec<Handle>,
    pub owned_destination_refs: Vec<Handle>,
    pub required_check_refs: Vec<Handle>,
    pub objective: String,
    pub max_output_tokens: u32,
}

impl Validate for WorkUnitSpecV1 {
    fn validate(&self) -> Result<(), CompositionError> {
        check_unique_items("taskRefs", &self.task_refs, 1, 64)?;
        check_unique_items("criterionRefs", &self.criterion_refs, 1, 64)?;
        check_unique_items("dependencies", &self.dependencies, 0, 16)?;
        check_unique_items("inputRefs", &self.input_refs, 0, 64)?;
        check_unique_items("ownedDestinationRefs", &self.owned_destination_refs, 0, 64)?;
        check_unique_items("requiredCheckRefs", &self.required_check_refs, 0, 64)?;
        check_text("objective", &self.objective, 1, MAX_TEXT_CHARS)?;
        check_min_int("maxOutputTokens", self.max_output_tokens, 1)?;
        check_max_int("maxOutputTokens", self.max_output_tokens, MAX_INT32)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileBindingV1 {
    pub unit_id: Uuid,
    pub executor_kind: ExecutorKind,
    pub profile_snapshot_id: Uuid,
}

impl Validate for ProfileBindingV1 {
    fn validate(&self) -> Result<(), CompositionError> {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StageExecutionPlanV1 {
    pub schema_version: SchemaV1,
    pub stage_execution_id: Uuid,
    pub budget_account_id: Uuid,
    pub parent_fence: ParentFenceV1,
    pub stage: Stage,
    pub recipe_version: u8,
    pub policy_sha256: Sha256Hex,
    pub batch_sha256: Sha256Hex,
    #[serde(deserialize_with = "nullable")]
    pub approved_contract_sha256: Option<Sha256Hex>,
    pub plan_sha256: Sha256Hex,
    pub scope: Vec<TaskScopeV1>,
    pub references: Vec<ReferenceEntryV1>,
    pub anchors: Vec<AnchorBindingV1>,
    pub check_refs: Vec<Handle>,
    pub units: Vec<WorkUnitSpecV1>,
    pub profile_bindings: Vec<ProfileBindingV1>,
    pub budget: StageBudgetV1,
}

impl Validate for StageExecutionPlanV1 {
    fn validate(&self) -> Result<(), CompositionError> {
        if self.recipe_version != RECIPE_VERSION {
            return Err(shape(format!(
                "`recipeVersion` must be {RECIPE_VERSION}; got {}.",
                self.recipe_version
            )));
        }
        self.parent_fence.validate()?;
        check_items("scope", &self.scope, 1, 64)?;
        validate_all(&self.scope)?;
        check_items("references", &self.references, 0, 256)?;
        validate_all(&self.references)?;
        check_items("anchors", &self.anchors, 0, 256)?;
        validate_all(&self.anchors)?;
        check_unique_items("checkRefs", &self.check_refs, 0, 64)?;
        check_items("units", &self.units, 0, MAX_UNITS_PER_PLAN)?;
        validate_all(&self.units)?;
        check_items("profileBindings", &self.profile_bindings, 0, 32)?;
        validate_all(&self.profile_bindings)?;
        self.budget.validate()
    }
}

// ---------------------------------------------------------------------------
// Semantic payloads
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuestionV1 {
    pub key: Handle,
    pub task_refs: Vec<Handle>,
    pub question: String,
    pub rationale: String,
}

impl Validate for QuestionV1 {
    fn validate(&self) -> Result<(), CompositionError> {
        check_unique_items("taskRefs", &self.task_refs, 1, 64)?;
        check_text("question", &self.question, 1, MAX_TEXT_CHARS)?;
        check_text("rationale", &self.rationale, 1, MAX_TEXT_CHARS)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IntakeTaskV1 {
    pub task_ref: Handle,
    pub classification: TaskClassification,
    pub risk: Risk,
    pub summary: String,
}

impl Validate for IntakeTaskV1 {
    fn validate(&self) -> Result<(), CompositionError> {
        check_text("summary", &self.summary, 1, MAX_TEXT_CHARS)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IntakePayloadV1 {
    pub tasks: Vec<IntakeTaskV1>,
    pub issues: Vec<String>,
    pub used_refs: Vec<Handle>,
}

impl Validate for IntakePayloadV1 {
    fn validate(&self) -> Result<(), CompositionError> {
        check_items("tasks", &self.tasks, 1, 64)?;
        validate_all(&self.tasks)?;
        check_texts("issues", &self.issues, 0, 16)?;
        check_unique_items("usedRefs", &self.used_refs, 0, 64)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequirementV1 {
    pub task_ref: Handle,
    pub criterion_ref: Handle,
    pub requirement: String,
    pub verification_intent: String,
    pub verification_mode: VerificationMode,
    pub required_check_refs: Vec<Handle>,
    #[serde(deserialize_with = "nullable")]
    pub not_applicable_reason: Option<String>,
}

impl Validate for RequirementV1 {
    fn validate(&self) -> Result<(), CompositionError> {
        check_text("requirement", &self.requirement, 1, MAX_TEXT_CHARS)?;
        check_text(
            "verificationIntent",
            &self.verification_intent,
            1,
            MAX_TEXT_CHARS,
        )?;
        check_unique_items("requiredCheckRefs", &self.required_check_refs, 0, 64)?;
        if let Some(reason) = &self.not_applicable_reason {
            check_text("notApplicableReason", reason, 1, MAX_TEXT_CHARS)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InterfaceIntentV1 {
    pub intent: InterfaceIntent,
    pub source_refs: Vec<Handle>,
    pub criterion_refs: Vec<Handle>,
    pub description: String,
}

impl Validate for InterfaceIntentV1 {
    fn validate(&self) -> Result<(), CompositionError> {
        check_unique_items("sourceRefs", &self.source_refs, 0, 64)?;
        check_unique_items("criterionRefs", &self.criterion_refs, 1, 64)?;
        check_text("description", &self.description, 1, MAX_TEXT_CHARS)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UnitProposalV1 {
    pub unit_key: Handle,
    pub kind: ProposalKind,
    pub task_refs: Vec<Handle>,
    pub criterion_refs: Vec<Handle>,
    pub input_refs: Vec<Handle>,
    pub destination_refs: Vec<Handle>,
    pub depends_on: Vec<Handle>,
    pub objective: String,
}

impl Validate for UnitProposalV1 {
    fn validate(&self) -> Result<(), CompositionError> {
        check_unique_items("taskRefs", &self.task_refs, 1, 64)?;
        check_unique_items("criterionRefs", &self.criterion_refs, 1, 64)?;
        check_unique_items("inputRefs", &self.input_refs, 0, 64)?;
        check_unique_items("destinationRefs", &self.destination_refs, 0, 64)?;
        check_unique_items("dependsOn", &self.depends_on, 0, 16)?;
        check_text("objective", &self.objective, 1, MAX_TEXT_CHARS)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SpecificationContractV1 {
    pub objective: String,
    pub exclusions: Vec<String>,
    pub decisions: Vec<String>,
    pub requirements: Vec<RequirementV1>,
    pub existing_file_refs: Vec<Handle>,
    pub planned_file_refs: Vec<Handle>,
    pub interface_intent: Vec<InterfaceIntentV1>,
    pub unit_proposals: Vec<UnitProposalV1>,
}

impl Validate for SpecificationContractV1 {
    fn validate(&self) -> Result<(), CompositionError> {
        check_text("objective", &self.objective, 1, MAX_TEXT_CHARS)?;
        check_texts("exclusions", &self.exclusions, 0, 32)?;
        check_texts("decisions", &self.decisions, 0, 32)?;
        check_items("requirements", &self.requirements, 1, 64)?;
        validate_all(&self.requirements)?;
        check_unique_items("existingFileRefs", &self.existing_file_refs, 0, 64)?;
        check_unique_items("plannedFileRefs", &self.planned_file_refs, 0, 64)?;
        check_items("interfaceIntent", &self.interface_intent, 1, 64)?;
        validate_all(&self.interface_intent)?;
        check_items("unitProposals", &self.unit_proposals, 0, MAX_UNITS_PER_PLAN)?;
        validate_all(&self.unit_proposals)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InterfaceGroupV1 {
    pub interface_key: Handle,
    pub intent: GroupIntent,
    pub source_refs: Vec<Handle>,
    pub criterion_refs: Vec<Handle>,
    pub signature: String,
    pub invariants: Vec<String>,
}

impl Validate for InterfaceGroupV1 {
    fn validate(&self) -> Result<(), CompositionError> {
        check_unique_items("sourceRefs", &self.source_refs, 0, 64)?;
        check_unique_items("criterionRefs", &self.criterion_refs, 1, 64)?;
        check_text("signature", &self.signature, 1, MAX_SIGNATURE_CHARS)?;
        check_texts("invariants", &self.invariants, 1, 32)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UnitInterfaceBindingV1 {
    pub unit_key: Handle,
    pub interface_keys: Vec<Handle>,
    pub depends_on: Vec<Handle>,
}

impl Validate for UnitInterfaceBindingV1 {
    fn validate(&self) -> Result<(), CompositionError> {
        check_unique_items("interfaceKeys", &self.interface_keys, 0, 64)?;
        check_unique_items("dependsOn", &self.depends_on, 0, 16)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InterfaceContractV1 {
    pub groups: Vec<InterfaceGroupV1>,
    pub unit_bindings: Vec<UnitInterfaceBindingV1>,
    pub used_refs: Vec<Handle>,
}

impl Validate for InterfaceContractV1 {
    fn validate(&self) -> Result<(), CompositionError> {
        check_items("groups", &self.groups, 0, 32)?;
        validate_all(&self.groups)?;
        check_items("unitBindings", &self.unit_bindings, 0, MAX_UNITS_PER_PLAN)?;
        validate_all(&self.unit_bindings)?;
        check_unique_items("usedRefs", &self.used_refs, 0, 64)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "operation",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum EditOperationV1 {
    CreateFile {
        destination_ref: Handle,
        content: String,
    },
    ReplaceFile {
        source_ref: Handle,
        content: String,
    },
    ReplaceAnchor {
        anchor_ref: Handle,
        content: String,
    },
}

impl EditOperationV1 {
    pub fn content(&self) -> &str {
        match self {
            Self::CreateFile { content, .. }
            | Self::ReplaceFile { content, .. }
            | Self::ReplaceAnchor { content, .. } => content,
        }
    }

    /// The handle the operation targets: a destination, a source or an anchor.
    pub fn target(&self) -> &Handle {
        match self {
            Self::CreateFile {
                destination_ref, ..
            } => destination_ref,
            Self::ReplaceFile { source_ref, .. } => source_ref,
            Self::ReplaceAnchor { anchor_ref, .. } => anchor_ref,
        }
    }

    pub const fn name(&self) -> &'static str {
        match self {
            Self::CreateFile { .. } => "create_file",
            Self::ReplaceFile { .. } => "replace_file",
            Self::ReplaceAnchor { .. } => "replace_anchor",
        }
    }
}

impl Validate for EditOperationV1 {
    fn validate(&self) -> Result<(), CompositionError> {
        match self {
            Self::CreateFile { content, .. } | Self::ReplaceAnchor { content, .. } => {
                check_text("content", content, 0, MAX_ARTIFACT_BYTES)
            }
            Self::ReplaceFile { content, .. } => {
                check_text("content", content, 1, MAX_REPLACE_FILE_BYTES)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EditEntryV1 {
    pub artifact_key: Handle,
    pub edit: EditOperationV1,
}

impl Validate for EditEntryV1 {
    fn validate(&self) -> Result<(), CompositionError> {
        self.edit.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvidenceItemV1 {
    pub task_ref: Handle,
    pub criterion_refs: Vec<Handle>,
    pub artifact_keys: Vec<Handle>,
    pub input_refs: Vec<Handle>,
    pub explanation: String,
}

impl Validate for EvidenceItemV1 {
    fn validate(&self) -> Result<(), CompositionError> {
        check_unique_items("criterionRefs", &self.criterion_refs, 1, 64)?;
        check_unique_items(
            "artifactKeys",
            &self.artifact_keys,
            0,
            MAX_AGGREGATE_ARTIFACTS,
        )?;
        check_unique_items("inputRefs", &self.input_refs, 0, 64)?;
        check_text("explanation", &self.explanation, 1, MAX_TEXT_CHARS)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EditSetV1 {
    pub edits: Vec<EditEntryV1>,
    pub evidence: Vec<EvidenceItemV1>,
    pub used_refs: Vec<Handle>,
}

impl Validate for EditSetV1 {
    fn validate(&self) -> Result<(), CompositionError> {
        check_items("edits", &self.edits, 1, MAX_EDITS_PER_PAYLOAD)?;
        validate_all(&self.edits)?;
        check_items("evidence", &self.evidence, 1, 64)?;
        validate_all(&self.evidence)?;
        check_unique_items("usedRefs", &self.used_refs, 0, 64)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TestCoverageV1 {
    pub task_ref: Handle,
    pub criterion_ref: Handle,
    pub coverage: CoverageKind,
    pub test_artifact_keys: Vec<Handle>,
    pub test_input_refs: Vec<Handle>,
    pub check_refs: Vec<Handle>,
    pub explanation: String,
}

impl Validate for TestCoverageV1 {
    fn validate(&self) -> Result<(), CompositionError> {
        check_unique_items(
            "testArtifactKeys",
            &self.test_artifact_keys,
            0,
            MAX_AGGREGATE_ARTIFACTS,
        )?;
        check_unique_items("testInputRefs", &self.test_input_refs, 0, 64)?;
        check_unique_items("checkRefs", &self.check_refs, 0, 64)?;
        check_text("explanation", &self.explanation, 1, MAX_TEXT_CHARS)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TestPayloadV1 {
    pub edits: Vec<EditEntryV1>,
    pub coverage: Vec<TestCoverageV1>,
    pub evidence: Vec<EvidenceItemV1>,
    pub used_refs: Vec<Handle>,
}

impl Validate for TestPayloadV1 {
    fn validate(&self) -> Result<(), CompositionError> {
        check_items("edits", &self.edits, 0, MAX_EDITS_PER_PAYLOAD)?;
        validate_all(&self.edits)?;
        check_items("coverage", &self.coverage, 1, 64)?;
        validate_all(&self.coverage)?;
        check_items("evidence", &self.evidence, 1, 64)?;
        validate_all(&self.evidence)?;
        check_unique_items("usedRefs", &self.used_refs, 0, 64)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CriterionAssessmentV1 {
    pub task_ref: Handle,
    pub criterion_ref: Handle,
    pub verdict: Verdict,
    pub input_refs: Vec<Handle>,
    pub check_refs: Vec<Handle>,
    pub explanation: String,
}

impl Validate for CriterionAssessmentV1 {
    fn validate(&self) -> Result<(), CompositionError> {
        check_unique_items("inputRefs", &self.input_refs, 0, 64)?;
        check_unique_items("checkRefs", &self.check_refs, 0, 64)?;
        check_text("explanation", &self.explanation, 1, MAX_TEXT_CHARS)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IntegrationPayloadV1 {
    pub assessments: Vec<CriterionAssessmentV1>,
    pub blockers: Vec<String>,
    pub documentation_edits: Vec<EditEntryV1>,
    pub used_refs: Vec<Handle>,
}

impl Validate for IntegrationPayloadV1 {
    fn validate(&self) -> Result<(), CompositionError> {
        check_items("assessments", &self.assessments, 1, 64)?;
        validate_all(&self.assessments)?;
        check_texts("blockers", &self.blockers, 0, 32)?;
        check_items(
            "documentationEdits",
            &self.documentation_edits,
            0,
            MAX_AGGREGATE_ARTIFACTS,
        )?;
        validate_all(&self.documentation_edits)?;
        check_unique_items("usedRefs", &self.used_refs, 0, 64)
    }
}

// ---------------------------------------------------------------------------
// Model response (closed tagged union with executor-dependent payload)
// ---------------------------------------------------------------------------

/// A completed worker payload, typed by the specialty that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletedPayloadV1 {
    Intake(IntakePayloadV1),
    Specification(SpecificationContractV1),
    Contract(InterfaceContractV1),
    Edits(EditSetV1),
    Tests(TestPayloadV1),
    Integration(IntegrationPayloadV1),
}

impl CompletedPayloadV1 {
    /// Which specialties may produce this payload shape.
    pub fn accepts(&self, kind: ExecutorKind) -> bool {
        matches!(
            (self, kind),
            (Self::Intake(_), ExecutorKind::IntakeClassifier)
                | (Self::Specification(_), ExecutorKind::SpecificationAuthor)
                | (Self::Contract(_), ExecutorKind::ContractAuthor)
                | (
                    Self::Edits(_),
                    ExecutorKind::CodeImplementer | ExecutorKind::CodeRepairer
                )
                | (
                    Self::Tests(_),
                    ExecutorKind::TestAuthor | ExecutorKind::TestRepairer
                )
                | (Self::Integration(_), ExecutorKind::IntegrationReviewer)
        )
    }

    fn from_value(kind: ExecutorKind, value: Value) -> Result<Self, serde_json::Error> {
        Ok(match kind {
            ExecutorKind::IntakeClassifier => Self::Intake(serde_json::from_value(value)?),
            ExecutorKind::SpecificationAuthor => {
                Self::Specification(serde_json::from_value(value)?)
            }
            ExecutorKind::ContractAuthor => Self::Contract(serde_json::from_value(value)?),
            ExecutorKind::CodeImplementer | ExecutorKind::CodeRepairer => {
                Self::Edits(serde_json::from_value(value)?)
            }
            ExecutorKind::TestAuthor | ExecutorKind::TestRepairer => {
                Self::Tests(serde_json::from_value(value)?)
            }
            ExecutorKind::IntegrationReviewer => Self::Integration(serde_json::from_value(value)?),
        })
    }

    fn to_value(&self) -> Value {
        match self {
            Self::Intake(payload) => serde_json::to_value(payload),
            Self::Specification(payload) => serde_json::to_value(payload),
            Self::Contract(payload) => serde_json::to_value(payload),
            Self::Edits(payload) => serde_json::to_value(payload),
            Self::Tests(payload) => serde_json::to_value(payload),
            Self::Integration(payload) => serde_json::to_value(payload),
        }
        .expect("contract payloads serialize")
    }

    /// Edits carried by the payload, if any.
    pub fn edits(&self) -> &[EditEntryV1] {
        match self {
            Self::Edits(payload) => &payload.edits,
            Self::Tests(payload) => &payload.edits,
            Self::Integration(payload) => &payload.documentation_edits,
            Self::Intake(_) | Self::Specification(_) | Self::Contract(_) => &[],
        }
    }

    pub fn evidence(&self) -> &[EvidenceItemV1] {
        match self {
            Self::Edits(payload) => &payload.evidence,
            Self::Tests(payload) => &payload.evidence,
            Self::Intake(_) | Self::Specification(_) | Self::Contract(_) | Self::Integration(_) => {
                &[]
            }
        }
    }

    pub fn used_refs(&self) -> &[Handle] {
        match self {
            Self::Intake(payload) => &payload.used_refs,
            Self::Specification(_) => &[],
            Self::Contract(payload) => &payload.used_refs,
            Self::Edits(payload) => &payload.used_refs,
            Self::Tests(payload) => &payload.used_refs,
            Self::Integration(payload) => &payload.used_refs,
        }
    }
}

impl Validate for CompletedPayloadV1 {
    fn validate(&self) -> Result<(), CompositionError> {
        match self {
            Self::Intake(payload) => payload.validate(),
            Self::Specification(payload) => payload.validate(),
            Self::Contract(payload) => payload.validate(),
            Self::Edits(payload) => payload.validate(),
            Self::Tests(payload) => payload.validate(),
            Self::Integration(payload) => payload.validate(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnitModelResponseV1 {
    Completed {
        executor_kind: ExecutorKind,
        payload: CompletedPayloadV1,
    },
    NeedsInput {
        missing_refs: Vec<Handle>,
        questions: Vec<QuestionV1>,
    },
    Failed {
        code: WorkerFailureCode,
        message: String,
        related_refs: Vec<Handle>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Completed,
    NeedsInput,
    Failed,
}

/// The flat wire shape. Every optional field is either required or forbidden
/// depending on `outcome`; the typed conversion enforces that.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawUnitModelResponse {
    schema_version: SchemaV1,
    outcome: Outcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    executor_kind: Option<ExecutorKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    payload: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    missing_refs: Option<Vec<Handle>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    questions: Option<Vec<QuestionV1>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    code: Option<WorkerFailureCode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    related_refs: Option<Vec<Handle>>,
}

impl<'de> Deserialize<'de> for UnitModelResponseV1 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = RawUnitModelResponse::deserialize(deserializer)?;
        let forbid = |present: bool, field: &str, outcome: &str| {
            if present {
                Err(D::Error::custom(format!(
                    "`{field}` is not allowed when outcome is `{outcome}`"
                )))
            } else {
                Ok(())
            }
        };
        match raw.outcome {
            Outcome::Completed => {
                forbid(raw.missing_refs.is_some(), "missingRefs", "completed")?;
                forbid(raw.questions.is_some(), "questions", "completed")?;
                forbid(raw.code.is_some(), "code", "completed")?;
                forbid(raw.message.is_some(), "message", "completed")?;
                forbid(raw.related_refs.is_some(), "relatedRefs", "completed")?;
                let executor_kind = raw
                    .executor_kind
                    .ok_or_else(|| D::Error::missing_field("executorKind"))?;
                let payload = raw
                    .payload
                    .ok_or_else(|| D::Error::missing_field("payload"))?;
                let payload = CompletedPayloadV1::from_value(executor_kind, payload)
                    .map_err(|error| D::Error::custom(format!("payload: {error}")))?;
                Ok(Self::Completed {
                    executor_kind,
                    payload,
                })
            }
            Outcome::NeedsInput => {
                forbid(raw.executor_kind.is_some(), "executorKind", "needs_input")?;
                forbid(raw.payload.is_some(), "payload", "needs_input")?;
                forbid(raw.code.is_some(), "code", "needs_input")?;
                forbid(raw.message.is_some(), "message", "needs_input")?;
                forbid(raw.related_refs.is_some(), "relatedRefs", "needs_input")?;
                Ok(Self::NeedsInput {
                    missing_refs: raw
                        .missing_refs
                        .ok_or_else(|| D::Error::missing_field("missingRefs"))?,
                    questions: raw
                        .questions
                        .ok_or_else(|| D::Error::missing_field("questions"))?,
                })
            }
            Outcome::Failed => {
                forbid(raw.executor_kind.is_some(), "executorKind", "failed")?;
                forbid(raw.payload.is_some(), "payload", "failed")?;
                forbid(raw.missing_refs.is_some(), "missingRefs", "failed")?;
                forbid(raw.questions.is_some(), "questions", "failed")?;
                Ok(Self::Failed {
                    code: raw.code.ok_or_else(|| D::Error::missing_field("code"))?,
                    message: raw
                        .message
                        .ok_or_else(|| D::Error::missing_field("message"))?,
                    related_refs: raw
                        .related_refs
                        .ok_or_else(|| D::Error::missing_field("relatedRefs"))?,
                })
            }
        }
    }
}

impl Serialize for UnitModelResponseV1 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut raw = RawUnitModelResponse {
            schema_version: SchemaV1,
            outcome: Outcome::Completed,
            executor_kind: None,
            payload: None,
            missing_refs: None,
            questions: None,
            code: None,
            message: None,
            related_refs: None,
        };
        match self {
            Self::Completed {
                executor_kind,
                payload,
            } => {
                raw.executor_kind = Some(*executor_kind);
                raw.payload = Some(payload.to_value());
            }
            Self::NeedsInput {
                missing_refs,
                questions,
            } => {
                raw.outcome = Outcome::NeedsInput;
                raw.missing_refs = Some(missing_refs.clone());
                raw.questions = Some(questions.clone());
            }
            Self::Failed {
                code,
                message,
                related_refs,
            } => {
                raw.outcome = Outcome::Failed;
                raw.code = Some(*code);
                raw.message = Some(message.clone());
                raw.related_refs = Some(related_refs.clone());
            }
        }
        raw.serialize(serializer)
    }
}

impl Validate for UnitModelResponseV1 {
    fn validate(&self) -> Result<(), CompositionError> {
        match self {
            Self::Completed {
                executor_kind,
                payload,
            } => {
                if !payload.accepts(*executor_kind) {
                    return Err(shape(format!(
                        "payload shape does not belong to `{}`.",
                        executor_kind.as_str()
                    )));
                }
                payload.validate()
            }
            Self::NeedsInput {
                missing_refs,
                questions,
            } => {
                check_unique_items("missingRefs", missing_refs, 0, 64)?;
                check_items("questions", questions, 1, MAX_QUESTIONS)?;
                validate_all(questions)
            }
            Self::Failed {
                message,
                related_refs,
                ..
            } => {
                check_text("message", message, 1, MAX_TEXT_CHARS)?;
                check_unique_items("relatedRefs", related_refs, 0, 64)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Host envelopes
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkUnitResultV1 {
    pub schema_version: SchemaV1,
    pub stage_execution_id: Uuid,
    pub unit_id: Uuid,
    pub attempt_id: Uuid,
    pub unit_generation: u32,
    pub input_sha256: Sha256Hex,
    pub result_sha256: Sha256Hex,
    pub validation_state: ValidationState,
    pub profile_snapshot_id: Uuid,
    pub original_attempt_id: Uuid,
    pub response: UnitModelResponseV1,
}

impl Validate for WorkUnitResultV1 {
    fn validate(&self) -> Result<(), CompositionError> {
        check_min_int("unitGeneration", self.unit_generation, 1)?;
        check_max_int("unitGeneration", self.unit_generation, MAX_INT32)?;
        self.response.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StageArtifactV1 {
    pub artifact_key: Handle,
    #[serde(deserialize_with = "nullable")]
    pub relative_path: Option<RelPath>,
    pub kind: ArtifactKind,
    pub content: String,
    pub content_sha256: Sha256Hex,
}

impl Validate for StageArtifactV1 {
    fn validate(&self) -> Result<(), CompositionError> {
        check_text("content", &self.content, 0, MAX_ARTIFACT_BYTES)
    }
}

impl StageArtifactV1 {
    /// Host identity rule (SC-003): the digest is over the exact content bytes,
    /// the content stays within the publication limit and contains no NUL.
    pub fn check_content_identity(&self) -> Result<(), CompositionError> {
        if self.content.len() > MAX_ARTIFACT_BYTES || self.content.as_bytes().contains(&0) {
            return Err(CompositionError::identity(
                "artifact content exceeds 1 MiB or contains NUL.",
            ));
        }
        if Sha256Hex::of(self.content.as_bytes()) != self.content_sha256 {
            return Err(CompositionError::identity(
                "artifact contentSha256 does not match the exact content bytes.",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StageAggregateV1 {
    pub schema_version: SchemaV1,
    pub stage_execution_id: Uuid,
    pub stage: Stage,
    pub plan_sha256: Sha256Hex,
    pub artifacts: Vec<StageArtifactV1>,
    pub task_evidence: Vec<EvidenceItemV1>,
    pub validation_receipt_ids: Vec<Uuid>,
    pub findings: Vec<String>,
}

impl Validate for StageAggregateV1 {
    fn validate(&self) -> Result<(), CompositionError> {
        check_items("artifacts", &self.artifacts, 1, MAX_AGGREGATE_ARTIFACTS)?;
        validate_all(&self.artifacts)?;
        check_items("taskEvidence", &self.task_evidence, 1, 64)?;
        validate_all(&self.task_evidence)?;
        check_unique_items("validationReceiptIds", &self.validation_receipt_ids, 0, 64)?;
        check_texts("findings", &self.findings, 0, 64)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LogExcerptV1 {
    pub text: String,
    pub truncated: bool,
}

impl Validate for LogExcerptV1 {
    fn validate(&self) -> Result<(), CompositionError> {
        check_text("text", &self.text, 0, MAX_LOG_BYTES)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiagnosticV1 {
    pub severity: Severity,
    pub code: Handle,
    pub message: String,
    #[serde(deserialize_with = "nullable")]
    pub source_ref: Option<Handle>,
}

impl Validate for DiagnosticV1 {
    fn validate(&self) -> Result<(), CompositionError> {
        check_text("message", &self.message, 1, MAX_TEXT_CHARS)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ValidationReceiptV1 {
    pub schema_version: SchemaV1,
    pub receipt_id: Uuid,
    pub stage_execution_id: Uuid,
    pub parent_generation: u32,
    pub check_ref: Handle,
    pub profile_snapshot_id: Uuid,
    pub profile_sha256: Sha256Hex,
    pub candidate_sha256: Sha256Hex,
    pub input_manifest_sha256: Sha256Hex,
    pub toolchain_sha256: Sha256Hex,
    pub environment_sha256: Sha256Hex,
    pub status: ReceiptStatus,
    #[serde(deserialize_with = "nullable")]
    pub exit_code: Option<i32>,
    #[serde(deserialize_with = "nullable")]
    pub started_at: Option<Timestamp>,
    #[serde(deserialize_with = "nullable")]
    pub finished_at: Option<Timestamp>,
    pub stdout: LogExcerptV1,
    pub stderr: LogExcerptV1,
    pub diagnostics: Vec<DiagnosticV1>,
    #[serde(deserialize_with = "nullable")]
    pub baseline_receipt_id: Option<Uuid>,
}

impl Validate for ValidationReceiptV1 {
    fn validate(&self) -> Result<(), CompositionError> {
        check_min_int("parentGeneration", self.parent_generation, 1)?;
        check_max_int("parentGeneration", self.parent_generation, MAX_INT32)?;
        self.stdout.validate()?;
        self.stderr.validate()?;
        check_items("diagnostics", &self.diagnostics, 0, 64)?;
        validate_all(&self.diagnostics)
    }
}

impl ValidationReceiptV1 {
    /// Host-only execution semantics (`Stage-Composition-Validation-Contract.md` §2):
    /// `passed` requires a process that started, finished and exited 0;
    /// never-launched statuses carry no exit code or timestamps.
    pub fn check_execution_semantics(&self) -> Result<(), CompositionError> {
        let launched = self.started_at.is_some();
        let finished = self.finished_at.is_some();
        let fail = |message: &str| {
            Err(CompositionError::new(
                Invariant::Sc011,
                ErrorCode::HostContractError,
                message,
            ))
        };
        match self.status {
            ReceiptStatus::Passed => {
                if !launched || !finished || self.exit_code != Some(0) {
                    return fail("`passed` requires a started, finished process with exit code 0.");
                }
            }
            ReceiptStatus::Failed | ReceiptStatus::Error => {
                if !launched && self.status == ReceiptStatus::Failed {
                    return fail("`failed` requires execution evidence.");
                }
            }
            ReceiptStatus::TimedOut | ReceiptStatus::Cancelled => {
                if !launched {
                    return fail("interrupted statuses require a started process.");
                }
            }
            ReceiptStatus::Unavailable | ReceiptStatus::NotRun => {
                if launched || finished || self.exit_code.is_some() {
                    return fail("`unavailable`/`not_run` must carry no execution data.");
                }
            }
        }
        if let (Some(started), Some(finished)) = (&self.started_at, &self.finished_at)
            && finished.parsed() < started.parsed()
        {
            return fail("`finishedAt` precedes `startedAt`.");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ValidationCommandProfileV1 {
    pub schema_version: SchemaV1,
    pub profile_id: Uuid,
    pub profile_version: u32,
    pub check_ref: Handle,
    pub kind: CheckKind,
    pub required: bool,
    pub executable: String,
    pub argv: Vec<String>,
    pub cwd_relative_path: String,
    pub input_globs: Vec<String>,
    pub environment_names: Vec<String>,
    pub toolchain_identity: String,
    pub timeout_seconds: u32,
    pub max_log_bytes: u32,
}

impl ValidationCommandProfileV1 {
    /// The executable text when it names a shell wrapper or shortcut, which
    /// is never a launch target (host rule, validation contract §1).
    pub fn resolved_wrapper_hint(&self) -> Option<&str> {
        let lower = self.executable.to_ascii_lowercase();
        [".cmd", ".bat", ".ps1", ".lnk", ".vbs"]
            .iter()
            .any(|suffix| lower.ends_with(suffix))
            .then_some(self.executable.as_str())
    }
}

impl Validate for ValidationCommandProfileV1 {
    fn validate(&self) -> Result<(), CompositionError> {
        check_min_int("profileVersion", self.profile_version, 1)?;
        check_max_int("profileVersion", self.profile_version, MAX_INT32)?;
        check_text("executable", &self.executable, 1, 4096)?;
        check_items("argv", &self.argv, 0, 128)?;
        self.argv
            .iter()
            .try_for_each(|arg| check_text("argv", arg, 0, 4096))?;
        if self.cwd_relative_path != "." {
            RelPath::new(self.cwd_relative_path.clone())?;
        }
        check_unique_items("inputGlobs", &self.input_globs, 1, 64)?;
        self.input_globs
            .iter()
            .try_for_each(|glob| check_text("inputGlobs", glob, 1, 1024))?;
        check_unique_items("environmentNames", &self.environment_names, 0, 64)?;
        for name in &self.environment_names {
            let mut bytes = name.bytes();
            let valid = name.len() <= 128
                && bytes
                    .next()
                    .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
                && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
            if !valid {
                return Err(shape(format!(
                    "`environmentNames` entry `{name}` is invalid."
                )));
            }
        }
        check_text(
            "toolchainIdentity",
            &self.toolchain_identity,
            1,
            MAX_TEXT_CHARS,
        )?;
        check_min_int("timeoutSeconds", self.timeout_seconds, 1)?;
        check_max_int("timeoutSeconds", self.timeout_seconds, 900)?;
        check_min_int("maxLogBytes", self.max_log_bytes, 1)?;
        check_max_int("maxLogBytes", self.max_log_bytes, MAX_LOG_BYTES as u32)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "action",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum StageRecoveryCommandV1 {
    Retry {
        schema_version: SchemaV1,
        project_id: Uuid,
        pulsation_id: Uuid,
        stage_execution_id: Uuid,
        request_id: Uuid,
        expected_parent_generation: u32,
        expected_parent_state_version: u32,
        expected_execution_version: u32,
        #[serde(deserialize_with = "nullable")]
        profile_snapshot_id: Option<Uuid>,
    },
    Revise {
        schema_version: SchemaV1,
        project_id: Uuid,
        pulsation_id: Uuid,
        stage_execution_id: Uuid,
        request_id: Uuid,
        expected_parent_generation: u32,
        expected_parent_state_version: u32,
        expected_execution_version: u32,
        target_stage: Stage,
        reason: String,
    },
    Abort {
        schema_version: SchemaV1,
        project_id: Uuid,
        pulsation_id: Uuid,
        stage_execution_id: Uuid,
        request_id: Uuid,
        expected_parent_generation: u32,
        expected_parent_state_version: u32,
        expected_execution_version: u32,
    },
}

impl StageRecoveryCommandV1 {
    pub fn versions(&self) -> (u32, u32, u32) {
        match self {
            Self::Retry {
                expected_parent_generation,
                expected_parent_state_version,
                expected_execution_version,
                ..
            }
            | Self::Revise {
                expected_parent_generation,
                expected_parent_state_version,
                expected_execution_version,
                ..
            }
            | Self::Abort {
                expected_parent_generation,
                expected_parent_state_version,
                expected_execution_version,
                ..
            } => (
                *expected_parent_generation,
                *expected_parent_state_version,
                *expected_execution_version,
            ),
        }
    }
}

impl Validate for StageRecoveryCommandV1 {
    fn validate(&self) -> Result<(), CompositionError> {
        let (generation, state, execution) = self.versions();
        for (field, value) in [
            ("expectedParentGeneration", generation),
            ("expectedParentStateVersion", state),
            ("expectedExecutionVersion", execution),
        ] {
            check_min_int(field, value, 1)?;
            check_max_int(field, value, MAX_INT32)?;
        }
        if let Self::Revise { reason, .. } = self {
            check_text("reason", reason, 1, MAX_TEXT_CHARS)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UsageSummaryV1 {
    #[serde(deserialize_with = "nullable")]
    pub input_tokens: Option<u32>,
    #[serde(deserialize_with = "nullable")]
    pub output_tokens: Option<u32>,
    #[serde(deserialize_with = "nullable")]
    pub reasoning_tokens: Option<u32>,
    #[serde(deserialize_with = "nullable")]
    pub cached_input_tokens: Option<u32>,
    pub completeness: UsageCompleteness,
}

impl UsageSummaryV1 {
    pub const fn unavailable() -> Self {
        Self {
            input_tokens: None,
            output_tokens: None,
            reasoning_tokens: None,
            cached_input_tokens: None,
            completeness: UsageCompleteness::Unavailable,
        }
    }
}

impl Validate for UsageSummaryV1 {
    fn validate(&self) -> Result<(), CompositionError> {
        for (field, value) in [
            ("inputTokens", self.input_tokens),
            ("outputTokens", self.output_tokens),
            ("reasoningTokens", self.reasoning_tokens),
            ("cachedInputTokens", self.cached_input_tokens),
        ] {
            if let Some(value) = value {
                check_max_int(field, value, MAX_INT32)?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttemptSummaryV1 {
    pub attempt_id: Uuid,
    pub state: AttemptState,
    pub executor_kind: ExecutorKind,
    pub effective_model: String,
    pub profile_snapshot_id: Uuid,
    #[serde(deserialize_with = "nullable")]
    pub error_code: Option<Handle>,
    pub usage: UsageSummaryV1,
}

impl Validate for AttemptSummaryV1 {
    fn validate(&self) -> Result<(), CompositionError> {
        check_text("effectiveModel", &self.effective_model, 1, 256)?;
        self.usage.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UnitSnapshotV1 {
    pub unit_id: Uuid,
    pub logical_unit_key: Handle,
    pub executor_kind: ExecutorKind,
    pub generation: u32,
    pub state_version: u32,
    pub state: UnitState,
    #[serde(deserialize_with = "nullable")]
    pub reused_from_attempt_id: Option<Uuid>,
    pub attempts: Vec<AttemptSummaryV1>,
}

impl Validate for UnitSnapshotV1 {
    fn validate(&self) -> Result<(), CompositionError> {
        check_min_int("generation", self.generation, 1)?;
        check_max_int("generation", self.generation, MAX_INT32)?;
        check_min_int("stateVersion", self.state_version, 1)?;
        check_max_int("stateVersion", self.state_version, MAX_INT32)?;
        check_items("attempts", &self.attempts, 0, 2)?;
        validate_all(&self.attempts)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CheckSummaryV1 {
    pub check_ref: Handle,
    pub required: bool,
    #[serde(deserialize_with = "nullable")]
    pub receipt_id: Option<Uuid>,
    pub status: CheckStatus,
}

impl Validate for CheckSummaryV1 {
    fn validate(&self) -> Result<(), CompositionError> {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemainingBudgetV1 {
    pub initial_invocations: u32,
    pub extra_invocations: u32,
    pub initial_output_tokens: u32,
    pub extra_output_tokens: u32,
    pub initial_prompt_characters: u32,
    pub extra_prompt_characters: u32,
}

impl Validate for RemainingBudgetV1 {
    fn validate(&self) -> Result<(), CompositionError> {
        check_max_int("initialInvocations", self.initial_invocations, 16)?;
        check_max_int("extraInvocations", self.extra_invocations, 2)?;
        check_max_int("initialOutputTokens", self.initial_output_tokens, MAX_INT32)?;
        check_max_int("extraOutputTokens", self.extra_output_tokens, MAX_INT32)?;
        check_max_int(
            "initialPromptCharacters",
            self.initial_prompt_characters,
            MAX_INT32,
        )?;
        check_max_int(
            "extraPromptCharacters",
            self.extra_prompt_characters,
            MAX_INT32,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StageExecutionSnapshot {
    pub schema_version: SchemaV1,
    pub project_id: Uuid,
    pub pulsation_id: Uuid,
    pub stage_execution_id: Uuid,
    pub stage: Stage,
    pub parent_generation: u32,
    pub parent_state_version: u32,
    pub execution_version: u32,
    pub status: ExecutionStatus,
    pub units: Vec<UnitSnapshotV1>,
    #[serde(deserialize_with = "nullable")]
    pub blocked_reason: Option<String>,
    pub checks: Vec<CheckSummaryV1>,
    pub remaining_budget: RemainingBudgetV1,
    pub usage: UsageSummaryV1,
}

impl Validate for StageExecutionSnapshot {
    fn validate(&self) -> Result<(), CompositionError> {
        for (field, value) in [
            ("parentGeneration", self.parent_generation),
            ("parentStateVersion", self.parent_state_version),
            ("executionVersion", self.execution_version),
        ] {
            check_min_int(field, value, 1)?;
            check_max_int(field, value, MAX_INT32)?;
        }
        check_items("units", &self.units, 0, MAX_UNITS_PER_PLAN)?;
        validate_all(&self.units)?;
        if let Some(reason) = &self.blocked_reason {
            check_text("blockedReason", reason, 1, MAX_TEXT_CHARS)?;
        }
        check_items("checks", &self.checks, 0, 64)?;
        validate_all(&self.checks)?;
        self.remaining_budget.validate()?;
        self.usage.validate()
    }
}

// ---------------------------------------------------------------------------
// Canonical serialization and hashing
// ---------------------------------------------------------------------------

/// Canonical JSON: object keys sorted by Unicode scalar value, no insignificant
/// whitespace, non-ASCII unescaped, standard control escaping, arrays in
/// contract order. Floating-point numbers are a contract violation.
pub fn canonical_json(value: &Value) -> Result<String, CompositionError> {
    let mut out = String::new();
    write_canonical(value, &mut out)?;
    Ok(out)
}

fn write_canonical(value: &Value, out: &mut String) -> Result<(), CompositionError> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
        Value::Number(number) => {
            if number.is_f64() {
                return Err(CompositionError::identity(
                    "canonical JSON forbids floating-point numbers.",
                ));
            }
            out.push_str(&number.to_string());
        }
        Value::String(text) => {
            out.push_str(&serde_json::to_string(text).expect("strings serialize"));
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(item, out)?;
            }
            out.push(']');
        }
        Value::Object(fields) => {
            let mut keys = fields.keys().collect::<Vec<_>>();
            // `String` ordering is byte-wise UTF-8, which equals scalar-value order.
            keys.sort_unstable();
            out.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(key).expect("keys serialize"));
                out.push(':');
                write_canonical(&fields[key], out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

/// SHA-256 of the canonical JSON of `value` with the named top-level digest
/// fields removed first (a contract hashes itself excluding its own digest).
pub fn canonical_sha256<T: Serialize>(
    value: &T,
    excluded_fields: &[&str],
) -> Result<Sha256Hex, CompositionError> {
    let mut json = serde_json::to_value(value)
        .map_err(|error| CompositionError::identity(format!("serialization failed: {error}")))?;
    if let Value::Object(fields) = &mut json {
        for field in excluded_fields {
            fields.remove(*field);
        }
    }
    Ok(Sha256Hex::of(canonical_json(&json)?.as_bytes()))
}

// ---------------------------------------------------------------------------
// Conformance tests against the documented registry
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::fixtures;
    use super::*;

    /// Parse plus schema-constraint validation, the same path a real value
    /// takes. Returns the re-serialized value when accepted.
    fn typed_round_trip(definition: &str, instance: &Value) -> Result<Value, String> {
        macro_rules! go {
            ($ty:ty) => {{
                let parsed: $ty =
                    serde_json::from_value(instance.clone()).map_err(|e| e.to_string())?;
                parsed.validate().map_err(|e| e.to_string())?;
                serde_json::to_value(&parsed).map_err(|e| e.to_string())
            }};
        }
        match definition {
            "UnitModelResponseV1" => go!(UnitModelResponseV1),
            "ReferenceEntryV1" => go!(ReferenceEntryV1),
            "EditOperationV1" => go!(EditOperationV1),
            "RelativePath" => {
                let text = instance.as_str().ok_or("not a string")?;
                // The schema layer alone; the device-name rule is host-only.
                if RelPath::schema_shape_only(text) {
                    Ok(instance.clone())
                } else {
                    Err("shape".into())
                }
            }
            "ValidationReceiptV1" => go!(ValidationReceiptV1),
            "StageRecoveryCommandV1" => go!(StageRecoveryCommandV1),
            "WorkUnitSpecV1" => go!(WorkUnitSpecV1),
            "EvidenceItemV1" => go!(EvidenceItemV1),
            "StageExecutionPlanV1" => go!(StageExecutionPlanV1),
            "WorkUnitResultV1" => go!(WorkUnitResultV1),
            "StageAggregateV1" => go!(StageAggregateV1),
            "ValidationCommandProfileV1" => go!(ValidationCommandProfileV1),
            "StageExecutionSnapshot" => go!(StageExecutionSnapshot),
            "StageBudgetV1" => go!(StageBudgetV1),
            "TestPayloadV1" => go!(TestPayloadV1),
            other => panic!("example uses an unmapped definition `{other}`"),
        }
    }

    #[test]
    fn every_documented_example_is_accepted_or_rejected_exactly_like_the_schema() {
        let mut checked = 0;
        for example in fixtures::examples() {
            let validator = fixtures::validator_for(&example.definition);
            let schema_says = validator.is_valid(&example.instance);
            assert_eq!(
                schema_says, example.schema_valid,
                "registry drift on `{}`",
                example.name
            );
            let typed = typed_round_trip(&example.definition, &example.instance);
            assert_eq!(
                typed.is_ok(),
                example.schema_valid,
                "`{}`: typed contract disagrees with the schema: {:?}",
                example.name,
                typed.as_ref().err()
            );
            if let Ok(round_tripped) = typed {
                assert_eq!(
                    canonical_json(&round_tripped).unwrap(),
                    canonical_json(&example.instance).unwrap(),
                    "`{}` does not round-trip byte-for-byte",
                    example.name
                );
                assert!(
                    validator.is_valid(&round_tripped),
                    "`{}` re-serialization violates the schema",
                    example.name
                );
            }
            checked += 1;
        }
        assert_eq!(checked, 53);
    }

    #[test]
    fn host_only_rules_are_not_claimed_by_the_shape_layer() {
        // These examples pass the schema and the typed shape on purpose; the
        // relational/filesystem rules live in `validate`/`references`/`edits`.
        for example in fixtures::examples() {
            if example.host_rule_violation.is_some() {
                assert!(example.schema_valid, "{}", example.name);
                assert!(
                    typed_round_trip(&example.definition, &example.instance).is_ok(),
                    "{}",
                    example.name
                );
            }
        }
        // The device-name rule is one of them and IS enforced by `RelPath`.
        assert!(RelPath::schema_shape_only("src/CON.rs"));
        assert!(RelPath::new("src/CON.rs").is_err());
        assert!(RelPath::new("src/con.txt").is_err());
        assert!(RelPath::new("src/console.rs").is_ok());
    }

    #[test]
    fn unknown_versions_and_fields_fail_closed() {
        let ok = serde_json::json!({
            "schemaVersion": 1, "outcome": "failed", "code": "scope_insufficient",
            "message": "Scope is too small.", "relatedRefs": []
        });
        assert!(serde_json::from_value::<UnitModelResponseV1>(ok.clone()).is_ok());
        let mut version = ok.clone();
        version["schemaVersion"] = serde_json::json!(2);
        assert!(serde_json::from_value::<UnitModelResponseV1>(version).is_err());
        let mut extra = ok.clone();
        extra["statusUpdate"] = serde_json::json!("advance");
        assert!(serde_json::from_value::<UnitModelResponseV1>(extra).is_err());
        let mut mixed = ok;
        mixed["questions"] = serde_json::json!([]);
        assert!(serde_json::from_value::<UnitModelResponseV1>(mixed).is_err());
        let mut executor_kind = serde_json::json!({
            "schemaVersion": 1, "outcome": "failed", "code": "scope_insufficient",
            "message": "Scope is too small.", "relatedRefs": []
        });
        executor_kind["executorKind"] = serde_json::json!("code_implementer");
        assert!(serde_json::from_value::<UnitModelResponseV1>(executor_kind).is_err());
    }

    #[test]
    fn primitives_reject_what_the_schema_patterns_reject() {
        assert!(Handle::new("a").is_ok());
        assert!(Handle::new(format!("a{}", "b".repeat(63))).is_ok());
        assert!(Handle::new(format!("a{}", "b".repeat(64))).is_err());
        assert!(Handle::new("A").is_err());
        assert!(Handle::new("1a").is_err());
        assert!(Handle::new("a-b").is_err());
        assert!(Handle::new("").is_err());
        assert!(Uuid::new("10000000-0000-4000-8000-000000000001").is_ok());
        assert!(Uuid::new("10000000-0000-4000-8000-00000000000G").is_err());
        assert!(Uuid::new("10000000-0000-4000-8000-00000000000A").is_err());
        assert!(Uuid::new("{10000000-0000-4000-8000-000000000001}").is_err());
        assert!(Sha256Hex::new("0".repeat(64)).is_ok());
        assert!(Sha256Hex::new("A".repeat(64)).is_err());
        assert!(Sha256Hex::new("0".repeat(63)).is_err());
        assert!(Timestamp::new("2026-09-12T12:00:00Z").is_ok());
        assert!(Timestamp::new("2026-09-12 12:00").is_err());
        for path in [
            "../secret",
            "C:/secret",
            "src\\lib.rs",
            "src//lib.rs",
            "src/a /b",
            "src/a./b",
            "/src/a",
            "src/./a",
            "src/a:b",
            "",
        ] {
            assert!(RelPath::new(path).is_err(), "{path}");
        }
        assert!(RelPath::new("src/lib.rs").is_ok());
        assert!(RelPath::new("knowledge/obsidian/features/a-b.md").is_ok());
        assert_eq!(
            RelPath::new("Src/Lib.RS").unwrap().canonical_key(),
            "src/lib.rs"
        );
    }

    #[test]
    fn canonical_json_sorts_keys_keeps_arrays_and_unicode_and_rejects_floats() {
        let value = serde_json::json!({"b": [3, 1, {"z": "é", "a": null}], "a": "ção\n\"x\""});
        assert_eq!(
            canonical_json(&value).unwrap(),
            "{\"a\":\"ção\\n\\\"x\\\"\",\"b\":[3,1,{\"a\":null,\"z\":\"é\"}]}"
        );
        assert!(canonical_json(&serde_json::json!({"n": 1.5})).is_err());
        // Golden: the same bytes hash identically regardless of key order.
        let reordered = serde_json::json!({"a": "ção\n\"x\"", "b": [3, 1, {"a": null, "z": "é"}]});
        assert_eq!(
            canonical_sha256(&value, &[]).unwrap(),
            canonical_sha256(&reordered, &[]).unwrap()
        );
        let with_digest = serde_json::json!({"planSha256": "0".repeat(64), "x": 1});
        assert_eq!(
            canonical_sha256(&with_digest, &["planSha256"]).unwrap(),
            canonical_sha256(&serde_json::json!({"x": 1}), &[]).unwrap()
        );
    }

    #[test]
    fn exact_bytes_define_content_identity() {
        // CRLF, LF and Unicode variants are distinct contents with distinct
        // golden digests; no normalization happens before hashing.
        let lf = Sha256Hex::of(b"fn a() {}\n");
        let crlf = Sha256Hex::of(b"fn a() {}\r\n");
        let nfd = Sha256Hex::of("cafe\u{301}\n".as_bytes());
        let nfc = Sha256Hex::of("caf\u{e9}\n".as_bytes());
        assert_ne!(lf, crlf);
        assert_ne!(nfd, nfc);
        assert_eq!(
            lf.as_str(),
            crate::lower_hex(&Sha256::digest(b"fn a() {}\n"))
        );
        assert_eq!(
            Sha256Hex::of(b"").as_str(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            Sha256Hex::from(ContentSha256::of(b"x")),
            Sha256Hex::of(b"x")
        );
    }

    #[test]
    fn receipt_execution_semantics_reject_fabricated_passes() {
        let example = fixtures::examples()
            .into_iter()
            .find(|example| example.name == "host_rejects_fake_pass")
            .unwrap();
        let receipt: ValidationReceiptV1 = serde_json::from_value(example.instance).unwrap();
        assert!(receipt.validate().is_ok());
        assert!(receipt.check_execution_semantics().is_err());
        let passed = fixtures::examples()
            .into_iter()
            .find(|example| example.name == "passed_receipt_shape")
            .unwrap();
        let receipt: ValidationReceiptV1 = serde_json::from_value(passed.instance).unwrap();
        assert!(receipt.check_execution_semantics().is_ok());
    }
}
