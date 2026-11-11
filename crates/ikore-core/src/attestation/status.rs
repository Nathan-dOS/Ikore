//! What the host reports about attestations and its key (ADR-036 M3): plain
//! data the persistence layer fills and the UI and report draw.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttestationKeyState {
    Ready,
    /// No key yet; the first seal creates one.
    NoKey,
    /// The credential store could not be read or written.
    KeyringUnavailable,
    /// A key is registered but its stored secret is gone.
    KeyMissing,
    /// The stored secret is not the registered key.
    Mismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttestationKeyRecord {
    pub key_id: String,
    /// `active` or `retired`.
    pub state: String,
    pub created_at: String,
    pub retired_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttestationKeyStatus {
    pub state: AttestationKeyState,
    /// Newest first; the active key, if any, leads.
    pub keys: Vec<AttestationKeyRecord>,
    pub sealing_enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttestationState {
    /// The pulsation has not ended, or ended before attestations existed.
    NotSealed,
    Pending,
    Skipped,
    Failed,
    Signed,
    Unsigned,
    TooLarge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignatureCheck {
    Valid,
    Invalid,
    Unsigned,
    UnknownKey,
    TooLarge,
}

impl SignatureCheck {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::Invalid => "invalid",
            Self::Unsigned => "unsigned",
            Self::UnknownKey => "unknown_key",
            Self::TooLarge => "too_large",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RederivationCheck {
    Matches,
    Differs,
    Unavailable,
}

impl RederivationCheck {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Matches => "matches",
            Self::Differs => "differs",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceCheck {
    Unchanged,
    Modified,
    Missing,
    Unreadable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubjectCheck {
    pub path: String,
    pub state: WorkspaceCheck,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttestationVerification {
    pub checked_at: String,
    pub signature: SignatureCheck,
    pub rederivation: RederivationCheck,
    /// The first part that differs when `rederivation` is `differs`.
    pub difference: Option<String>,
    pub workspace: Vec<SubjectCheck>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttestationStatus {
    pub state: AttestationState,
    pub key_id: Option<String>,
    pub statement_sha256: Option<String>,
    pub subject_count: usize,
    pub outcome: Option<String>,
    pub sealed_at: Option<String>,
    pub signed_at: Option<String>,
    /// Why sealing failed.
    pub diagnostic: Option<String>,
    /// The last check, kept for display only.
    pub verification: Option<AttestationVerification>,
}

/// What "Save attestation…" writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttestationExport {
    pub bundle_file_name: String,
    /// One DSSE envelope per line.
    pub bundle: String,
    pub key_file_name: String,
    pub public_key_pem: String,
    /// What the files contain, for the confirmation.
    pub contents: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKey {
    /// The signing key is one of this machine's attestation keys.
    KnownOnThisMachine,
    /// Signed by the key in the accompanying PEM, which this machine does not
    /// know.
    UnknownKey,
    /// No key to check the signature with.
    Unavailable,
}

/// The check of an attestation file chosen with "Verify a file…".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttestationFileCheck {
    pub signature: SignatureCheck,
    pub key: FileKey,
    pub key_id: Option<String>,
    pub pulsation_id: Option<String>,
    pub statement_sha256: Option<String>,
    pub subject_count: usize,
}

/// The trailer a commit of a pulsation's changes can carry (feature 08 reads
/// it): the pulsation and the digest of its statement.
pub fn commit_trailer(pulsation_id: &str, statement_sha256: &str) -> String {
    format!("HVC-Pulsation: {pulsation_id}\nHVC-Attestation: sha256:{statement_sha256}")
}
