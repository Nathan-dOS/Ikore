//! The Integrator review boundary (ADR-032, M1).
//!
//! The reviewer's inputs are classified before they reach a prompt: approved
//! intent, candidate files with a base → candidate diff, baseline context,
//! author claims, host evidence and the read-only review policy. Stage
//! documents (the Specification note, the Test report) are not delivered: the
//! normative content already arrives typed, and the Test report mixes the
//! author's narrative with the host's check table.
//!
//! What the host could not establish becomes an explicit evidence gap. A gap
//! never promotes a verdict; a gap that invalidates evidence downgrades a
//! `supported` assessment to `unverified` (`normalize_for_gaps`). The canonical
//! manifest records origin, revision and class of every input without
//! execution identifiers, so identical inputs yield an identical digest.
//! Everything here is pure: the host gathers the facts.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use super::line_diff::{self, LineDiff};
use super::recipes::StagePayloadRecord;
use super::review_policy::{self, PolicyFileState, ResolvedPolicy};
use super::{
    AnchorBindingV1, CompletedPayloadV1, CoverageKind, EditOperationV1, Handle, Stage,
    UnitModelResponseV1, Verdict,
};

pub const MANIFEST_SCHEMA_VERSION: u32 = 1;
pub const TOOL_SCOPE_HOST_CONTEXT_ONLY: &str = "host_context_only";
pub const MAX_DIFF_CHARS_PER_FILE: usize = 8_000;
pub const MAX_DIFF_CHARS_TOTAL: usize = 32_000;
pub const MAX_CLAIM_CHARS: usize = 400;
pub const MAX_CLAIMS_CHARS_TOTAL: usize = 8_000;

/// Where the host found a source of the stage.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceOrigin {
    #[default]
    Brief,
    Task,
    Clarification,
    /// An accepted artifact of an earlier stage, read back from the workspace.
    StageArtifact {
        stage: Stage,
        #[serde(rename = "artifactKind")]
        artifact_kind: String,
    },
}

/// Artifact kinds that are stage documents rather than reviewed files.
pub const STAGE_DOCUMENT_KINDS: &[&str] = &[
    "specification",
    "validation_report",
    "intake_note",
    "contract",
    "integration_report",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceClass {
    BaselineContext,
    Candidate,
    StageDocument,
}

impl SourceOrigin {
    /// A stage artifact supersedes a workspace read of the same path: the
    /// file is what this pulsation published, not baseline context.
    pub fn prefers_over(&self, other: &Self) -> bool {
        matches!(self, Self::StageArtifact { .. }) && !matches!(other, Self::StageArtifact { .. })
    }

    pub fn class(&self) -> EvidenceClass {
        match self {
            Self::StageArtifact { artifact_kind, .. }
                if STAGE_DOCUMENT_KINDS.contains(&artifact_kind.as_str()) =>
            {
                EvidenceClass::StageDocument
            }
            Self::StageArtifact {
                stage: Stage::Code | Stage::Test,
                ..
            } => EvidenceClass::Candidate,
            Self::StageArtifact { .. } => EvidenceClass::StageDocument,
            _ => EvidenceClass::BaselineContext,
        }
    }
}

// ------------------------------------------------------------------ gaps

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GapCode {
    WorkspaceUnavailable,
    CandidateChangedOutsidePipeline,
    CandidateUnreadable,
    SourceUnreadable,
    CheckNotRun,
    CheckStale,
    CheckUnverifiable,
    BaseUnavailable,
    DiffTruncated,
    DiffUnavailable,
    OrphanedPublication,
    ReviewPolicyInvalid,
    ReviewPolicyOverBudget,
}

impl GapCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WorkspaceUnavailable => "workspace_unavailable",
            Self::CandidateChangedOutsidePipeline => "candidate_changed_outside_pipeline",
            Self::CandidateUnreadable => "candidate_unreadable",
            Self::SourceUnreadable => "source_unreadable",
            Self::CheckNotRun => "check_not_run",
            Self::CheckStale => "check_stale",
            Self::CheckUnverifiable => "check_unverifiable",
            Self::BaseUnavailable => "base_unavailable",
            Self::DiffTruncated => "diff_truncated",
            Self::DiffUnavailable => "diff_unavailable",
            Self::OrphanedPublication => "orphaned_publication",
            Self::ReviewPolicyInvalid => "review_policy_invalid",
            Self::ReviewPolicyOverBudget => "review_policy_over_budget",
        }
    }

    /// Whether the gap invalidates evidence a `supported` verdict could rest
    /// on. A missing diff does not: the complete candidate is delivered.
    pub const fn downgrades(self) -> bool {
        matches!(
            self,
            Self::WorkspaceUnavailable
                | Self::CandidateChangedOutsidePipeline
                | Self::CandidateUnreadable
                | Self::SourceUnreadable
                | Self::CheckNotRun
                | Self::CheckStale
                | Self::CheckUnverifiable
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", content = "items", rename_all = "snake_case")]
pub enum GapScope {
    Global,
    Paths(Vec<String>),
    Checks(Vec<Handle>),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceGap {
    pub code: GapCode,
    /// English, value-free description for the prompt and the checkpoint.
    pub subject: String,
    pub scope: GapScope,
    #[serde(default)]
    pub affected_criteria: Vec<Handle>,
}

impl EvidenceGap {
    /// Whether this gap can change a verdict: an evidence-invalidating code,
    /// and, for a check, at least one criterion mapped to it. A path gap also
    /// limits an assessment that cites the path.
    pub fn limits_verdicts(&self) -> bool {
        self.code.downgrades()
            && match &self.scope {
                GapScope::Checks(_) => !self.affected_criteria.is_empty(),
                GapScope::Global | GapScope::Paths(_) => true,
            }
    }

    pub fn global(code: GapCode, subject: impl Into<String>) -> Self {
        Self {
            code,
            subject: subject.into(),
            scope: GapScope::Global,
            affected_criteria: Vec::new(),
        }
    }

    pub fn path(code: GapCode, path: &str, subject: impl Into<String>) -> Self {
        Self {
            code,
            subject: subject.into(),
            scope: GapScope::Paths(vec![path.to_owned()]),
            affected_criteria: Vec::new(),
        }
    }

    pub fn check(code: GapCode, check: &Handle, subject: impl Into<String>) -> Self {
        Self {
            code,
            subject: subject.into(),
            scope: GapScope::Checks(vec![check.clone()]),
            affected_criteria: Vec::new(),
        }
    }
}

// --------------------------------------------------------- host evidence

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Freshness {
    Fresh,
    Stale,
    Unverifiable,
    /// The check did not pass or never ran: freshness does not apply.
    NotApplicable,
}

impl Freshness {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::Stale => "stale",
            Self::Unverifiable => "unverifiable",
            Self::NotApplicable => "—",
        }
    }
}

/// One check of an accepted Code/Test execution as the host observed it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostCheckRow {
    pub stage: Stage,
    pub check_ref: Handle,
    /// `passed`, `failed`, `not_run`, … (receipt status, or `not_run`).
    pub status: String,
    pub freshness: Freshness,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Manifest and UI only; never rendered into the prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_id: Option<String>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub diagnostics: usize,
}

/// The structured result of the Integrator pre-checks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostEvidence {
    pub rows: Vec<HostCheckRow>,
    pub gaps: Vec<EvidenceGap>,
    /// Deterministic findings kept for the aggregate notes and the brief.
    pub findings: Vec<String>,
}

// ------------------------------------------------------------ candidates

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BaseState {
    Absent,
    Present,
    PresentUnstored,
    /// No base was captured for this path (published before ADR-032).
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Binding {
    Fresh,
    ChangedOutsidePipeline,
    Unreadable,
}

impl Binding {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::ChangedOutsidePipeline => "changed_outside_pipeline",
            Self::Unreadable => "unreadable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffState {
    Included,
    Truncated,
    Created,
    Unchanged,
    TooLarge,
    Unavailable,
    OverBudget,
}

/// The base the host captured for a candidate path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseFact {
    pub state: BaseState,
    pub sha256: Option<String>,
    pub reason: Option<String>,
    pub content: Option<String>,
}

impl BaseFact {
    pub fn unknown() -> Self {
        Self {
            state: BaseState::Unknown,
            sha256: None,
            reason: None,
            content: None,
        }
    }
}

/// A candidate file as the host observed it before review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateFact {
    pub path: String,
    /// Bytes delivered to the reviewer (live, or the stored artifact when the
    /// live file is unreadable).
    pub content: String,
    /// Hash the pipeline published.
    pub published_sha256: String,
    pub live_sha256: Option<String>,
    pub base: BaseFact,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateFile {
    pub path: String,
    pub base: BaseState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_sha256: Option<String>,
    pub published_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_sha256: Option<String>,
    pub binding: Binding,
    pub diff: DiffState,
}

// ---------------------------------------------------------------- claims

/// An explanation Code or Test wrote about its own work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorClaim {
    pub stage: Stage,
    pub criteria: Vec<Handle>,
    /// `authored`/`existing`/`not_applicable` for Test coverage, `evidence`
    /// for a Code evidence item.
    pub kind: &'static str,
    pub paths: Vec<String>,
    pub explanation: String,
}

/// Criterion → paths and criterion → checks, from the accepted payloads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CriterionMap {
    pub paths: BTreeMap<Handle, BTreeSet<String>>,
    pub checks: BTreeMap<Handle, BTreeSet<Handle>>,
}

fn clip(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out = text.chars().take(max.saturating_sub(1)).collect::<String>();
    out.push('…');
    out
}

/// Path of a handle in a stage's own reference table, resolving anchors
/// through that stage's frozen plan.
fn resolve_path(
    record: &StagePayloadRecord,
    anchors: &[AnchorBindingV1],
    handle: &Handle,
) -> Option<String> {
    record
        .path_of(handle)
        .map(|path| path.as_str().to_owned())
        .or_else(|| {
            anchors
                .iter()
                .find(|anchor| &anchor.anchor_ref == handle)
                .and_then(|anchor| record.path_of(&anchor.source_ref))
                .map(|path| path.as_str().to_owned())
        })
}

fn edit_target(edit: &EditOperationV1) -> &Handle {
    match edit {
        EditOperationV1::CreateFile {
            destination_ref, ..
        } => destination_ref,
        EditOperationV1::ReplaceFile { source_ref, .. } => source_ref,
        EditOperationV1::ReplaceAnchor { anchor_ref, .. } => anchor_ref,
    }
}

/// Author claims and the criterion map from the accepted Specification,
/// Code and Test payloads. `anchors` are each stage's frozen plan anchors.
pub fn author_claims(
    prior: &BTreeMap<Stage, StagePayloadRecord>,
    anchors: &BTreeMap<Stage, Vec<AnchorBindingV1>>,
) -> (Vec<AuthorClaim>, CriterionMap) {
    let mut claims = Vec::new();
    let mut map = CriterionMap::default();
    if let Some(record) = prior.get(&Stage::Specification)
        && let Some(spec) = super::recipes::specification_of(record)
    {
        for requirement in &spec.requirements {
            map.checks
                .entry(requirement.criterion_ref.clone())
                .or_default()
                .extend(requirement.required_check_refs.iter().cloned());
        }
    }
    let empty = Vec::new();
    for stage in [Stage::Code, Stage::Test] {
        let Some(record) = prior.get(&stage) else {
            continue;
        };
        let anchors = anchors.get(&stage).unwrap_or(&empty);
        for response in record.responses() {
            let UnitModelResponseV1::Completed { payload, .. } = response else {
                continue;
            };
            let (edits, evidence, coverage) = match payload {
                CompletedPayloadV1::Edits(set) => (&set.edits, &set.evidence, None),
                CompletedPayloadV1::Tests(tests) => {
                    (&tests.edits, &tests.evidence, Some(&tests.coverage))
                }
                _ => continue,
            };
            let artifact_path = |key: &Handle| -> Option<String> {
                edits
                    .iter()
                    .find(|edit| &edit.artifact_key == key)
                    .and_then(|edit| resolve_path(record, anchors, edit_target(&edit.edit)))
            };
            if let Some(coverage) = coverage {
                for item in coverage {
                    let mut paths = item
                        .test_artifact_keys
                        .iter()
                        .filter_map(artifact_path)
                        .collect::<BTreeSet<_>>();
                    paths.extend(
                        item.test_input_refs
                            .iter()
                            .filter_map(|handle| resolve_path(record, anchors, handle)),
                    );
                    map.paths
                        .entry(item.criterion_ref.clone())
                        .or_default()
                        .extend(paths.iter().cloned());
                    map.checks
                        .entry(item.criterion_ref.clone())
                        .or_default()
                        .extend(item.check_refs.iter().cloned());
                    claims.push(AuthorClaim {
                        stage,
                        criteria: vec![item.criterion_ref.clone()],
                        kind: match item.coverage {
                            CoverageKind::Authored => "authored",
                            CoverageKind::Existing => "existing",
                            CoverageKind::NotApplicable => "not_applicable",
                        },
                        paths: paths.into_iter().collect(),
                        explanation: clip(&item.explanation, MAX_CLAIM_CHARS),
                    });
                }
            }
            for item in evidence {
                let mut paths = item
                    .artifact_keys
                    .iter()
                    .filter_map(artifact_path)
                    .collect::<BTreeSet<_>>();
                paths.extend(
                    item.input_refs
                        .iter()
                        .filter_map(|handle| resolve_path(record, anchors, handle)),
                );
                for criterion in &item.criterion_refs {
                    map.paths
                        .entry(criterion.clone())
                        .or_default()
                        .extend(paths.iter().cloned());
                }
                // Test evidence repeats its coverage; only Code's is a claim.
                if stage == Stage::Code {
                    claims.push(AuthorClaim {
                        stage,
                        criteria: item.criterion_refs.clone(),
                        kind: "evidence",
                        paths: paths.into_iter().collect(),
                        explanation: clip(&item.explanation, MAX_CLAIM_CHARS),
                    });
                }
            }
        }
    }
    (claims, map)
}

// -------------------------------------------------------------- manifest

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestSource {
    pub path: String,
    pub class: EvidenceClass,
    pub origin: SourceOrigin,
    pub sha256: String,
    pub delivered: bool,
    pub owned: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestLimits {
    pub diff_chars_per_file: usize,
    pub diff_chars_total: usize,
    pub claim_chars: usize,
    pub claims_chars_total: usize,
}

/// The persisted, canonical record of the reviewer's inputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewInputManifestV1 {
    pub schema_version: u32,
    pub tool_scope: String,
    pub sanitization_policy: String,
    pub sources: Vec<ManifestSource>,
    pub candidates: Vec<CandidateFile>,
    pub checks: Vec<HostCheckRow>,
    pub policy: ResolvedPolicy,
    pub gaps: Vec<EvidenceGap>,
    pub author_claims: usize,
    pub limits: ManifestLimits,
}

impl ReviewInputManifestV1 {
    pub fn gap_count(&self) -> usize {
        self.gaps.len()
    }

    /// Gaps that downgraded or may have downgraded a `supported` verdict.
    pub fn limiting_gaps(&self) -> impl Iterator<Item = &EvidenceGap> {
        self.gaps.iter().filter(|gap| gap.limits_verdicts())
    }
}

/// What the Integrator checkpoint shows about the reviewer's inputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CheckpointReviewInputs {
    /// The pulsation runs a pipeline before v4: no review policy applies.
    LegacyPipeline,
    Recorded {
        manifest: Box<ReviewInputManifestV1>,
        /// Review policy files whose bytes differ from the reviewed ones now.
        #[serde(rename = "changedPolicyFiles")]
        changed_policy_files: Vec<String>,
    },
}

/// Everything the host gathered for one Integrator preparation.
#[derive(Debug, Clone, Default)]
pub struct ReviewFacts {
    pub sanitization_policy: String,
    pub sources: Vec<ManifestSource>,
    pub candidates: Vec<CandidateFact>,
    /// Paths with a captured base but no accepted artifact any more.
    pub orphaned: Vec<String>,
    pub host: HostEvidence,
    pub policy: Option<ResolvedPolicy>,
    pub claims: Vec<AuthorClaim>,
    pub criteria: CriterionMap,
}

/// Rendered, already-sanitized prompt sections.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewSections {
    pub policy: Option<String>,
    pub host_evidence: String,
    pub gaps: Option<String>,
    pub changes: Option<String>,
    pub author_claims: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ReviewBoundary {
    pub manifest: ReviewInputManifestV1,
    pub manifest_sha256: String,
    pub sections: ReviewSections,
    pub candidate_paths: BTreeSet<String>,
    pub criteria: CriterionMap,
}

fn base_label(base: &BaseFact) -> &'static str {
    match base.state {
        BaseState::Absent => "created",
        BaseState::Present => "modified",
        BaseState::PresentUnstored => "modified (base not stored)",
        BaseState::Unknown => "base unknown",
    }
}

fn fence_for(content: &str) -> &'static str {
    if content.contains("````") {
        "`````"
    } else if content.contains("```") {
        "````"
    } else {
        "```"
    }
}

/// Builds the boundary. `sanitize(path, content)` returns the model-facing
/// view under the frozen policy, or `None` when sanitization fails.
pub fn build(
    facts: ReviewFacts,
    sanitize: &dyn Fn(&str, &str) -> Option<String>,
) -> ReviewBoundary {
    let mut gaps = facts.host.gaps.clone();
    let mut candidates = Vec::new();
    let mut changes = String::new();
    let mut diff_chars = 0usize;
    for fact in &facts.candidates {
        let binding = match &fact.live_sha256 {
            None => Binding::Unreadable,
            Some(live) if live == &fact.published_sha256 => Binding::Fresh,
            Some(_) => Binding::ChangedOutsidePipeline,
        };
        match binding {
            Binding::Fresh => {}
            Binding::ChangedOutsidePipeline => gaps.push(EvidenceGap::path(
                GapCode::CandidateChangedOutsidePipeline,
                &fact.path,
                format!(
                    "`{}` changed after this pulsation published it; checks and earlier stages saw other bytes.",
                    fact.path
                ),
            )),
            Binding::Unreadable => gaps.push(EvidenceGap::path(
                GapCode::CandidateUnreadable,
                &fact.path,
                format!(
                    "`{}` could not be read from the workspace; the published bytes are shown instead.",
                    fact.path
                ),
            )),
        }
        let diff = match (fact.base.state, &fact.base.content) {
            (BaseState::Absent, _) => {
                let _ = writeln!(
                    changes,
                    "### `{}` (created)\n\nNew file; its complete content is under Candidate files.\n",
                    fact.path
                );
                DiffState::Created
            }
            (BaseState::Present, Some(base)) => {
                let views = sanitize(&fact.path, base).zip(sanitize(&fact.path, &fact.content));
                match views {
                    None => {
                        gaps.push(EvidenceGap::path(
                            GapCode::DiffUnavailable,
                            &fact.path,
                            format!("The diff of `{}` could not be sanitized.", fact.path),
                        ));
                        DiffState::Unavailable
                    }
                    Some((base_view, candidate_view)) => {
                        let remaining = MAX_DIFF_CHARS_TOTAL.saturating_sub(diff_chars);
                        if remaining == 0 {
                            gaps.push(EvidenceGap::path(
                                GapCode::DiffTruncated,
                                &fact.path,
                                format!(
                                    "The diff of `{}` exceeds the diff budget; review the complete file.",
                                    fact.path
                                ),
                            ));
                            DiffState::OverBudget
                        } else {
                            match line_diff::unified(
                                &base_view,
                                &candidate_view,
                                MAX_DIFF_CHARS_PER_FILE.min(remaining),
                            ) {
                                LineDiff::Identical => {
                                    let _ = writeln!(
                                        changes,
                                        "### `{}` (unchanged)\n\nThe published bytes equal the base.\n",
                                        fact.path
                                    );
                                    DiffState::Unchanged
                                }
                                LineDiff::TooLarge => {
                                    gaps.push(EvidenceGap::path(
                                        GapCode::DiffUnavailable,
                                        &fact.path,
                                        format!(
                                            "The change to `{}` is too large for a line diff; review the complete file.",
                                            fact.path
                                        ),
                                    ));
                                    DiffState::TooLarge
                                }
                                LineDiff::Unified { text, truncated } => {
                                    diff_chars += text.chars().count();
                                    let fence = fence_for(&text);
                                    let _ = writeln!(
                                        changes,
                                        "### `{}` (modified)\n\n{fence}diff\n{text}{fence}\n",
                                        fact.path
                                    );
                                    if truncated {
                                        gaps.push(EvidenceGap::path(
                                            GapCode::DiffTruncated,
                                            &fact.path,
                                            format!(
                                                "The diff of `{}` was cut at the budget; review the complete file.",
                                                fact.path
                                            ),
                                        ));
                                        DiffState::Truncated
                                    } else {
                                        DiffState::Included
                                    }
                                }
                            }
                        }
                    }
                }
            }
            _ => {
                let reason = fact.base.reason.as_deref().unwrap_or("not captured");
                gaps.push(EvidenceGap::path(
                    GapCode::BaseUnavailable,
                    &fact.path,
                    format!(
                        "The base of `{}` is unavailable ({reason}); only the final file is shown.",
                        fact.path
                    ),
                ));
                let _ = writeln!(
                    changes,
                    "### `{}` ({})\n\nNo diff: the base is unavailable ({reason}).\n",
                    fact.path,
                    base_label(&fact.base)
                );
                DiffState::Unavailable
            }
        };
        candidates.push(CandidateFile {
            path: fact.path.clone(),
            base: fact.base.state,
            base_reason: fact.base.reason.clone(),
            base_sha256: fact.base.sha256.clone(),
            published_sha256: fact.published_sha256.clone(),
            live_sha256: fact.live_sha256.clone(),
            binding,
            diff,
        });
    }
    for path in &facts.orphaned {
        gaps.push(EvidenceGap::path(
            GapCode::OrphanedPublication,
            path,
            format!(
                "`{path}` was published by an earlier round of this pulsation but no accepted artifact owns it now."
            ),
        ));
    }
    let policy = facts.policy.clone().unwrap_or(ResolvedPolicy {
        state: review_policy::PolicyState::Absent,
        files: Vec::new(),
    });
    for file in &policy.files {
        match file.state {
            PolicyFileState::Loaded => {}
            PolicyFileState::OverBudget => gaps.push(EvidenceGap::path(
                GapCode::ReviewPolicyOverBudget,
                &file.path,
                format!(
                    "`{}` exceeds the review policy budget and was not delivered.",
                    file.path
                ),
            )),
            PolicyFileState::Invalid => gaps.push(EvidenceGap::path(
                GapCode::ReviewPolicyInvalid,
                &file.path,
                format!(
                    "`{}` could not be read safely and was not delivered.",
                    file.path
                ),
            )),
        }
    }
    // Affected criteria: every criterion mapped to a gapped path or check.
    for gap in &mut gaps {
        let mut affected = BTreeSet::new();
        match &gap.scope {
            GapScope::Global => {}
            GapScope::Paths(paths) => {
                for (criterion, mapped) in &facts.criteria.paths {
                    if paths.iter().any(|path| mapped.contains(path)) {
                        affected.insert(criterion.clone());
                    }
                }
            }
            GapScope::Checks(checks) => {
                for (criterion, mapped) in &facts.criteria.checks {
                    if checks.iter().any(|check| mapped.contains(check)) {
                        affected.insert(criterion.clone());
                    }
                }
            }
        }
        gap.affected_criteria = affected.into_iter().collect();
    }
    gaps.sort();
    gaps.dedup();

    let sections = ReviewSections {
        policy: render_policy(&policy, sanitize),
        host_evidence: render_host_evidence(&facts.host),
        gaps: render_gaps(&gaps),
        changes: (!changes.is_empty()).then(|| {
            format!(
                "Base is each file before this pulsation first published it; candidate is the file as published now. Redacted values are masked on both sides.\n\n{changes}"
            )
        }),
        author_claims: render_claims(&facts.claims),
    };
    let mut sources = facts.sources.clone();
    sources.sort_by(|a, b| a.path.cmp(&b.path));
    let manifest = ReviewInputManifestV1 {
        schema_version: MANIFEST_SCHEMA_VERSION,
        tool_scope: TOOL_SCOPE_HOST_CONTEXT_ONLY.to_owned(),
        sanitization_policy: facts.sanitization_policy.clone(),
        sources,
        candidates,
        checks: facts.host.rows.clone(),
        policy,
        gaps,
        author_claims: facts.claims.len(),
        limits: ManifestLimits {
            diff_chars_per_file: MAX_DIFF_CHARS_PER_FILE,
            diff_chars_total: MAX_DIFF_CHARS_TOTAL,
            claim_chars: MAX_CLAIM_CHARS,
            claims_chars_total: MAX_CLAIMS_CHARS_TOTAL,
        },
    };
    let manifest_sha256 = super::canonical_sha256(&manifest, &[])
        .map(|sha| sha.as_str().to_owned())
        .unwrap_or_else(|_| super::Sha256Hex::of(b"unhashable").as_str().to_owned());
    ReviewBoundary {
        candidate_paths: facts
            .candidates
            .iter()
            .map(|fact| fact.path.clone())
            .collect(),
        manifest,
        manifest_sha256,
        sections,
        criteria: facts.criteria,
    }
}

fn render_policy(
    policy: &ResolvedPolicy,
    sanitize: &dyn Fn(&str, &str) -> Option<String>,
) -> Option<String> {
    let delivered = policy
        .loaded()
        .filter_map(|file| {
            let view = sanitize(&file.path, file.content.as_deref()?)?;
            Some((file.path.as_str(), view))
        })
        .collect::<Vec<_>>();
    if delivered.is_empty() {
        return None;
    }
    let mut text = format!("{}\n\n", review_policy::PRECEDENCE);
    for (path, view) in delivered {
        let fence = fence_for(&view);
        let body = if view.ends_with('\n') {
            view
        } else {
            format!("{view}\n")
        };
        let _ = writeln!(text, "### `{path}`\n\n{fence}markdown\n{body}{fence}\n");
    }
    Some(text)
}

fn render_host_evidence(host: &HostEvidence) -> String {
    let mut text = String::from(
        "The host ran and verified these deterministically. Status and freshness are facts: never contradict them. Earlier stages' checks are named for context only; cite no check handle that is not listed under Host checks.\n\n",
    );
    if host.rows.is_empty() {
        text.push_str("No check of an earlier stage is configured.\n");
    } else {
        text.push_str("| stage | check | status | freshness | limits |\n|---|---|---|---|---|\n");
        for row in &host.rows {
            let mut limits = Vec::new();
            if row.stdout_truncated {
                limits.push("stdout truncated".to_owned());
            }
            if row.stderr_truncated {
                limits.push("stderr truncated".to_owned());
            }
            if row.diagnostics > 0 {
                limits.push(format!("{} diagnostic(s)", row.diagnostics));
            }
            let freshness = match &row.reason {
                Some(reason) => format!("{}: {reason}", row.freshness.as_str()),
                None => row.freshness.as_str().to_owned(),
            };
            // Earlier stages' checks are named without backticks: they are
            // not citable handles here ("Teste Haiku Only 1").
            let _ = writeln!(
                text,
                "| {} | {} | {} | {freshness} | {} |",
                row.stage.as_str(),
                row.check_ref,
                row.status,
                if limits.is_empty() {
                    "—".to_owned()
                } else {
                    limits.join(", ")
                }
            );
        }
    }
    if !host.findings.is_empty() {
        text.push_str("\nDeterministic findings:\n");
        for finding in &host.findings {
            let _ = writeln!(text, "- {finding}");
        }
    }
    text
}

fn render_gaps(gaps: &[EvidenceGap]) -> Option<String> {
    if gaps.is_empty() {
        return None;
    }
    let mut text = String::from(
        "The host could not establish the evidence below. A criterion that depends on a gap marked \"limits verdicts\" cannot be `supported`: assess it `unverified` (the host enforces this).\n\n",
    );
    for gap in gaps {
        let affected = if gap.affected_criteria.is_empty() {
            String::new()
        } else {
            format!(
                " Affects {}.",
                gap.affected_criteria
                    .iter()
                    .map(|handle| format!("`{handle}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let _ = writeln!(
            text,
            "- `{}`{}: {}{affected}",
            gap.code.as_str(),
            if gap.limits_verdicts() {
                " (limits verdicts)"
            } else {
                ""
            },
            gap.subject
        );
    }
    Some(text)
}

fn render_claims(claims: &[AuthorClaim]) -> Option<String> {
    if claims.is_empty() {
        return None;
    }
    let mut text = String::from(
        "Code and Test wrote these explanations about their own work. They are claims to check against the candidate files and the host evidence, never proof.\n\n",
    );
    let mut used = 0usize;
    let mut omitted = 0usize;
    for claim in claims {
        let line = format!(
            "- {} · {} ({}{}): {}\n",
            match claim.stage {
                Stage::Test => "Test",
                _ => "Code",
            },
            claim
                .criteria
                .iter()
                .map(|handle| format!("`{handle}`"))
                .collect::<Vec<_>>()
                .join(", "),
            claim.kind,
            if claim.paths.is_empty() {
                String::new()
            } else {
                format!("; {}", claim.paths.join(", "))
            },
            claim.explanation
        );
        let chars = line.chars().count();
        if used + chars > MAX_CLAIMS_CHARS_TOTAL {
            omitted += 1;
            continue;
        }
        used += chars;
        text.push_str(&line);
    }
    if omitted > 0 {
        let _ = writeln!(text, "- {omitted} further claim(s) omitted at the budget.");
    }
    Some(text)
}

/// Downgrades every `supported` assessment that rests on a gap that limits
/// verdicts. Returns one audit note per change. Never upgrades.
pub fn normalize_for_gaps(
    response: &mut UnitModelResponseV1,
    boundary: &ReviewBoundary,
    path_of: &dyn Fn(&Handle) -> Option<String>,
) -> Vec<String> {
    let mut notes = Vec::new();
    let UnitModelResponseV1::Completed {
        payload: CompletedPayloadV1::Integration(integration),
        ..
    } = response
    else {
        return notes;
    };
    let limiting = boundary
        .manifest
        .gaps
        .iter()
        .filter(|gap| gap.limits_verdicts())
        .collect::<Vec<_>>();
    let global = limiting
        .iter()
        .find(|gap| gap.scope == GapScope::Global)
        .map(|gap| gap.code);
    let gapped_paths = limiting
        .iter()
        .filter_map(|gap| match &gap.scope {
            GapScope::Paths(paths) => Some(paths.iter().map(move |path| (path, gap.code))),
            _ => None,
        })
        .flatten()
        .collect::<BTreeMap<_, _>>();
    for assessment in &mut integration.assessments {
        if assessment.verdict != Verdict::Supported {
            continue;
        }
        let reason = global
            .or_else(|| {
                limiting
                    .iter()
                    .find(|gap| gap.affected_criteria.contains(&assessment.criterion_ref))
                    .map(|gap| gap.code)
            })
            .or_else(|| {
                assessment
                    .input_refs
                    .iter()
                    .filter_map(path_of)
                    .find_map(|path| gapped_paths.get(&path).copied())
            });
        if let Some(code) = reason {
            assessment.verdict = Verdict::Unverified;
            notes.push(format!(
                "host downgraded `{}` to unverified: evidence gap `{}`",
                assessment.criterion_ref,
                code.as_str()
            ));
        }
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::composition::{
        CriterionAssessmentV1, ExecutorKind, IntegrationPayloadV1,
    };

    fn h(text: &str) -> Handle {
        Handle::new(text).unwrap()
    }

    fn sanitize(_: &str, content: &str) -> Option<String> {
        Some(content.replace("sk-secret", "<REDACTED_SECRET:API_KEY_1>"))
    }

    fn candidate(
        path: &str,
        base: Option<&str>,
        content: &str,
        live: Option<&str>,
    ) -> CandidateFact {
        let published = super::super::Sha256Hex::of(content.as_bytes())
            .as_str()
            .to_owned();
        CandidateFact {
            path: path.into(),
            content: content.into(),
            published_sha256: published.clone(),
            live_sha256: live.map(|live| {
                if live == content {
                    published.clone()
                } else {
                    super::super::Sha256Hex::of(live.as_bytes())
                        .as_str()
                        .to_owned()
                }
            }),
            base: match base {
                None => BaseFact {
                    state: BaseState::Absent,
                    sha256: None,
                    reason: None,
                    content: None,
                },
                Some(base) => BaseFact {
                    state: BaseState::Present,
                    sha256: Some(
                        super::super::Sha256Hex::of(base.as_bytes())
                            .as_str()
                            .to_owned(),
                    ),
                    reason: None,
                    content: Some(base.into()),
                },
            },
        }
    }

    fn response(verdicts: &[(&str, Verdict, &[&str])]) -> UnitModelResponseV1 {
        UnitModelResponseV1::Completed {
            executor_kind: ExecutorKind::IntegrationReviewer,
            payload: CompletedPayloadV1::Integration(IntegrationPayloadV1 {
                assessments: verdicts
                    .iter()
                    .map(|(criterion, verdict, inputs)| CriterionAssessmentV1 {
                        task_ref: h("task_1"),
                        criterion_ref: h(criterion),
                        verdict: *verdict,
                        input_refs: inputs.iter().map(|input| h(input)).collect(),
                        check_refs: Vec::new(),
                        explanation: "The candidate implements the requirement.".into(),
                    })
                    .collect(),
                blockers: Vec::new(),
                documentation_edits: Vec::new(),
                used_refs: Vec::new(),
            }),
        }
    }

    fn verdicts(response: &UnitModelResponseV1) -> Vec<Verdict> {
        match response {
            UnitModelResponseV1::Completed {
                payload: CompletedPayloadV1::Integration(integration),
                ..
            } => integration.assessments.iter().map(|a| a.verdict).collect(),
            _ => Vec::new(),
        }
    }

    #[test]
    fn stage_documents_and_candidates_are_classified_by_origin() {
        let artifact = |stage, kind: &str| SourceOrigin::StageArtifact {
            stage,
            artifact_kind: kind.into(),
        };
        assert_eq!(
            artifact(Stage::Specification, "specification").class(),
            EvidenceClass::StageDocument
        );
        assert_eq!(
            artifact(Stage::Test, "validation_report").class(),
            EvidenceClass::StageDocument
        );
        assert_eq!(
            artifact(Stage::Code, "code").class(),
            EvidenceClass::Candidate
        );
        assert_eq!(
            artifact(Stage::Code, "documentation").class(),
            EvidenceClass::Candidate
        );
        assert_eq!(
            artifact(Stage::Test, "test").class(),
            EvidenceClass::Candidate
        );
        assert_eq!(SourceOrigin::Task.class(), EvidenceClass::BaselineContext);
        assert!(artifact(Stage::Code, "code").prefers_over(&SourceOrigin::Brief));
        assert!(!SourceOrigin::Brief.prefers_over(&artifact(Stage::Code, "code")));
    }

    #[test]
    fn a_modified_candidate_gets_a_sanitized_diff_and_a_created_one_none() {
        let facts = ReviewFacts {
            sanitization_policy: "hvc-secret-projection-v1".into(),
            candidates: vec![
                candidate(
                    "src/lib.rs",
                    Some("const KEY: &str = \"sk-secret\";\nfn a() {}\n"),
                    "const KEY: &str = \"sk-secret\";\nfn a() { b() }\n",
                    Some("const KEY: &str = \"sk-secret\";\nfn a() { b() }\n"),
                ),
                candidate("src/new.rs", None, "fn b() {}\n", Some("fn b() {}\n")),
            ],
            ..ReviewFacts::default()
        };
        let boundary = build(facts, &sanitize);
        let changes = boundary.sections.changes.as_deref().unwrap();
        assert!(changes.contains("-fn a() {}\n+fn a() { b() }"));
        assert!(!changes.contains("sk-secret"), "{changes}");
        assert!(changes.contains("`src/new.rs` (created)"));
        assert_eq!(boundary.manifest.candidates[0].diff, DiffState::Included);
        assert_eq!(boundary.manifest.candidates[1].diff, DiffState::Created);
        assert!(boundary.manifest.gaps.is_empty());
    }

    #[test]
    fn a_hand_edit_after_publication_is_a_gap_that_downgrades_its_criteria() {
        let mut criteria = CriterionMap::default();
        criteria
            .paths
            .entry(h("crit_1_1"))
            .or_default()
            .insert("src/lib.rs".into());
        let facts = ReviewFacts {
            candidates: vec![candidate(
                "src/lib.rs",
                Some("a\n"),
                "b\n",
                Some("edited by hand\n"),
            )],
            criteria,
            ..ReviewFacts::default()
        };
        let boundary = build(facts, &sanitize);
        let gap = &boundary.manifest.gaps[0];
        assert_eq!(gap.code, GapCode::CandidateChangedOutsidePipeline);
        assert_eq!(gap.affected_criteria, [h("crit_1_1")]);
        assert_eq!(
            boundary.manifest.candidates[0].binding,
            Binding::ChangedOutsidePipeline
        );
        let mut answer = response(&[
            ("crit_1_1", Verdict::Supported, &[]),
            ("crit_1_2", Verdict::Supported, &[]),
            ("crit_1_3", Verdict::Contradicted, &[]),
        ]);
        let notes = normalize_for_gaps(&mut answer, &boundary, &|_| None);
        assert_eq!(
            verdicts(&answer),
            [
                Verdict::Unverified,
                Verdict::Supported,
                Verdict::Contradicted
            ]
        );
        assert_eq!(notes.len(), 1);
    }

    #[test]
    fn gap_normalization_matrix() {
        let base = |gaps: Vec<EvidenceGap>| {
            let mut boundary = build(ReviewFacts::default(), &sanitize);
            boundary.manifest.gaps = gaps;
            boundary
        };
        // Global gap: every supported verdict.
        let boundary = base(vec![EvidenceGap::global(
            GapCode::WorkspaceUnavailable,
            "no workspace",
        )]);
        let mut answer = response(&[("crit_1_1", Verdict::Supported, &[])]);
        normalize_for_gaps(&mut answer, &boundary, &|_| None);
        assert_eq!(verdicts(&answer), [Verdict::Unverified]);
        // A cited input on a gapped path.
        let boundary = base(vec![EvidenceGap::path(
            GapCode::CandidateUnreadable,
            "src/lib.rs",
            "unreadable",
        )]);
        let mut answer = response(&[
            ("crit_1_1", Verdict::Supported, &["src_1"]),
            ("crit_1_2", Verdict::Supported, &["src_2"]),
        ]);
        normalize_for_gaps(&mut answer, &boundary, &|handle| {
            (handle.as_str() == "src_1").then(|| "src/lib.rs".to_owned())
        });
        assert_eq!(verdicts(&answer), [Verdict::Unverified, Verdict::Supported]);
        // A gap that does not limit verdicts changes nothing.
        let mut gap = EvidenceGap::path(GapCode::DiffTruncated, "src/lib.rs", "cut");
        gap.affected_criteria = vec![h("crit_1_1")];
        let boundary = base(vec![gap]);
        let mut answer = response(&[("crit_1_1", Verdict::Supported, &[])]);
        assert!(normalize_for_gaps(&mut answer, &boundary, &|_| None).is_empty());
        assert_eq!(verdicts(&answer), [Verdict::Supported]);
        // A check gap through the criterion map; an unverified verdict stays.
        let mut gap = EvidenceGap::check(GapCode::CheckStale, &h("cargo_test"), "stale");
        gap.affected_criteria = vec![h("crit_1_1")];
        let boundary = base(vec![gap]);
        let mut answer = response(&[
            ("crit_1_1", Verdict::Supported, &[]),
            ("crit_1_2", Verdict::Unverified, &[]),
        ]);
        normalize_for_gaps(&mut answer, &boundary, &|_| None);
        assert_eq!(
            verdicts(&answer),
            [Verdict::Unverified, Verdict::Unverified]
        );
    }

    /// A check gap that no criterion maps to cannot change a verdict, so it
    /// is never presented as one that does (found in the real app's QA).
    #[test]
    fn only_gaps_that_can_change_a_verdict_say_they_limit_verdicts() {
        let unmapped = EvidenceGap::check(GapCode::CheckStale, &h("cargo_test"), "stale");
        assert!(!unmapped.limits_verdicts());
        let mut mapped = unmapped.clone();
        mapped.affected_criteria = vec![h("crit_1_1")];
        assert!(mapped.limits_verdicts());
        assert!(
            EvidenceGap::path(GapCode::CandidateUnreadable, "src/lib.rs", "gone").limits_verdicts(),
            "a cited path still limits"
        );
        assert!(!EvidenceGap::path(GapCode::DiffTruncated, "src/lib.rs", "cut").limits_verdicts());
        assert!(EvidenceGap::global(GapCode::WorkspaceUnavailable, "none").limits_verdicts());
        let text = render_gaps(std::slice::from_ref(&unmapped)).unwrap();
        assert!(!text.contains("(limits verdicts)"), "{text}");
        let text = render_gaps(&[mapped]).unwrap();
        assert!(text.contains("`check_stale` (limits verdicts)"), "{text}");
    }

    #[test]
    fn identical_facts_have_an_identical_manifest_digest() {
        let facts = || ReviewFacts {
            sanitization_policy: "p".into(),
            candidates: vec![candidate("src/a.rs", Some("a\n"), "b\n", Some("b\n"))],
            ..ReviewFacts::default()
        };
        let first = build(facts(), &sanitize);
        let second = build(facts(), &sanitize);
        assert_eq!(first.manifest_sha256, second.manifest_sha256);
        let mut changed = facts();
        changed.candidates[0].content = "c\n".into();
        changed.candidates[0].published_sha256 =
            super::super::Sha256Hex::of(b"c\n").as_str().to_owned();
        changed.candidates[0].live_sha256 = Some(changed.candidates[0].published_sha256.clone());
        assert_ne!(
            build(changed, &sanitize).manifest_sha256,
            first.manifest_sha256
        );
    }

    #[test]
    fn host_evidence_never_renders_receipt_ids() {
        let host = HostEvidence {
            rows: vec![HostCheckRow {
                stage: Stage::Test,
                check_ref: h("cargo_test"),
                status: "passed".into(),
                freshness: Freshness::Stale,
                reason: Some("its inputs changed after the receipt".into()),
                receipt_id: Some("0b7f8c39-1111-4222-8333-944455556666".into()),
                stdout_truncated: true,
                stderr_truncated: false,
                diagnostics: 2,
            }],
            gaps: Vec::new(),
            findings: vec!["Criterion `crit_1_1` has no Test coverage entry.".into()],
        };
        let text = render_host_evidence(&host);
        assert!(text.contains("| test | cargo_test | passed | stale: its inputs changed"));
        assert!(!text.contains("`cargo_test`"), "not a citable handle");
        assert!(text.contains("stdout truncated, 2 diagnostic(s)"));
        assert!(!text.contains("0b7f8c39"));
        assert!(text.contains("Deterministic findings"));
    }

    #[test]
    fn claims_are_labeled_and_bounded() {
        let claims = (0..100)
            .map(|index| AuthorClaim {
                stage: Stage::Test,
                criteria: vec![h("crit_1_1")],
                kind: "authored",
                paths: vec![format!("tests/t{index}.rs")],
                explanation: "x".repeat(MAX_CLAIM_CHARS),
            })
            .collect::<Vec<_>>();
        let text = render_claims(&claims).unwrap();
        assert!(text.starts_with("Code and Test wrote these explanations"));
        assert!(text.contains("further claim(s) omitted"));
        assert!(text.chars().count() < MAX_CLAIMS_CHARS_TOTAL + 400);
        assert_eq!(clip(&"y".repeat(500), 400).chars().count(), 400);
    }
}
