//! Just-in-time scope requests (ADR-035 M4). A Code or Test unit that must
//! change a delivered file it does not own fails with `scope_insufficient`
//! and names the file's ref. This decides, from the frozen plan, the mandate
//! in force and the stage's request history, which of those files the
//! developer may grant to that unit, and whether the request is offered at
//! all. Pure: the host gathers the facts, keeps the derivation with the
//! request and derives it again before a grant.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::orchestration::ContentSha256;
use crate::orchestration::composition::review_policy::is_review_policy_path;
use crate::orchestration::composition::{
    ExecutorKind, Handle, ReferenceEntryV1, Risk, SourceRole, Stage, StageExecutionPlanV1,
    canonical_json,
};
use crate::orchestration::publication::has_protected_name;

use super::contract::{ExecutionMandateV1, path_key};

pub const SCOPE_DERIVATION_VERSION: u32 = 1;
/// Accepted paths one request may carry; past it the request is not offered.
pub const MAX_SCOPE_PATHS: usize = 8;
/// Requests offered per logical stage execution (its shared budget account).
pub const MAX_SCOPE_REQUESTS: usize = 2;

/// Why a ref, or the request as a whole, is not granted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeRefusalCode {
    ScopeRefNotDeliveredFile,
    ScopeRoleNotWritable,
    ScopeProtectedPath,
    ScopeStageDocument,
    ScopeForeignCandidate,
    ScopeOwnedByOtherUnit,
    ScopeAlreadyGranted,
    ScopePathLimit,
    ScopeRequestLimit,
    ScopeBudgetInsufficient,
}

impl ScopeRefusalCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ScopeRefNotDeliveredFile => "scope_ref_not_delivered_file",
            Self::ScopeRoleNotWritable => "scope_role_not_writable",
            Self::ScopeProtectedPath => "scope_protected_path",
            Self::ScopeStageDocument => "scope_stage_document",
            Self::ScopeForeignCandidate => "scope_foreign_candidate",
            Self::ScopeOwnedByOtherUnit => "scope_owned_by_other_unit",
            Self::ScopeAlreadyGranted => "scope_already_granted",
            Self::ScopePathLimit => "scope_path_limit",
            Self::ScopeRequestLimit => "scope_request_limit",
            Self::ScopeBudgetInsufficient => "scope_budget_insufficient",
        }
    }

    /// One English sentence for the card and the diagnostic.
    pub const fn describe(self) -> &'static str {
        match self {
            Self::ScopeRefNotDeliveredFile => "not an existing file delivered to the unit",
            Self::ScopeRoleNotWritable => "a kind of file this stage never writes",
            Self::ScopeProtectedPath => "a protected path",
            Self::ScopeStageDocument => "a stage document",
            Self::ScopeForeignCandidate => "produced by another stage or unit",
            Self::ScopeOwnedByOtherUnit => "owned by another unit of the stage",
            Self::ScopeAlreadyGranted => "already in the unit's files",
            Self::ScopePathLimit => "more files than one request may carry",
            Self::ScopeRequestLimit => "the stage already used its scope requests",
            Self::ScopeBudgetInsufficient => "the stage budget has no invocation left for a retry",
        }
    }
}

/// A delivered file the request would grant, with the bytes it was
/// delivered with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopePathV1 {
    #[serde(rename = "ref")]
    pub handle: Handle,
    pub path: String,
    pub role: SourceRole,
    pub sha256: String,
    pub sensitive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopeRefusalV1 {
    #[serde(rename = "ref")]
    pub handle: Handle,
    pub path: Option<String>,
    pub code: ScopeRefusalCode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopeRequestDerivationV1 {
    pub version: u32,
    pub stage_execution_id: String,
    pub stage: Stage,
    pub unit_key: String,
    /// Sorted by the path's Windows identity.
    pub accepted: Vec<ScopePathV1>,
    /// Sorted by ref.
    pub refused: Vec<ScopeRefusalV1>,
    /// Why the request as a whole is not offered, when it is not.
    pub request_refusal: Option<ScopeRefusalCode>,
    pub risk_before: Risk,
    /// The level with the accepted paths added.
    pub risk_after: Risk,
    pub mandate_revision: Option<u32>,
    pub mandate_sha256: Option<String>,
}

impl ScopeRequestDerivationV1 {
    /// Whether the host offers "Grant and retry" for it.
    pub fn grantable(&self) -> bool {
        self.request_refusal.is_none() && !self.accepted.is_empty()
    }

    /// Digest of the canonical JSON; a grant re-derives and compares it.
    pub fn sha256(&self) -> Result<String, String> {
        let value = serde_json::to_value(self).map_err(|error| error.to_string())?;
        let canonical = canonical_json(&value).map_err(|error| error.message)?;
        Ok(ContentSha256::of(canonical.as_bytes()).as_str().to_owned())
    }
}

/// What the host knows when a unit fails with `scope_insufficient`, and
/// again when the developer grants the request.
pub struct ScopeFacts<'a> {
    pub plan: &'a StageExecutionPlanV1,
    pub unit_key: &'a str,
    pub related_refs: &'a [Handle],
    /// Path identities of this pulsation's stage documents.
    pub stage_documents: &'a BTreeSet<String>,
    /// Path identities another stage of this pulsation produced.
    pub foreign_paths: &'a BTreeSet<String>,
    pub mandate: Option<(&'a ExecutionMandateV1, &'a str)>,
    /// Requests already recorded for this logical stage execution.
    pub earlier_requests: usize,
    /// Whether the shared budget could reserve the retry's invocation.
    pub budget_allows: bool,
    pub risk_before: Risk,
    pub is_sensitive: &'a dyn Fn(&str) -> bool,
}

/// The roles a stage may be granted (SC-022): production files for Code,
/// test files for Test.
fn grantable_role(kind: ExecutorKind, role: SourceRole) -> bool {
    match kind {
        ExecutorKind::CodeImplementer | ExecutorKind::CodeRepairer => matches!(
            role,
            SourceRole::Source | SourceRole::Configuration | SourceRole::Document
        ),
        ExecutorKind::TestAuthor | ExecutorKind::TestRepairer => role == SourceRole::Test,
        _ => false,
    }
}

/// `None` when the unit is not in the plan or its stage takes no scope
/// requests (only Code and Test do).
pub fn derive_scope_request(
    facts: &ScopeFacts<'_>,
    risk_after: impl FnOnce(&[ScopePathV1]) -> Risk,
) -> Option<ScopeRequestDerivationV1> {
    if !matches!(facts.plan.stage, Stage::Code | Stage::Test) {
        return None;
    }
    let unit = facts
        .plan
        .units
        .iter()
        .find(|unit| unit.logical_unit_key.as_str() == facts.unit_key)?;
    let entry = |handle: &Handle| {
        facts
            .plan
            .references
            .iter()
            .find(|entry| entry.handle() == handle)
    };
    let path_of = |handle: &Handle| entry(handle).and_then(ReferenceEntryV1::relative_path);
    let owned_by = |unit: &crate::orchestration::composition::WorkUnitSpecV1, key: &str| {
        unit.owned_destination_refs
            .iter()
            .filter_map(path_of)
            .any(|path| path_key(path.as_str()) == key)
    };
    // A granted path is one of the unit's destinations in every later plan
    // (`recipes::prepare`), so ownership alone says it was granted.
    let mut seen = BTreeSet::new();
    let mut accepted = Vec::new();
    let mut refused = Vec::new();
    for handle in facts.related_refs {
        if !seen.insert(handle.clone()) {
            continue;
        }
        let refuse = |code, path: Option<String>| ScopeRefusalV1 {
            handle: handle.clone(),
            path,
            code,
        };
        let delivered =
            unit.input_refs.contains(handle) || unit.owned_destination_refs.contains(handle);
        let (path, sha, role) = match entry(handle) {
            Some(ReferenceEntryV1::ExistingSource {
                relative_path,
                content_sha256,
                source_role,
                ..
            }) if delivered => (relative_path, content_sha256, *source_role),
            Some(ReferenceEntryV1::CandidateSource { relative_path, .. }) => {
                refused.push(refuse(
                    ScopeRefusalCode::ScopeForeignCandidate,
                    Some(relative_path.as_str().to_owned()),
                ));
                continue;
            }
            other => {
                refused.push(refuse(
                    ScopeRefusalCode::ScopeRefNotDeliveredFile,
                    other
                        .and_then(ReferenceEntryV1::relative_path)
                        .map(|path| path.as_str().to_owned()),
                ));
                continue;
            }
        };
        let text = path.as_str().to_owned();
        let key = path_key(&text);
        let code =
            if has_protected_name(&text) || is_review_policy_path(&text) {
                Some(ScopeRefusalCode::ScopeProtectedPath)
            } else if facts.stage_documents.contains(&key) {
                Some(ScopeRefusalCode::ScopeStageDocument)
            } else if facts.foreign_paths.contains(&key) {
                Some(ScopeRefusalCode::ScopeForeignCandidate)
            } else if !grantable_role(unit.executor_kind, role) {
                Some(ScopeRefusalCode::ScopeRoleNotWritable)
            } else if owned_by(unit, &key) {
                Some(ScopeRefusalCode::ScopeAlreadyGranted)
            } else if facts.plan.units.iter().any(|other| {
                other.logical_unit_key != unit.logical_unit_key && owned_by(other, &key)
            }) {
                Some(ScopeRefusalCode::ScopeOwnedByOtherUnit)
            } else {
                None
            };
        match code {
            Some(code) => refused.push(refuse(code, Some(text))),
            None => accepted.push(ScopePathV1 {
                handle: handle.clone(),
                sensitive: (facts.is_sensitive)(&text),
                path: text,
                role,
                sha256: sha.as_str().to_owned(),
            }),
        }
    }
    accepted.sort_by_key(|path| path_key(&path.path));
    refused.sort_by(|a, b| a.handle.cmp(&b.handle));
    let request_refusal = if accepted.is_empty() {
        None
    } else if facts.earlier_requests >= MAX_SCOPE_REQUESTS {
        Some(ScopeRefusalCode::ScopeRequestLimit)
    } else if accepted.len() > MAX_SCOPE_PATHS {
        Some(ScopeRefusalCode::ScopePathLimit)
    } else if !facts.budget_allows {
        Some(ScopeRefusalCode::ScopeBudgetInsufficient)
    } else {
        None
    };
    let risk_after = if accepted.is_empty() {
        facts.risk_before
    } else {
        risk_after(&accepted)
    };
    Some(ScopeRequestDerivationV1 {
        version: SCOPE_DERIVATION_VERSION,
        stage_execution_id: facts.plan.stage_execution_id.as_str().to_owned(),
        stage: facts.plan.stage,
        unit_key: facts.unit_key.to_owned(),
        accepted,
        refused,
        request_refusal,
        risk_before: facts.risk_before,
        risk_after,
        mandate_revision: facts.mandate.map(|(mandate, _)| mandate.revision),
        mandate_sha256: facts.mandate.map(|(_, sha)| sha.to_owned()),
    })
}

/// The command "Grant and retry" sends; `StageRecoveryCommandV1` is unchanged
/// (SC-001).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopeGrantCommandV1 {
    pub schema_version: u32,
    pub project_id: String,
    pub pulsation_id: String,
    pub checkpoint_id: String,
    pub request_id: String,
    pub expected_state_version: u64,
    pub expected_derivation_sha256: String,
}

pub const SCOPE_GRANT_COMMAND_VERSION: u32 = 1;
