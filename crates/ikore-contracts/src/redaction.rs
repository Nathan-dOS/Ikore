//! Shared redaction values. Policy approval and proof issuance belong to the host.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Cryptographic sanitization policy version applied to projection and embeddings (ADR-030).
/// Preserved as `hvc-secret-projection-v1` (Recommendation R2) to avoid invalidating existing
/// cached embeddings; scheduled for renaming upon the next structural policy revision.
pub const SANITIZATION_POLICY_VERSION: &str = "hvc-secret-projection-v1";
pub const MAX_SANITIZATION_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_REDACTION_SPANS: usize = 65_536;
pub const OMITTED_CONTENT: &str = "[Content omitted: sanitization could not be completed.]";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RedactionResult {
    pub text: String,
    pub count: u64,
    pub categories: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedactionSpan {
    pub start_byte: usize,
    pub end_byte: usize,
    pub detector_id: String,
    pub detector_version: u32,
    pub category: String,
    pub marker: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedSpan {
    pub start_byte: usize,
    pub end_byte: usize,
    pub exception_id: String,
}
#[derive(Clone, PartialEq, Eq)]
pub struct ContextRedaction {
    pub text: String,
    pub spans: Vec<RedactionSpan>,
    pub applied_exceptions: Vec<String>,
    pub authorized_spans: Vec<AuthorizedSpan>,
}
impl std::fmt::Debug for ContextRedaction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContextRedaction")
            .field("bytes", &self.text.len())
            .field("spans", &self.spans)
            .field("applied_exceptions", &self.applied_exceptions)
            .finish()
    }
}
impl ContextRedaction {
    pub fn result(&self) -> RedactionResult {
        RedactionResult {
            text: self.text.clone(),
            count: self.spans.len() as u64,
            categories: self
                .spans
                .iter()
                .map(|s| s.category.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
        }
    }
}
