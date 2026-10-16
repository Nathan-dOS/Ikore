//! The derived review brief of a developer checkpoint (ADR-027).
//!
//! A brief is built from the same validated typed payload that produced the
//! checkpoint's Markdown, never from the Markdown itself. It is a view model:
//! nothing here is persisted, and the candidate documents stay the record.
//! Every count, status and judgement rule is decided here by the host; text a
//! model wrote (objective, decisions, requirements, signatures, explanations)
//! is carried as data and labelled by the renderer.
//!
//! Every artifact checkpoint shares one anatomy (decision line, changes since
//! the previous revision, judgement items, host-verified lines) and adds the
//! visual of its stage: Specification (criteria, files, units), Contract
//! (interfaces and the units bound to them), Test (coverage next to executed
//! checks) and Integrator (verdicts, blockers, checks and publications). A
//! stage-failure checkpoint gets [`failure_choices`] instead.
//!
//! When a stage runs again after Request revision, [`compare`] lays the
//! previous candidate's brief next to the new one (part 3), keyed by what
//! stays stable across revisions: task-scoped criterion ids, paths,
//! interface and unit keys, and check handles.

mod contract;
mod diff;
mod failure;
mod integrator;
mod specification;
mod test_stage;
#[cfg(test)]
mod tests;
mod visualizer;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::PublicationState;
use super::composition::recipes::{StagePayloadRecord, specification_of, task_table};
use super::composition::references::ReferenceTable;
use super::composition::{
    CheckStatus, CheckSummaryV1, CompositionError, CoverageKind, GroupIntent, Handle,
    InterfaceIntent, ProposalKind, SpecificationContractV1, Stage, Verdict, VerificationMode,
};
use crate::feature_forge::BatchManifest;
use crate::persistence_service::PulsationStage;

pub(crate) use diff::compare_records;
pub use diff::{BriefChange, ChangeKind, ChangeSubject, RevisionDiff, compare};
pub use failure::{FailureChoice, FailureDecisionKind, failure_choices};
pub use visualizer::{
    CheckpointVisualization, DeliveredItem, DeliveredReference, HEADLINE_MAX_CHARS, NOTE_MAX_CHARS,
    NOTES_MAX, ResolvedNote, ResolvedVisualization, ReviewBriefV1, VisualizationState,
    VisualizerInputV1, VisualizerNoteV1, resolve_visualization, validate_visualization,
    visualizer_input, visualizer_schema, visualizer_system_prompt,
};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReviewBrief {
    pub stage: PulsationStage,
    /// The stage an approval starts; `None` at the Integrator, whose
    /// approval publishes.
    pub next_stage: Option<PulsationStage>,
    /// Agent-written objective: the stage's own at Specification, the
    /// approved specification's afterwards. `None` when no approved
    /// specification payload is readable.
    pub objective: Option<String>,
    /// Key counts for the decision line, in host words.
    pub counts: Vec<String>,
    /// What changed since the previous candidate of this stage, when the
    /// stage ran again after Request revision.
    pub revision: Option<RevisionDiff>,
    /// Points only the developer can settle, in a fixed rule order.
    pub judgement: Vec<JudgementItem>,
    /// Host validations the accepted payload already passed.
    pub verified: Vec<String>,
    pub visual: StageVisual,
}

/// What the stage produced, drawn the way its checkpoint is reviewed.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum StageVisual {
    Specification(SpecificationVisual),
    Contract(ContractVisual),
    Test(TestVisual),
    Integrator(IntegratorVisual),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct BriefFacts {
    pub tasks: usize,
    pub criteria: usize,
    pub executed: usize,
    pub by_reading: usize,
    pub not_applicable: usize,
    pub existing_files: usize,
    pub planned_files: usize,
    pub changed_interfaces: usize,
    pub units: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JudgementKind {
    Decision,
    ReadingOnlyProof,
    NotApplicable,
    InterfaceChange,
    Exclusions,
    /// Contract: an interface that does not exist yet.
    NewInterface,
    /// Test: a criterion covered only by tests that already existed.
    ExistingTestsOnly,
    /// Test and Integrator: a required host check without a passing receipt.
    UnexecutedCheck,
    /// Integrator: the reviewer found the criterion contradicted.
    Contradicted,
    /// Integrator: nothing the reviewer saw supports the criterion.
    Unverified,
    /// Integrator: a blocker the reviewer reported.
    Blocker,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JudgementItem {
    pub kind: JudgementKind,
    /// Short host label: `D1`, a criterion id, a path, a key or a count.
    pub label: String,
    pub text: String,
    pub detail: Option<String>,
    /// Criterion ids and paths the item concerns.
    pub refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SpecificationVisual {
    pub facts: BriefFacts,
    /// Agent-written exclusions, one entry each.
    pub exclusions: Vec<String>,
    pub tasks: Vec<BriefTask>,
    pub files: Vec<BriefFileGroup>,
    /// Interface intents that name no existing file.
    pub unattached_interfaces: Vec<BriefInterface>,
    pub units: Vec<BriefUnit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BriefTask {
    pub task_id: String,
    pub title: String,
    pub criteria: Vec<BriefCriterion>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BriefCriterion {
    pub criterion_id: String,
    /// The developer's own criterion text from the frozen batch.
    pub description: String,
    /// Agent-written requirement for the criterion.
    pub requirement: String,
    pub verification: VerificationMode,
    /// Agent-written plan for proving the criterion.
    pub verification_intent: String,
    pub checks: Vec<String>,
    pub not_applicable_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BriefFileGroup {
    /// `crates/<name>`, a top-level folder, or empty for the project root.
    pub root: String,
    pub files: Vec<BriefFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BriefFile {
    /// Path relative to the group root.
    pub path: String,
    /// Planned (Specification) or created (Test) rather than existing.
    pub planned: bool,
    pub interfaces: Vec<BriefInterface>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BriefInterface {
    pub intent: InterfaceIntent,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BriefUnit {
    pub key: String,
    pub kind: ProposalKind,
    pub destinations: Vec<String>,
    pub criteria: Vec<String>,
    pub depends_on: Vec<String>,
    pub objective: String,
    /// Interface keys the contract binds to the unit (Contract only).
    pub interfaces: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContractVisual {
    pub interfaces: Vec<BriefInterfaceGroup>,
    /// The approved specification's work units, with the dependencies and
    /// interfaces the contract adds. Empty when no approved specification
    /// payload is readable.
    pub units: Vec<BriefUnit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BriefInterfaceGroup {
    pub key: String,
    pub intent: GroupIntent,
    /// Agent-written signature.
    pub signature: String,
    /// Agent-written invariants.
    pub invariants: Vec<String>,
    pub sources: Vec<String>,
    pub criteria: Vec<String>,
    /// Work units the contract binds to the interface.
    pub units: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TestVisual {
    /// One row per criterion, in batch order.
    pub coverage: Vec<BriefCoverage>,
    /// What the host executed, apart from what the agent declared.
    pub checks: Vec<BriefCheck>,
    /// Test files this stage wrote.
    pub files: Vec<BriefFileGroup>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BriefCoverage {
    /// The task the criterion id belongs to; ids are local to their task.
    pub task_id: String,
    pub criterion_id: String,
    pub description: String,
    pub task_title: String,
    pub coverage: CoverageKind,
    /// The authored or existing test files the coverage names.
    pub tests: Vec<String>,
    /// Checks the agent declared; whether they ran is in the checks list.
    pub declared_checks: Vec<String>,
    /// Agent-written.
    pub explanation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BriefCheck {
    pub check_ref: String,
    pub required: bool,
    pub status: CheckStatus,
    /// The host receipt behind the status, when one exists.
    pub receipt: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IntegratorVisual {
    /// One verdict per criterion, in batch order.
    pub verdicts: Vec<BriefVerdict>,
    /// Agent-written.
    pub blockers: Vec<String>,
    pub checks: Vec<BriefCheck>,
    /// Files of this input revision and their publication state.
    pub publications: Vec<BriefPublication>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BriefVerdict {
    /// The task the criterion id belongs to; ids are local to their task.
    pub task_id: String,
    pub criterion_id: String,
    pub description: String,
    pub task_title: String,
    pub verdict: Verdict,
    pub checks: Vec<String>,
    /// Agent-written.
    pub explanation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BriefPublication {
    pub path: String,
    /// The stage that produced the file.
    pub stage: PulsationStage,
    pub state: PublicationState,
}

/// What a brief needs besides the stage's own payload, read by the host in
/// the checkpoint's snapshot.
#[derive(Debug, Clone, Copy)]
pub struct BriefContext<'a> {
    pub manifest: &'a BatchManifest,
    /// The accepted Specification payload of the same input revision, for
    /// the stages after it.
    pub specification: Option<&'a StagePayloadRecord>,
    /// The candidate execution's host checks with their latest receipts.
    pub checks: &'a [CheckSummaryV1],
    /// Files of the input revision with their publication state.
    pub publications: &'a [BriefPublication],
}

/// The brief of an artifact checkpoint, or `None` when the record holds no
/// completed payload of a reviewed stage.
pub fn stage_brief(
    record: &StagePayloadRecord,
    context: &BriefContext<'_>,
) -> Result<Option<ReviewBrief>, CompositionError> {
    let table = task_table(context.manifest)?;
    let approved = context
        .specification
        .and_then(|record| specification_of(record).map(|spec| (spec, record)));
    Ok(match record.stage {
        Stage::Specification => {
            specification_of(record).map(|spec| specification::build(spec, record, &table))
        }
        Stage::Contract => contract::build(record, &table, approved),
        Stage::Test => test_stage::build(record, &table, approved, context.checks),
        Stage::Integrator => integrator::build(
            record,
            &table,
            approved,
            context.checks,
            context.publications,
        ),
        Stage::Intake | Stage::Code => None,
    })
}

/// The `stage_payload_v1` record behind a checkpoint, decoded.
pub(crate) fn decode_record(content: &str) -> Result<StagePayloadRecord, serde_json::Error> {
    serde_json::from_str(content)
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// The approved specification and the record that resolves its handles.
type Approved<'a> = Option<(&'a SpecificationContractV1, &'a StagePayloadRecord)>;

/// A stage as the developer reads it.
fn stage_label(stage: PulsationStage) -> &'static str {
    match stage {
        PulsationStage::Intake => "Intake",
        PulsationStage::Specification => "Specification",
        PulsationStage::Contract => "Contract",
        PulsationStage::Code => "Code",
        PulsationStage::Test => "Test",
        PulsationStage::Integrator => "Integrator",
    }
}

/// A path from its file group's root and its path inside it.
fn full_path(root: &str, path: &str) -> String {
    if root.is_empty() {
        path.to_owned()
    } else {
        format!("{root}/{path}")
    }
}

fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

/// `"3 criteria · 2 authored, 1 existing"`, leaving out zero parts.
fn split_count(total: String, parts: &[(usize, &str)]) -> String {
    let parts: Vec<String> = parts
        .iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, word)| format!("{count} {word}"))
        .collect();
    if parts.is_empty() {
        total
    } else {
        format!("{total} · {}", parts.join(", "))
    }
}

fn criterion_id(table: &ReferenceTable, handle: &Handle) -> String {
    table
        .criterion(handle)
        .map(|binding| binding.criterion_id.clone())
        .unwrap_or_else(|| handle.to_string())
}

fn path_of(record: &StagePayloadRecord, handle: &Handle) -> String {
    record
        .path_of(handle)
        .map(|path| path.as_str().to_owned())
        .unwrap_or_else(|| handle.to_string())
}

/// One criterion of the frozen batch, in batch order.
struct BatchCriterion {
    handle: Handle,
    task_id: String,
    task_title: String,
    criterion_id: String,
    description: String,
}

fn batch_criteria(table: &ReferenceTable) -> Vec<BatchCriterion> {
    table
        .scope()
        .iter()
        .flat_map(|scope| {
            let (task_id, task_title) = table
                .task(&scope.task_ref)
                .map(|task| (task.task_id.clone(), task.title.clone()))
                .unwrap_or_default();
            scope.criterion_refs.iter().filter_map(move |handle| {
                let binding = table.criterion(handle)?;
                Some(BatchCriterion {
                    handle: handle.clone(),
                    task_id: task_id.clone(),
                    task_title: task_title.clone(),
                    criterion_id: binding.criterion_id.clone(),
                    description: binding.description.clone(),
                })
            })
        })
        .collect()
}

/// Files grouped under their crate or first folder.
fn file_groups(files: impl IntoIterator<Item = (String, BriefFile)>) -> Vec<BriefFileGroup> {
    let mut groups: BTreeMap<String, Vec<BriefFile>> = BTreeMap::new();
    for (root, file) in files {
        groups.entry(root).or_default().push(file);
    }
    groups
        .into_iter()
        .map(|(root, files)| BriefFileGroup { root, files })
        .collect()
}

/// `crates/<name>/rest` groups under `crates/<name>`; any other nested path
/// under its first folder; a file at the top level under the project root.
fn split_root(path: &str) -> (String, String) {
    let parts: Vec<&str> = path.split('/').collect();
    match parts.as_slice() {
        ["crates", name, rest @ ..] if !rest.is_empty() => {
            (format!("crates/{name}"), rest.join("/"))
        }
        [first, rest @ ..] if !rest.is_empty() => ((*first).to_owned(), rest.join("/")),
        _ => (String::new(), path.to_owned()),
    }
}

fn check_words(status: CheckStatus) -> &'static str {
    match status {
        CheckStatus::Pending => "pending",
        CheckStatus::Running => "running",
        CheckStatus::Passed => "passed",
        CheckStatus::Failed => "failed",
        CheckStatus::TimedOut => "timed out",
        CheckStatus::Cancelled => "cancelled",
        CheckStatus::Unavailable => "unavailable",
        CheckStatus::NotRun => "not run",
        CheckStatus::Error => "error",
    }
}

fn brief_checks(checks: &[CheckSummaryV1]) -> Vec<BriefCheck> {
    checks
        .iter()
        .map(|check| BriefCheck {
            check_ref: check.check_ref.to_string(),
            required: check.required,
            status: check.status,
            receipt: check.receipt_id.as_ref().map(ToString::to_string),
        })
        .collect()
}

/// Every required check without a passing receipt is the developer's call.
fn unexecuted_checks(checks: &[BriefCheck]) -> Vec<JudgementItem> {
    checks
        .iter()
        .filter(|check| check.required && check.status != CheckStatus::Passed)
        .map(|check| JudgementItem {
            kind: JudgementKind::UnexecutedCheck,
            label: check.check_ref.clone(),
            text: format!(
                "The host has no passing receipt for this required check: {}.",
                check_words(check.status)
            ),
            detail: None,
            refs: Vec::new(),
        })
        .collect()
}

/// `"2 host checks · 1 passed, 1 not run"` or the absence of any.
fn checks_count(checks: &[BriefCheck]) -> String {
    if checks.is_empty() {
        return "No host check for this stage".to_owned();
    }
    let mut by_status: Vec<(CheckStatus, usize)> = Vec::new();
    for check in checks {
        match by_status
            .iter_mut()
            .find(|(status, _)| *status == check.status)
        {
            Some((_, count)) => *count += 1,
            None => by_status.push((check.status, 1)),
        }
    }
    let parts: Vec<String> = by_status
        .iter()
        .map(|(status, count)| format!("{count} {}", check_words(*status)))
        .collect();
    format!(
        "{} · {}",
        plural(checks.len(), "host check", "host checks"),
        parts.join(", ")
    )
}

/// Lines every stage with host checks can state about them.
fn checks_verified(checks: &[BriefCheck], lines: &mut Vec<String>) {
    if !checks.is_empty() {
        lines.push(
            "Check statuses come from host receipts, never from what the agent declared."
                .to_owned(),
        );
    }
}
