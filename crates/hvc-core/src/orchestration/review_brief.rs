//! The derived review brief of a developer checkpoint (ADR-024).
//!
//! A brief is built from the same validated typed payload that produced the
//! checkpoint's Markdown, never from the Markdown itself. It is a view model:
//! nothing here is persisted, and the candidate documents stay the record.
//! Every count, status and judgement rule is decided here by the host; text a
//! model wrote (objective, decisions, requirements, descriptions) is carried
//! as data and labelled by the renderer.
//!
//! When a stage runs again after Request revision, [`compare`] lays the
//! previous candidate's brief next to the new one (part 3), keyed by what
//! stays stable across revisions: criterion ids, paths and unit keys.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::composition::recipes::{StagePayloadRecord, specification_of, task_table};
use super::composition::references::ReferenceTable;
use super::composition::{
    CompositionError, Handle, InterfaceIntent, ProposalKind, SpecificationContractV1,
    VerificationMode,
};
use crate::feature_forge::BatchManifest;
use crate::persistence_service::PulsationStage;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReviewBrief {
    pub stage: PulsationStage,
    /// The stage an approval starts, when there is one.
    pub next_stage: Option<PulsationStage>,
    /// Agent-written objective of the stage output.
    pub objective: String,
    pub facts: BriefFacts,
    /// What changed since the previous candidate of this stage, when the
    /// stage ran again after Request revision.
    pub revision: Option<RevisionDiff>,
    /// Points only the developer can settle, in a fixed rule order.
    pub judgement: Vec<JudgementItem>,
    /// Agent-written exclusions, one entry each.
    pub exclusions: Vec<String>,
    /// Host validations the accepted payload already passed.
    pub verified: Vec<String>,
    pub tasks: Vec<BriefTask>,
    pub files: Vec<BriefFileGroup>,
    /// Interface intents that name no existing file.
    pub unattached_interfaces: Vec<BriefInterface>,
    pub units: Vec<BriefUnit>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JudgementKind {
    Decision,
    ReadingOnlyProof,
    NotApplicable,
    InterfaceChange,
    Exclusions,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JudgementItem {
    pub kind: JudgementKind,
    /// Short host label: `D1`, a criterion id, a path or a count.
    pub label: String,
    pub text: String,
    pub detail: Option<String>,
    /// Criterion ids and paths the item concerns.
    pub refs: Vec<String>,
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
}

/// The brief of a Specification checkpoint, or `None` when the record does
/// not hold a completed Specification payload.
pub fn specification_brief(
    record: &StagePayloadRecord,
    manifest: &BatchManifest,
) -> Result<Option<ReviewBrief>, CompositionError> {
    let Some(spec) = specification_of(record) else {
        return Ok(None);
    };
    let table = task_table(manifest)?;
    Ok(Some(build_specification(spec, record, &table)))
}

fn build_specification(
    spec: &SpecificationContractV1,
    record: &StagePayloadRecord,
    table: &ReferenceTable,
) -> ReviewBrief {
    let criterion_id = |handle: &Handle| {
        table
            .criterion(handle)
            .map(|binding| binding.criterion_id.clone())
            .unwrap_or_else(|| handle.to_string())
    };
    let path = |handle: &Handle| {
        record
            .path_of(handle)
            .map(|path| path.as_str().to_owned())
            .unwrap_or_else(|| handle.to_string())
    };

    let tasks = table
        .scope()
        .iter()
        .filter_map(|scope| {
            let task = table.task(&scope.task_ref)?;
            let criteria = scope
                .criterion_refs
                .iter()
                .filter_map(|handle| {
                    let binding = table.criterion(handle)?;
                    let requirement = spec
                        .requirements
                        .iter()
                        .find(|requirement| &requirement.criterion_ref == handle)?;
                    Some(BriefCriterion {
                        criterion_id: binding.criterion_id.clone(),
                        description: binding.description.clone(),
                        requirement: requirement.requirement.clone(),
                        verification: requirement.verification_mode,
                        verification_intent: requirement.verification_intent.clone(),
                        checks: requirement
                            .required_check_refs
                            .iter()
                            .map(ToString::to_string)
                            .collect(),
                        not_applicable_reason: requirement.not_applicable_reason.clone(),
                    })
                })
                .collect();
            Some(BriefTask {
                task_id: task.task_id.clone(),
                title: task.title.clone(),
                criteria,
            })
        })
        .collect::<Vec<BriefTask>>();

    let count = |mode: VerificationMode| {
        spec.requirements
            .iter()
            .filter(|requirement| requirement.verification_mode == mode)
            .count()
    };
    let changed_interfaces = spec
        .interface_intent
        .iter()
        .filter(|intent| intent.intent == InterfaceIntent::Change)
        .count();
    let facts = BriefFacts {
        tasks: tasks.len(),
        criteria: spec.requirements.len(),
        executed: count(VerificationMode::ExecutedCheck),
        by_reading: count(VerificationMode::DocumentCheck),
        not_applicable: count(VerificationMode::NotApplicable),
        existing_files: spec.existing_file_refs.len(),
        planned_files: spec.planned_file_refs.len(),
        changed_interfaces,
        units: spec.unit_proposals.len(),
    };

    let mut judgement = Vec::new();
    for (index, decision) in spec.decisions.iter().enumerate() {
        judgement.push(JudgementItem {
            kind: JudgementKind::Decision,
            label: format!("D{}", index + 1),
            text: decision.clone(),
            detail: None,
            refs: Vec::new(),
        });
    }
    for requirement in &spec.requirements {
        let (kind, detail) = match requirement.verification_mode {
            VerificationMode::ExecutedCheck => continue,
            VerificationMode::DocumentCheck => (
                JudgementKind::ReadingOnlyProof,
                requirement.verification_intent.clone(),
            ),
            VerificationMode::NotApplicable => (
                JudgementKind::NotApplicable,
                requirement
                    .not_applicable_reason
                    .clone()
                    .unwrap_or_default(),
            ),
        };
        let description = table
            .criterion(&requirement.criterion_ref)
            .map(|binding| binding.description.clone())
            .unwrap_or_else(|| requirement.requirement.clone());
        judgement.push(JudgementItem {
            kind,
            label: criterion_id(&requirement.criterion_ref),
            text: description,
            detail: Some(detail),
            refs: Vec::new(),
        });
    }
    for intent in spec
        .interface_intent
        .iter()
        .filter(|intent| intent.intent == InterfaceIntent::Change)
    {
        let paths: Vec<String> = intent.source_refs.iter().map(path).collect();
        judgement.push(JudgementItem {
            kind: JudgementKind::InterfaceChange,
            label: paths
                .first()
                .cloned()
                .unwrap_or_else(|| "interface".to_owned()),
            text: intent.description.clone(),
            detail: None,
            refs: paths
                .into_iter()
                .chain(intent.criterion_refs.iter().map(criterion_id))
                .collect(),
        });
    }
    if !spec.exclusions.is_empty() {
        judgement.push(JudgementItem {
            kind: JudgementKind::Exclusions,
            label: spec.exclusions.len().to_string(),
            text: spec.exclusions.join(" · "),
            detail: None,
            refs: Vec::new(),
        });
    }

    let (files, unattached_interfaces) = files(spec, record);
    let units = spec
        .unit_proposals
        .iter()
        .map(|proposal| BriefUnit {
            key: proposal.unit_key.to_string(),
            kind: proposal.kind,
            destinations: proposal.destination_refs.iter().map(path).collect(),
            criteria: proposal.criterion_refs.iter().map(criterion_id).collect(),
            depends_on: proposal
                .depends_on
                .iter()
                .map(ToString::to_string)
                .collect(),
            objective: proposal.objective.clone(),
        })
        .collect();

    ReviewBrief {
        stage: PulsationStage::Specification,
        next_stage: Some(PulsationStage::Contract),
        objective: spec.objective.clone(),
        verified: verified(&facts, spec),
        facts,
        revision: None,
        judgement,
        exclusions: spec.exclusions.clone(),
        tasks,
        files,
        unattached_interfaces,
        units,
    }
}

/// What host validation guarantees for an accepted Specification payload
/// (`composition::validate`). Only facts that hold by construction appear.
fn verified(facts: &BriefFacts, spec: &SpecificationContractV1) -> Vec<String> {
    let mut lines = vec![match facts.criteria {
        1 => "The acceptance criterion has exactly one requirement.".to_owned(),
        count => format!("Each of the {count} acceptance criteria has exactly one requirement."),
    }];
    match facts.existing_files {
        0 => {}
        1 => lines.push("The existing file it cites was read from the project.".to_owned()),
        count => lines.push(format!(
            "The {count} existing files it cites were read from the project."
        )),
    }
    match facts.planned_files {
        0 => {}
        1 => lines.push(
            "The new file it plans is a registered destination inside the project.".to_owned(),
        ),
        count => lines.push(format!(
            "The {count} new files it plans are registered destinations inside the project."
        )),
    }
    if spec
        .requirements
        .iter()
        .any(|requirement| !requirement.required_check_refs.is_empty())
    {
        lines.push("Every required check it names is configured for this project.".to_owned());
    }
    if facts.not_applicable > 0 {
        lines.push("Every criterion marked not applicable carries a reason.".to_owned());
    }
    match facts.units {
        0 => {}
        1 => lines.push("Its work unit is a code unit covering every criterion.".to_owned()),
        count => lines.push(format!(
            "Its {count} work units form no dependency cycle, and a code unit covers every criterion."
        )),
    }
    lines
}

fn files(
    spec: &SpecificationContractV1,
    record: &StagePayloadRecord,
) -> (Vec<BriefFileGroup>, Vec<BriefInterface>) {
    let mut groups: BTreeMap<String, Vec<BriefFile>> = BTreeMap::new();
    let mut attached = vec![false; spec.interface_intent.len()];
    let cited = spec
        .existing_file_refs
        .iter()
        .map(|handle| (handle, false))
        .chain(spec.planned_file_refs.iter().map(|handle| (handle, true)));
    for (handle, planned) in cited {
        let Some(path) = record.path_of(handle) else {
            continue;
        };
        let interfaces = spec
            .interface_intent
            .iter()
            .enumerate()
            .filter(|(_, intent)| intent.source_refs.contains(handle))
            .map(|(index, intent)| {
                attached[index] = true;
                BriefInterface {
                    intent: intent.intent,
                    description: intent.description.clone(),
                }
            })
            .collect();
        let (root, relative) = split_root(path.as_str());
        groups.entry(root).or_default().push(BriefFile {
            path: relative,
            planned,
            interfaces,
        });
    }
    let unattached = spec
        .interface_intent
        .iter()
        .zip(attached)
        .filter(|(intent, attached)| !attached && intent.intent != InterfaceIntent::None)
        .map(|(intent, _)| BriefInterface {
            intent: intent.intent,
            description: intent.description.clone(),
        })
        .collect();
    let groups = groups
        .into_iter()
        .map(|(root, files)| BriefFileGroup { root, files })
        .collect();
    (groups, unattached)
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

// ---------------------------------------------------------------------------
// Diff between revisions (part 3)
// ---------------------------------------------------------------------------

/// The previous candidate of the stage next to this one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RevisionDiff {
    /// The revision feedback this run received, verbatim, as the host handed
    /// it to the stage. `None` when no record of it exists.
    pub feedback: Option<String>,
    /// `None` when the previous candidate is not readable; nothing is
    /// inferred then. Empty when nothing structural changed.
    pub changes: Option<Vec<BriefChange>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Removed,
    /// The item stayed; what it belongs to changed.
    Moved,
    Changed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeSubject {
    Objective,
    Decision,
    Criterion,
    Interface,
    Exclusion,
    File,
    Unit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BriefChange {
    pub kind: ChangeKind,
    pub subject: ChangeSubject,
    /// The key compared: a criterion id, a path or a unit key. Decisions
    /// carry no id, so theirs is the label they have in the revision they
    /// appear in. Empty for the objective, exclusions and interfaces tied to
    /// no file.
    pub label: String,
    /// The part of the item that changed, for `Changed` and `Moved`.
    pub field: Option<&'static str>,
    pub before: Option<String>,
    pub after: Option<String>,
}

impl BriefChange {
    fn new(kind: ChangeKind, subject: ChangeSubject, label: impl Into<String>) -> Self {
        Self {
            kind,
            subject,
            label: label.into(),
            field: None,
            before: None,
            after: None,
        }
    }

    fn field(mut self, field: &'static str) -> Self {
        self.field = Some(field);
        self
    }

    fn before(mut self, text: impl Into<String>) -> Self {
        self.before = Some(text.into());
        self
    }

    fn after(mut self, text: impl Into<String>) -> Self {
        self.after = Some(text.into());
        self
    }
}

/// The structural difference between two briefs of the same stage and
/// batch, in the brief's own order: objective, decisions, criteria,
/// interfaces, exclusions, files, units. Decisions and exclusions carry no
/// id and are matched by their exact text, so an edited one reads as
/// removed and added.
pub fn compare(previous: &ReviewBrief, current: &ReviewBrief) -> Vec<BriefChange> {
    let mut changes = Vec::new();
    if previous.objective != current.objective {
        changes.push(
            BriefChange::new(ChangeKind::Changed, ChangeSubject::Objective, "")
                .before(previous.objective.clone())
                .after(current.objective.clone()),
        );
    }
    compare_texts(
        &mut changes,
        ChangeSubject::Decision,
        &decisions(previous),
        &decisions(current),
    );
    compare_criteria(&mut changes, previous, current);
    compare_interfaces(&mut changes, previous, current);
    compare_texts(
        &mut changes,
        ChangeSubject::Exclusion,
        &unlabelled(&previous.exclusions),
        &unlabelled(&current.exclusions),
    );
    compare_files(&mut changes, previous, current);
    compare_units(&mut changes, previous, current);
    changes
}

fn decisions(brief: &ReviewBrief) -> Vec<(String, String)> {
    brief
        .judgement
        .iter()
        .filter(|item| item.kind == JudgementKind::Decision)
        .map(|item| (item.label.clone(), item.text.clone()))
        .collect()
}

fn unlabelled(texts: &[String]) -> Vec<(String, String)> {
    texts
        .iter()
        .map(|text| (String::new(), text.clone()))
        .collect()
}

/// `(label, text)` items without an id: removed ones first, then added ones.
fn compare_texts(
    changes: &mut Vec<BriefChange>,
    subject: ChangeSubject,
    previous: &[(String, String)],
    current: &[(String, String)],
) {
    let mut unmatched: Vec<Option<&(String, String)>> = previous.iter().map(Some).collect();
    let mut added = Vec::new();
    for item in current {
        match unmatched
            .iter_mut()
            .find(|slot| slot.is_some_and(|old| old.1 == item.1))
        {
            Some(slot) => *slot = None,
            None => added.push(item),
        }
    }
    for (label, text) in unmatched.into_iter().flatten() {
        changes.push(
            BriefChange::new(ChangeKind::Removed, subject, label.clone()).before(text.clone()),
        );
    }
    for (label, text) in added {
        changes
            .push(BriefChange::new(ChangeKind::Added, subject, label.clone()).after(text.clone()));
    }
}

fn verification_words(mode: VerificationMode) -> &'static str {
    match mode {
        VerificationMode::ExecutedCheck => "executed check",
        VerificationMode::DocumentCheck => "by reading",
        VerificationMode::NotApplicable => "not applicable",
    }
}

fn listed(items: &[String]) -> String {
    if items.is_empty() {
        "none".to_owned()
    } else {
        items.join(", ")
    }
}

/// Criteria by id. A criterion whose work units changed is reported here,
/// once, as moved.
fn compare_criteria(changes: &mut Vec<BriefChange>, previous: &ReviewBrief, current: &ReviewBrief) {
    let criteria = |brief: &ReviewBrief| -> Vec<(String, String, BriefCriterion)> {
        brief
            .tasks
            .iter()
            .flat_map(|task| {
                task.criteria
                    .iter()
                    .map(|criterion| (task.task_id.clone(), task.title.clone(), criterion.clone()))
            })
            .collect()
    };
    let units_of = |brief: &ReviewBrief, id: &str| -> Vec<String> {
        brief
            .units
            .iter()
            .filter(|unit| unit.criteria.iter().any(|criterion| criterion == id))
            .map(|unit| unit.key.clone())
            .collect()
    };
    let before = criteria(previous);
    let after = criteria(current);
    for (task_id, title, criterion) in &after {
        let id = criterion.criterion_id.as_str();
        let Some((old_task, old_title, old)) = before
            .iter()
            .find(|(_, _, old)| old.criterion_id == criterion.criterion_id)
        else {
            changes.push(
                BriefChange::new(ChangeKind::Added, ChangeSubject::Criterion, id)
                    .after(criterion.requirement.clone()),
            );
            continue;
        };
        if old_task != task_id {
            changes.push(
                BriefChange::new(ChangeKind::Moved, ChangeSubject::Criterion, id)
                    .field("task")
                    .before(old_title.clone())
                    .after(title.clone()),
            );
        }
        let reason = |criterion: &BriefCriterion| {
            criterion
                .not_applicable_reason
                .clone()
                .unwrap_or_else(|| "none".to_owned())
        };
        let fields = [
            (
                "requirement",
                old.requirement.clone(),
                criterion.requirement.clone(),
            ),
            (
                "proof",
                verification_words(old.verification).to_owned(),
                verification_words(criterion.verification).to_owned(),
            ),
            (
                "proof plan",
                old.verification_intent.clone(),
                criterion.verification_intent.clone(),
            ),
            ("checks", listed(&old.checks), listed(&criterion.checks)),
            ("reason", reason(old), reason(criterion)),
        ];
        for (field, was, now) in fields {
            if was != now {
                changes.push(
                    BriefChange::new(ChangeKind::Changed, ChangeSubject::Criterion, id)
                        .field(field)
                        .before(was)
                        .after(now),
                );
            }
        }
        let (was, now) = (units_of(previous, id), units_of(current, id));
        if was != now {
            changes.push(
                BriefChange::new(ChangeKind::Moved, ChangeSubject::Criterion, id)
                    .field("work units")
                    .before(listed(&was))
                    .after(listed(&now)),
            );
        }
    }
    for (_, _, old) in &before {
        if !after
            .iter()
            .any(|(_, _, criterion)| criterion.criterion_id == old.criterion_id)
        {
            changes.push(
                BriefChange::new(
                    ChangeKind::Removed,
                    ChangeSubject::Criterion,
                    old.criterion_id.clone(),
                )
                .before(old.requirement.clone()),
            );
        }
    }
}

fn full_path(root: &str, path: &str) -> String {
    if root.is_empty() {
        path.to_owned()
    } else {
        format!("{root}/{path}")
    }
}

fn intent_words(intent: InterfaceIntent) -> &'static str {
    match intent {
        InterfaceIntent::Preserve => "preserve",
        InterfaceIntent::Change => "change",
        InterfaceIntent::New => "new",
        InterfaceIntent::None => "no interface",
    }
}

/// Interfaces by the path they sit on; empty for those tied to no file.
fn interfaces_by_path(brief: &ReviewBrief) -> BTreeMap<String, Vec<BriefInterface>> {
    let mut map: BTreeMap<String, Vec<BriefInterface>> = BTreeMap::new();
    for group in &brief.files {
        for file in &group.files {
            map.entry(full_path(&group.root, &file.path))
                .or_default()
                .extend(file.interfaces.iter().cloned());
        }
    }
    if !brief.unattached_interfaces.is_empty() {
        map.entry(String::new())
            .or_default()
            .extend(brief.unattached_interfaces.iter().cloned());
    }
    map
}

/// Per path: an identical interface is unchanged; one with the same
/// description and another intent changed intent; the rest were removed or
/// added.
fn compare_interfaces(
    changes: &mut Vec<BriefChange>,
    previous: &ReviewBrief,
    current: &ReviewBrief,
) {
    let before = interfaces_by_path(previous);
    let after = interfaces_by_path(current);
    let paths: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    let none = Vec::new();
    for path in paths {
        let mut old: Vec<&BriefInterface> = before.get(path).unwrap_or(&none).iter().collect();
        let mut new = Vec::new();
        for interface in after.get(path).unwrap_or(&none) {
            match old.iter().position(|was| *was == interface) {
                Some(index) => {
                    old.remove(index);
                }
                None => new.push(interface),
            }
        }
        let mut added = Vec::new();
        for interface in new {
            match old
                .iter()
                .position(|was| was.description == interface.description)
            {
                Some(index) => {
                    let was = old.remove(index);
                    changes.push(
                        BriefChange::new(
                            ChangeKind::Changed,
                            ChangeSubject::Interface,
                            path.clone(),
                        )
                        .field("intent")
                        .before(intent_words(was.intent))
                        .after(intent_words(interface.intent)),
                    );
                }
                None => added.push(interface),
            }
        }
        let describe = |interface: &BriefInterface| {
            format!(
                "{}: {}",
                intent_words(interface.intent),
                interface.description
            )
        };
        for was in old {
            changes.push(
                BriefChange::new(ChangeKind::Removed, ChangeSubject::Interface, path.clone())
                    .before(describe(was)),
            );
        }
        for interface in added {
            changes.push(
                BriefChange::new(ChangeKind::Added, ChangeSubject::Interface, path.clone())
                    .after(describe(interface)),
            );
        }
    }
}

fn file_words(planned: bool) -> &'static str {
    if planned { "new file" } else { "existing file" }
}

fn compare_files(changes: &mut Vec<BriefChange>, previous: &ReviewBrief, current: &ReviewBrief) {
    let files = |brief: &ReviewBrief| -> Vec<(String, bool)> {
        brief
            .files
            .iter()
            .flat_map(|group| {
                group
                    .files
                    .iter()
                    .map(|file| (full_path(&group.root, &file.path), file.planned))
            })
            .collect()
    };
    let before = files(previous);
    let after = files(current);
    for (path, planned) in &after {
        match before.iter().find(|(old, _)| old == path) {
            None => changes.push(
                BriefChange::new(ChangeKind::Added, ChangeSubject::File, path.clone())
                    .after(file_words(*planned)),
            ),
            Some((_, was)) if was != planned => changes.push(
                BriefChange::new(ChangeKind::Changed, ChangeSubject::File, path.clone())
                    .field("status")
                    .before(file_words(*was))
                    .after(file_words(*planned)),
            ),
            Some(_) => {}
        }
    }
    for (path, planned) in &before {
        if !after.iter().any(|(new, _)| new == path) {
            changes.push(
                BriefChange::new(ChangeKind::Removed, ChangeSubject::File, path.clone())
                    .before(file_words(*planned)),
            );
        }
    }
}

fn kind_words(kind: ProposalKind) -> &'static str {
    match kind {
        ProposalKind::Code => "code",
        ProposalKind::Test => "test",
    }
}

/// Units by key. Criteria moving between units are reported under the
/// criterion, not here.
fn compare_units(changes: &mut Vec<BriefChange>, previous: &ReviewBrief, current: &ReviewBrief) {
    for unit in &current.units {
        let Some(old) = previous.units.iter().find(|old| old.key == unit.key) else {
            changes.push(
                BriefChange::new(ChangeKind::Added, ChangeSubject::Unit, unit.key.clone())
                    .after(unit.objective.clone()),
            );
            continue;
        };
        let fields = [
            (
                "kind",
                kind_words(old.kind).to_owned(),
                kind_words(unit.kind).to_owned(),
            ),
            ("objective", old.objective.clone(), unit.objective.clone()),
            (
                "writes",
                listed(&old.destinations),
                listed(&unit.destinations),
            ),
            (
                "depends on",
                listed(&old.depends_on),
                listed(&unit.depends_on),
            ),
        ];
        for (field, was, now) in fields {
            if was != now {
                changes.push(
                    BriefChange::new(ChangeKind::Changed, ChangeSubject::Unit, unit.key.clone())
                        .field(field)
                        .before(was)
                        .after(now),
                );
            }
        }
    }
    for old in &previous.units {
        if !current.units.iter().any(|unit| unit.key == old.key) {
            changes.push(
                BriefChange::new(ChangeKind::Removed, ChangeSubject::Unit, old.key.clone())
                    .before(old.objective.clone()),
            );
        }
    }
}

/// The `stage_payload_v1` record behind a checkpoint, decoded.
pub(crate) fn decode_record(content: &str) -> Result<StagePayloadRecord, serde_json::Error> {
    serde_json::from_str(content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::SchemaV1;
    use crate::orchestration::composition::recipes::UnitPayloadRecord;
    use crate::orchestration::composition::{
        CompletedPayloadV1, ExecutorKind, InterfaceIntentV1, ReferenceEntryV1, RelPath,
        RequirementV1, SourceRole, UnitModelResponseV1, UnitProposalV1,
    };

    fn h(value: &str) -> Handle {
        Handle::new(value).unwrap()
    }

    fn table() -> ReferenceTable {
        let mut builder = ReferenceTable::builder();
        builder
            .add_task(
                "t-core",
                1,
                "Export usage summary to CSV",
                &[
                    ("AC-1".into(), "One row per pulsation and model.".into()),
                    ("AC-2".into(), "Empty range writes only the header.".into()),
                ],
            )
            .unwrap();
        builder
            .add_task(
                "t-ui",
                1,
                "Expose CSV export",
                &[("AC-3".into(), "Export is disabled while loading.".into())],
            )
            .unwrap();
        builder.build()
    }

    fn requirement(
        task: &str,
        criterion: &str,
        mode: VerificationMode,
        reason: Option<&str>,
    ) -> RequirementV1 {
        RequirementV1 {
            task_ref: h(task),
            criterion_ref: h(criterion),
            requirement: format!("Requirement for {criterion}."),
            verification_intent: format!("Verify {criterion}."),
            verification_mode: mode,
            required_check_refs: Vec::new(),
            not_applicable_reason: reason.map(str::to_owned),
        }
    }

    fn record(spec: SpecificationContractV1) -> StagePayloadRecord {
        let sha = "0".repeat(64);
        StagePayloadRecord {
            schema_version: SchemaV1,
            stage: crate::orchestration::composition::Stage::Specification,
            plan_sha256: serde_json::from_value(serde_json::json!(sha)).unwrap(),
            references: vec![
                ReferenceEntryV1::ExistingSource {
                    handle: h("src_1"),
                    relative_path: RelPath::new("crates/hvc-core/src/usage.rs").unwrap(),
                    content_sha256: serde_json::from_value(serde_json::json!(sha)).unwrap(),
                    source_role: SourceRole::Source,
                },
                ReferenceEntryV1::ExistingSource {
                    handle: h("src_2"),
                    relative_path: RelPath::new("crates/hvc-ui/src/usage.rs").unwrap(),
                    content_sha256: serde_json::from_value(serde_json::json!(sha)).unwrap(),
                    source_role: SourceRole::Source,
                },
                ReferenceEntryV1::PlannedDestination {
                    handle: h("dst_1"),
                    relative_path: RelPath::new("crates/hvc-core/src/usage/export.rs").unwrap(),
                    source_role: SourceRole::Source,
                },
                ReferenceEntryV1::ExistingSource {
                    handle: h("src_3"),
                    relative_path: RelPath::new("README.md").unwrap(),
                    content_sha256: serde_json::from_value(serde_json::json!(sha)).unwrap(),
                    source_role: SourceRole::Document,
                },
            ],
            units: vec![UnitPayloadRecord {
                logical_unit_key: h("specification"),
                response: UnitModelResponseV1::Completed {
                    executor_kind: ExecutorKind::SpecificationAuthor,
                    payload: CompletedPayloadV1::Specification(spec),
                },
            }],
        }
    }

    fn spec() -> SpecificationContractV1 {
        SpecificationContractV1 {
            objective: "Export token usage to CSV.".into(),
            exclusions: vec!["No scheduled export.".into(), "No per-call rows.".into()],
            decisions: vec!["Aggregate per pulsation and model.".into()],
            requirements: vec![
                requirement("task_1", "crit_1_1", VerificationMode::ExecutedCheck, None),
                requirement("task_1", "crit_1_2", VerificationMode::DocumentCheck, None),
                requirement(
                    "task_2",
                    "crit_2_1",
                    VerificationMode::NotApplicable,
                    Some("Covered by the existing layout suite."),
                ),
            ],
            existing_file_refs: vec![h("src_1"), h("src_2"), h("src_3")],
            planned_file_refs: vec![h("dst_1")],
            interface_intent: vec![
                InterfaceIntentV1 {
                    intent: InterfaceIntent::Change,
                    source_refs: vec![h("src_2")],
                    criterion_refs: vec![h("crit_2_1")],
                    description: "The Usage view gains an Export button.".into(),
                },
                InterfaceIntentV1 {
                    intent: InterfaceIntent::Preserve,
                    source_refs: vec![h("src_1")],
                    criterion_refs: vec![h("crit_1_1")],
                    description: "query_usage keeps its signature.".into(),
                },
                InterfaceIntentV1 {
                    intent: InterfaceIntent::New,
                    source_refs: Vec::new(),
                    criterion_refs: vec![h("crit_1_1")],
                    description: "write_usage_csv is new.".into(),
                },
            ],
            unit_proposals: vec![
                UnitProposalV1 {
                    unit_key: h("core_export"),
                    kind: ProposalKind::Code,
                    task_refs: vec![h("task_1")],
                    criterion_refs: vec![h("crit_1_1"), h("crit_1_2")],
                    input_refs: vec![h("src_1")],
                    destination_refs: vec![h("src_1"), h("dst_1")],
                    depends_on: Vec::new(),
                    objective: "Write the CSV export.".into(),
                },
                UnitProposalV1 {
                    unit_key: h("ui_export"),
                    kind: ProposalKind::Code,
                    task_refs: vec![h("task_2")],
                    criterion_refs: vec![h("crit_2_1")],
                    input_refs: vec![h("src_2")],
                    destination_refs: vec![h("src_2")],
                    depends_on: vec![h("core_export")],
                    objective: "Add the Export button.".into(),
                },
            ],
        }
    }

    #[test]
    fn specification_brief_labels_handles_and_orders_judgement_by_rule() {
        let brief = build_specification(&spec(), &record(spec()), &table());
        assert_eq!(brief.next_stage, Some(PulsationStage::Contract));
        assert_eq!(
            brief.facts,
            BriefFacts {
                tasks: 2,
                criteria: 3,
                executed: 1,
                by_reading: 1,
                not_applicable: 1,
                existing_files: 3,
                planned_files: 1,
                changed_interfaces: 1,
                units: 2,
            }
        );
        let kinds: Vec<(JudgementKind, &str)> = brief
            .judgement
            .iter()
            .map(|item| (item.kind, item.label.as_str()))
            .collect();
        assert_eq!(
            kinds,
            [
                (JudgementKind::Decision, "D1"),
                (JudgementKind::ReadingOnlyProof, "AC-2"),
                (JudgementKind::NotApplicable, "AC-3"),
                (JudgementKind::InterfaceChange, "crates/hvc-ui/src/usage.rs"),
                (JudgementKind::Exclusions, "2"),
            ]
        );
        // The developer's own criterion text leads; the agent's intent follows.
        assert_eq!(
            brief.judgement[1].text,
            "Empty range writes only the header."
        );
        assert_eq!(
            brief.judgement[1].detail.as_deref(),
            Some("Verify crit_1_2.")
        );
        assert_eq!(
            brief.judgement[2].detail.as_deref(),
            Some("Covered by the existing layout suite.")
        );
        assert_eq!(
            brief.judgement[3].refs,
            ["crates/hvc-ui/src/usage.rs", "AC-3"]
        );
        assert_eq!(
            brief.judgement[4].text,
            "No scheduled export. · No per-call rows."
        );
    }

    #[test]
    fn specification_brief_groups_files_and_keeps_unattached_interfaces() {
        let brief = build_specification(&spec(), &record(spec()), &table());
        let groups: Vec<(&str, Vec<(&str, bool)>)> = brief
            .files
            .iter()
            .map(|group| {
                (
                    group.root.as_str(),
                    group
                        .files
                        .iter()
                        .map(|file| (file.path.as_str(), file.planned))
                        .collect(),
                )
            })
            .collect();
        assert_eq!(
            groups,
            [
                ("", vec![("README.md", false)]),
                (
                    "crates/hvc-core",
                    vec![("src/usage.rs", false), ("src/usage/export.rs", true)]
                ),
                ("crates/hvc-ui", vec![("src/usage.rs", false)]),
            ]
        );
        assert_eq!(
            brief.files[1].files[0].interfaces[0].intent,
            InterfaceIntent::Preserve
        );
        assert_eq!(brief.unattached_interfaces.len(), 1);
        assert_eq!(brief.unattached_interfaces[0].intent, InterfaceIntent::New);
    }

    #[test]
    fn specification_brief_lists_criteria_per_task_and_units_by_path() {
        let brief = build_specification(&spec(), &record(spec()), &table());
        assert_eq!(brief.tasks.len(), 2);
        assert_eq!(brief.tasks[0].title, "Export usage summary to CSV");
        let ids: Vec<&str> = brief.tasks[0]
            .criteria
            .iter()
            .map(|criterion| criterion.criterion_id.as_str())
            .collect();
        assert_eq!(ids, ["AC-1", "AC-2"]);
        assert_eq!(
            brief.units[0].destinations,
            [
                "crates/hvc-core/src/usage.rs",
                "crates/hvc-core/src/usage/export.rs"
            ]
        );
        assert_eq!(brief.units[0].criteria, ["AC-1", "AC-2"]);
        assert_eq!(brief.units[1].depends_on, ["core_export"]);
    }

    #[test]
    fn verified_lines_only_claim_what_validation_guarantees() {
        let mut minimal = spec();
        minimal.existing_file_refs.clear();
        minimal.planned_file_refs.clear();
        minimal.unit_proposals.clear();
        minimal.requirements.truncate(1);
        let brief = build_specification(&minimal, &record(minimal.clone()), &table());
        assert_eq!(
            brief.verified,
            ["The acceptance criterion has exactly one requirement."]
        );
        let full = build_specification(&spec(), &record(spec()), &table());
        assert_eq!(full.verified.len(), 5, "{:?}", full.verified);
    }

    #[test]
    fn root_split_is_stable() {
        assert_eq!(
            split_root("crates/hvc-ui/src/a.rs"),
            ("crates/hvc-ui".into(), "src/a.rs".into())
        );
        assert_eq!(split_root("docs/a.md"), ("docs".into(), "a.md".into()));
        assert_eq!(split_root("README.md"), (String::new(), "README.md".into()));
        assert_eq!(split_root("crates/x"), ("crates".into(), "x".into()));
    }

    fn brief_of(spec: SpecificationContractV1) -> ReviewBrief {
        build_specification(&spec, &record(spec.clone()), &table())
    }

    fn line(change: &BriefChange) -> String {
        format!(
            "{:?} {:?} {} {} | {} -> {}",
            change.kind,
            change.subject,
            change.label,
            change.field.unwrap_or("-"),
            change.before.as_deref().unwrap_or("-"),
            change.after.as_deref().unwrap_or("-"),
        )
    }

    #[test]
    fn an_unchanged_revision_has_no_changes() {
        assert!(compare(&brief_of(spec()), &brief_of(spec())).is_empty());
    }

    #[test]
    fn revision_changes_are_keyed_by_criterion_path_and_unit_in_brief_order() {
        let mut revised = spec();
        revised.objective = "Export token usage to CSV, per day.".into();
        revised.decisions = vec![
            "Aggregate per pulsation, model and day.".into(),
            "Round costs to cents.".into(),
        ];
        revised.requirements[1].verification_mode = VerificationMode::ExecutedCheck;
        revised.requirements[1].verification_intent = "Run the empty-range test.".into();
        revised.interface_intent[0].intent = InterfaceIntent::Preserve;
        revised.interface_intent.truncate(2);
        revised.exclusions.truncate(1);
        revised
            .existing_file_refs
            .retain(|handle| handle != &h("src_3"));
        revised.unit_proposals[1].unit_key = h("ui_button");

        let changes: Vec<String> = compare(&brief_of(spec()), &brief_of(revised))
            .iter()
            .map(line)
            .collect();
        assert_eq!(
            changes,
            [
                "Changed Objective  - | Export token usage to CSV. -> Export token usage to CSV, per day.",
                "Removed Decision D1 - | Aggregate per pulsation and model. -> -",
                "Added Decision D1 - | - -> Aggregate per pulsation, model and day.",
                "Added Decision D2 - | - -> Round costs to cents.",
                "Changed Criterion AC-2 proof | by reading -> executed check",
                "Changed Criterion AC-2 proof plan | Verify crit_1_2. -> Run the empty-range test.",
                "Moved Criterion AC-3 work units | ui_export -> ui_button",
                "Removed Interface  - | new: write_usage_csv is new. -> -",
                "Changed Interface crates/hvc-ui/src/usage.rs intent | change -> preserve",
                "Removed Exclusion  - | No per-call rows. -> -",
                "Removed File README.md - | existing file -> -",
                "Added Unit ui_button - | - -> Add the Export button.",
                "Removed Unit ui_export - | Add the Export button. -> -",
            ]
        );
    }

    #[test]
    fn a_criterion_the_previous_revision_lacked_reads_as_added_only() {
        let mut previous = spec();
        previous.requirements.truncate(2);
        let changes = compare(&brief_of(previous), &brief_of(spec()));
        let about_ac3: Vec<String> = changes
            .iter()
            .filter(|change| change.label == "AC-3")
            .map(line)
            .collect();
        assert_eq!(
            about_ac3,
            ["Added Criterion AC-3 - | - -> Requirement for crit_2_1."]
        );
    }
}
