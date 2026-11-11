//! Diff between revisions (ADR-027 part 3): the previous candidate's brief
//! next to the new one, keyed by what stays stable across revisions.
//!
//! Criterion ids are local to their task, so criteria, coverage and verdicts
//! are keyed by task and id, and labelled with the task title when two tasks
//! share an id. Set-valued fields compare as sets: reordering them is not a
//! change.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::{
    BriefCheck, BriefContext, BriefCoverage, BriefCriterion, BriefFileGroup, BriefInterface,
    BriefInterfaceGroup, BriefUnit, BriefVerdict, JudgementKind, ReviewBrief, SpecificationVisual,
    StageVisual, check_words, full_path, stage_brief,
};
use crate::orchestration::composition::recipes::{
    StagePayloadRecord, specification_of, task_table,
};
use crate::orchestration::composition::references::ReferenceTable;
use crate::orchestration::composition::{
    CompositionError, CoverageKind, GroupIntent, InterfaceIntent, ProposalKind,
    SpecificationContractV1, Verdict, VerificationMode,
};

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
    /// A host check, keyed by its handle.
    Check,
    /// An Integrator blocker, matched by its text.
    Blocker,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BriefChange {
    pub kind: ChangeKind,
    pub subject: ChangeSubject,
    /// The key compared: a criterion id (prefixed with its task title when
    /// two tasks share it), a path, an interface, unit or check key.
    /// Decisions and blockers carry no id, so theirs is the label they have
    /// in the revision they appear in. Empty for the objective, exclusions
    /// and interfaces tied to no file.
    pub label: String,
    /// The part of the item that changed, for `Changed` and `Moved`.
    pub field: Option<&'static str>,
    pub before: Option<String>,
    pub after: Option<String>,
}

impl BriefChange {
    pub(super) fn new(kind: ChangeKind, subject: ChangeSubject, label: impl Into<String>) -> Self {
        Self {
            kind,
            subject,
            label: label.into(),
            field: None,
            before: None,
            after: None,
        }
    }

    pub(super) fn field(mut self, field: &'static str) -> Self {
        self.field = Some(field);
        self
    }

    pub(super) fn before(mut self, text: impl Into<String>) -> Self {
        self.before = Some(text.into());
        self
    }

    pub(super) fn after(mut self, text: impl Into<String>) -> Self {
        self.after = Some(text.into());
        self
    }
}

/// Work units per `(task id, criterion id)`, sorted.
type CriterionUnits = BTreeMap<(String, String), Vec<String>>;

/// The structural difference between two briefs of the same stage and
/// batch, in the brief's own order:
///
/// - Specification: objective, decisions, criteria, interfaces, exclusions,
///   files, units;
/// - Contract: interfaces by key, then units;
/// - Test: coverage by criterion, checks, test files;
/// - Integrator: verdicts by criterion, blockers, checks.
///
/// Decisions, exclusions and blockers carry no id and are matched by their
/// exact text, so an edited one reads as removed and added. Briefs of two
/// different stages compare only their objective. From the public view a
/// criterion's work units are known only when its id names one task in the
/// brief; [`compare_records`] knows them from the typed handles.
pub fn compare(previous: &ReviewBrief, current: &ReviewBrief) -> Vec<BriefChange> {
    let units = match (&previous.visual, &current.visual) {
        (StageVisual::Specification(before), StageVisual::Specification(after)) => Some((
            unambiguous_criterion_units(before),
            unambiguous_criterion_units(after),
        )),
        _ => None,
    };
    compare_with(previous, current, units.as_ref())
}

/// The previous and current candidates compared through their typed
/// records: their briefs, with every criterion's work units taken from the
/// task-scoped handles. `None` when either record has no brief.
pub(crate) fn compare_records(
    previous: &StagePayloadRecord,
    previous_context: &BriefContext<'_>,
    current: &StagePayloadRecord,
    current_context: &BriefContext<'_>,
) -> Result<Option<Vec<BriefChange>>, CompositionError> {
    let (Some(before), Some(after)) = (
        stage_brief(previous, previous_context)?,
        stage_brief(current, current_context)?,
    ) else {
        return Ok(None);
    };
    let units = match (specification_of(previous), specification_of(current)) {
        (Some(previous_spec), Some(current_spec)) => {
            let table = task_table(current_context.manifest)?;
            Some((
                scoped_criterion_units(previous_spec, &table),
                scoped_criterion_units(current_spec, &table),
            ))
        }
        _ => None,
    };
    Ok(Some(compare_with(&before, &after, units.as_ref())))
}

fn compare_with(
    previous: &ReviewBrief,
    current: &ReviewBrief,
    units: Option<&(CriterionUnits, CriterionUnits)>,
) -> Vec<BriefChange> {
    let mut changes = Vec::new();
    if previous.objective != current.objective {
        let mut change = BriefChange::new(ChangeKind::Changed, ChangeSubject::Objective, "");
        change.before = previous.objective.clone();
        change.after = current.objective.clone();
        changes.push(change);
    }
    match (&previous.visual, &current.visual) {
        (StageVisual::Specification(before), StageVisual::Specification(after)) => {
            compare_texts(
                &mut changes,
                ChangeSubject::Decision,
                &decisions(previous),
                &decisions(current),
            );
            let none = (CriterionUnits::new(), CriterionUnits::new());
            let (previous_units, current_units) = units.unwrap_or(&none);
            compare_criteria(&mut changes, before, after, previous_units, current_units);
            compare_interfaces(&mut changes, before, after);
            compare_texts(
                &mut changes,
                ChangeSubject::Exclusion,
                &unlabelled(&before.exclusions),
                &unlabelled(&after.exclusions),
            );
            compare_files(&mut changes, &before.files, &after.files, true);
            compare_units(&mut changes, &before.units, &after.units);
        }
        (StageVisual::Contract(before), StageVisual::Contract(after)) => {
            compare_keyed(
                &mut changes,
                ChangeSubject::Interface,
                &keyed_interfaces(&before.interfaces),
                &keyed_interfaces(&after.interfaces),
            );
            compare_units(&mut changes, &before.units, &after.units);
        }
        (StageVisual::Test(before), StageVisual::Test(after)) => {
            let labels = CriterionLabels::of(
                before
                    .coverage
                    .iter()
                    .chain(&after.coverage)
                    .map(|row| (&row.task_id, &row.task_title, &row.criterion_id)),
            );
            compare_keyed(
                &mut changes,
                ChangeSubject::Criterion,
                &keyed_coverage(&before.coverage, &labels),
                &keyed_coverage(&after.coverage, &labels),
            );
            compare_keyed(
                &mut changes,
                ChangeSubject::Check,
                &keyed_checks(&before.checks),
                &keyed_checks(&after.checks),
            );
            // Whether a test file is created or replaced depends on what the
            // previous candidate left on disk, not on the agent: only added
            // and removed files are reported.
            compare_files(&mut changes, &before.files, &after.files, false);
        }
        (StageVisual::Integrator(before), StageVisual::Integrator(after)) => {
            let labels = CriterionLabels::of(
                before
                    .verdicts
                    .iter()
                    .chain(&after.verdicts)
                    .map(|row| (&row.task_id, &row.task_title, &row.criterion_id)),
            );
            compare_keyed(
                &mut changes,
                ChangeSubject::Criterion,
                &keyed_verdicts(&before.verdicts, &labels),
                &keyed_verdicts(&after.verdicts, &labels),
            );
            compare_texts(
                &mut changes,
                ChangeSubject::Blocker,
                &numbered(&before.blockers, "B"),
                &numbered(&after.blockers, "B"),
            );
            compare_keyed(
                &mut changes,
                ChangeSubject::Check,
                &keyed_checks(&before.checks),
                &keyed_checks(&after.checks),
            );
        }
        _ => {}
    }
    changes
}

// ---------------------------------------------------------------------------
// Task-scoped criteria
// ---------------------------------------------------------------------------

/// Labels criterion ids, prefixing the task title only where two tasks
/// share an id in either brief.
struct CriterionLabels {
    ambiguous: BTreeSet<String>,
}

impl CriterionLabels {
    fn of<'a>(rows: impl Iterator<Item = (&'a String, &'a String, &'a String)>) -> Self {
        let mut owners: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for (task_id, _, criterion_id) in rows {
            owners.entry(criterion_id).or_default().insert(task_id);
        }
        Self {
            ambiguous: owners
                .into_iter()
                .filter(|(_, tasks)| tasks.len() > 1)
                .map(|(id, _)| id.to_owned())
                .collect(),
        }
    }

    fn label(&self, task_title: &str, criterion_id: &str) -> String {
        if self.ambiguous.contains(criterion_id) {
            format!("{task_title} · {criterion_id}")
        } else {
            criterion_id.to_owned()
        }
    }
}

/// From the typed handles: the work units that cover each task's criterion.
fn scoped_criterion_units(
    spec: &SpecificationContractV1,
    table: &ReferenceTable,
) -> CriterionUnits {
    let mut units = CriterionUnits::new();
    for (_, task) in table.tasks() {
        for handle in &task.criteria {
            if let Some(criterion) = table.criterion(handle) {
                units.insert(
                    (task.task_id.clone(), criterion.criterion_id.clone()),
                    Vec::new(),
                );
            }
        }
    }
    for unit in &spec.unit_proposals {
        for handle in &unit.criterion_refs {
            if let Some(criterion) = table.criterion(handle)
                && let Some(task) = table.task(&criterion.task)
                && let Some(owners) =
                    units.get_mut(&(task.task_id.clone(), criterion.criterion_id.clone()))
            {
                owners.push(unit.unit_key.to_string());
            }
        }
    }
    canonicalize_criterion_units(units)
}

/// From the public view: only criterion ids that name one task can be tied
/// to their work units, whose labels carry ids alone.
fn unambiguous_criterion_units(visual: &SpecificationVisual) -> CriterionUnits {
    let mut owners: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for task in &visual.tasks {
        for criterion in &task.criteria {
            owners
                .entry(&criterion.criterion_id)
                .or_default()
                .push(&task.task_id);
        }
    }
    canonicalize_criterion_units(
        owners
            .into_iter()
            .filter(|(_, tasks)| tasks.len() == 1)
            .map(|(id, tasks)| {
                (
                    (tasks[0].to_owned(), id.to_owned()),
                    visual
                        .units
                        .iter()
                        .filter(|unit| unit.criteria.iter().any(|criterion| criterion == id))
                        .map(|unit| unit.key.clone())
                        .collect(),
                )
            })
            .collect(),
    )
}

fn canonicalize_criterion_units(mut units: CriterionUnits) -> CriterionUnits {
    for keys in units.values_mut() {
        keys.sort_unstable();
        keys.dedup();
    }
    units
}

// ---------------------------------------------------------------------------
// Shared comparison helpers
// ---------------------------------------------------------------------------

fn decisions(brief: &ReviewBrief) -> Vec<(String, String)> {
    brief
        .judgement
        .iter()
        .filter(|item| item.kind == JudgementKind::Decision)
        .map(|item| (item.label.clone(), item.text.clone()))
        .collect()
}

fn numbered(texts: &[String], prefix: &str) -> Vec<(String, String)> {
    texts
        .iter()
        .enumerate()
        .map(|(index, text)| (format!("{prefix}{}", index + 1), text.clone()))
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

fn changed_text(before: String, after: String) -> Option<(String, String)> {
    (before != after).then_some((before, after))
}

fn listed(items: &BTreeSet<&str>) -> String {
    if items.is_empty() {
        "none".to_owned()
    } else {
        items.iter().copied().collect::<Vec<_>>().join(", ")
    }
}

fn changed_set_text(before: &[String], after: &[String]) -> Option<(String, String)> {
    // Compare the raw sets, never their presentation: [] and ["none"],
    // or ["a, b"] and ["a", "b"], must remain distinguishable.
    let before: BTreeSet<&str> = before.iter().map(String::as_str).collect();
    let after: BTreeSet<&str> = after.iter().map(String::as_str).collect();
    if before == after {
        return None;
    }
    let (was, now) = (listed(&before), listed(&after));
    if was == now {
        Some((
            serde_json::to_string(&before).expect("string sets serialize"),
            serde_json::to_string(&after).expect("string sets serialize"),
        ))
    } else {
        Some((was, now))
    }
}

// ---------------------------------------------------------------------------
// Specification
// ---------------------------------------------------------------------------

/// Criterion ids are local to each task. A criterion whose known work units
/// changed is reported here, once, as moved.
fn compare_criteria(
    changes: &mut Vec<BriefChange>,
    previous: &SpecificationVisual,
    current: &SpecificationVisual,
    previous_units: &CriterionUnits,
    current_units: &CriterionUnits,
) {
    let criteria = |brief: &SpecificationVisual| -> Vec<(String, String, BriefCriterion)> {
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
    let before = criteria(previous);
    let after = criteria(current);
    let labels = CriterionLabels::of(
        before
            .iter()
            .chain(&after)
            .map(|(task_id, title, criterion)| (task_id, title, &criterion.criterion_id)),
    );
    for (task_id, title, criterion) in &after {
        let label = labels.label(title, &criterion.criterion_id);
        let Some((_, _, old)) = before
            .iter()
            .find(|(owner, _, old)| owner == task_id && old.criterion_id == criterion.criterion_id)
        else {
            changes.push(
                BriefChange::new(ChangeKind::Added, ChangeSubject::Criterion, label)
                    .after(criterion.requirement.clone()),
            );
            continue;
        };
        let reason = |criterion: &BriefCriterion| {
            criterion
                .not_applicable_reason
                .clone()
                .unwrap_or_else(|| "none".to_owned())
        };
        let fields = [
            (
                "requirement",
                changed_text(old.requirement.clone(), criterion.requirement.clone()),
            ),
            (
                "proof",
                changed_text(
                    verification_words(old.verification).to_owned(),
                    verification_words(criterion.verification).to_owned(),
                ),
            ),
            (
                "proof plan",
                changed_text(
                    old.verification_intent.clone(),
                    criterion.verification_intent.clone(),
                ),
            ),
            ("checks", changed_set_text(&old.checks, &criterion.checks)),
            ("reason", changed_text(reason(old), reason(criterion))),
        ];
        for (field, change) in fields {
            if let Some((was, now)) = change {
                changes.push(
                    BriefChange::new(ChangeKind::Changed, ChangeSubject::Criterion, label.clone())
                        .field(field)
                        .before(was)
                        .after(now),
                );
            }
        }
        let key = (task_id.clone(), criterion.criterion_id.clone());
        if let (Some(was), Some(now)) = (previous_units.get(&key), current_units.get(&key))
            && let Some((was, now)) = changed_set_text(was, now)
        {
            changes.push(
                BriefChange::new(ChangeKind::Moved, ChangeSubject::Criterion, label)
                    .field("work units")
                    .before(was)
                    .after(now),
            );
        }
    }
    for (task_id, title, old) in &before {
        if !after.iter().any(|(owner, _, criterion)| {
            owner == task_id && criterion.criterion_id == old.criterion_id
        }) {
            changes.push(
                BriefChange::new(
                    ChangeKind::Removed,
                    ChangeSubject::Criterion,
                    labels.label(title, &old.criterion_id),
                )
                .before(old.requirement.clone()),
            );
        }
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
fn interfaces_by_path(brief: &SpecificationVisual) -> BTreeMap<String, Vec<BriefInterface>> {
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
    previous: &SpecificationVisual,
    current: &SpecificationVisual,
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

/// Files by path. `status` also reports a file that switched between new and
/// existing.
fn compare_files(
    changes: &mut Vec<BriefChange>,
    previous: &[BriefFileGroup],
    current: &[BriefFileGroup],
    status: bool,
) {
    let files = |groups: &[BriefFileGroup]| -> Vec<(String, bool)> {
        groups
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
            Some((_, was)) if status && was != planned => changes.push(
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
fn compare_units(changes: &mut Vec<BriefChange>, previous: &[BriefUnit], current: &[BriefUnit]) {
    for unit in current {
        let Some(old) = previous.iter().find(|old| old.key == unit.key) else {
            changes.push(
                BriefChange::new(ChangeKind::Added, ChangeSubject::Unit, unit.key.clone())
                    .after(unit.objective.clone()),
            );
            continue;
        };
        let fields = [
            (
                "kind",
                changed_text(
                    kind_words(old.kind).to_owned(),
                    kind_words(unit.kind).to_owned(),
                ),
            ),
            (
                "objective",
                changed_text(old.objective.clone(), unit.objective.clone()),
            ),
            (
                "writes",
                changed_set_text(&old.destinations, &unit.destinations),
            ),
            (
                "depends on",
                changed_set_text(&old.depends_on, &unit.depends_on),
            ),
            (
                "interfaces",
                changed_set_text(&old.interfaces, &unit.interfaces),
            ),
        ];
        for (field, change) in fields {
            if let Some((was, now)) = change {
                changes.push(
                    BriefChange::new(ChangeKind::Changed, ChangeSubject::Unit, unit.key.clone())
                        .field(field)
                        .before(was)
                        .after(now),
                );
            }
        }
    }
    for old in previous {
        if !current.iter().any(|unit| unit.key == old.key) {
            changes.push(
                BriefChange::new(ChangeKind::Removed, ChangeSubject::Unit, old.key.clone())
                    .before(old.objective.clone()),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Keyed items (Contract, Test, Integrator)
// ---------------------------------------------------------------------------

/// A field compared when a keyed item stays: one text, or a set whose order
/// does not matter.
enum Field {
    Text(String),
    Set(Vec<String>),
}

impl Field {
    fn changed(&self, after: &Self) -> Option<(String, String)> {
        match (self, after) {
            (Self::Text(before), Self::Text(after)) => changed_text(before.clone(), after.clone()),
            (Self::Set(before), Self::Set(after)) => changed_set_text(before, after),
            _ => None,
        }
    }
}

/// An item with a stable key, the label the developer reads, the one line
/// shown when it is added or removed, and the fields compared when it stays.
struct Keyed {
    key: String,
    label: String,
    summary: String,
    fields: Vec<(&'static str, Field)>,
}

/// Added and changed items in the current order, then removed ones in the
/// previous order.
fn compare_keyed(
    changes: &mut Vec<BriefChange>,
    subject: ChangeSubject,
    previous: &[Keyed],
    current: &[Keyed],
) {
    for item in current {
        let Some(old) = previous.iter().find(|old| old.key == item.key) else {
            changes.push(
                BriefChange::new(ChangeKind::Added, subject, item.label.clone())
                    .after(item.summary.clone()),
            );
            continue;
        };
        for ((field, was), (_, now)) in old.fields.iter().zip(&item.fields) {
            if let Some((was, now)) = was.changed(now) {
                changes.push(
                    BriefChange::new(ChangeKind::Changed, subject, item.label.clone())
                        .field(field)
                        .before(was)
                        .after(now),
                );
            }
        }
    }
    for old in previous {
        if !current.iter().any(|item| item.key == old.key) {
            changes.push(
                BriefChange::new(ChangeKind::Removed, subject, old.label.clone())
                    .before(old.summary.clone()),
            );
        }
    }
}

/// The key of a task-scoped criterion; the separator cannot occur in ids.
fn criterion_key(task_id: &str, criterion_id: &str) -> String {
    format!("{task_id}\u{1f}{criterion_id}")
}

fn group_intent_words(intent: GroupIntent) -> &'static str {
    match intent {
        GroupIntent::Preserve => "preserve",
        GroupIntent::Change => "change",
        GroupIntent::New => "new",
    }
}

fn keyed_interfaces(groups: &[BriefInterfaceGroup]) -> Vec<Keyed> {
    groups
        .iter()
        .map(|group| Keyed {
            key: group.key.clone(),
            label: group.key.clone(),
            summary: format!("{}: {}", group_intent_words(group.intent), group.signature),
            fields: vec![
                (
                    "intent",
                    Field::Text(group_intent_words(group.intent).to_owned()),
                ),
                ("signature", Field::Text(group.signature.clone())),
                ("invariants", Field::Set(group.invariants.clone())),
                ("sources", Field::Set(group.sources.clone())),
                ("criteria", Field::Set(group.criteria.clone())),
                ("bound units", Field::Set(group.units.clone())),
            ],
        })
        .collect()
}

fn coverage_words(coverage: CoverageKind) -> &'static str {
    match coverage {
        CoverageKind::Authored => "authored",
        CoverageKind::Existing => "existing",
        CoverageKind::NotApplicable => "not applicable",
    }
}

fn keyed_coverage(rows: &[BriefCoverage], labels: &CriterionLabels) -> Vec<Keyed> {
    rows.iter()
        .map(|row| Keyed {
            key: criterion_key(&row.task_id, &row.criterion_id),
            label: labels.label(&row.task_title, &row.criterion_id),
            summary: format!("{}: {}", coverage_words(row.coverage), row.explanation),
            fields: vec![
                (
                    "coverage",
                    Field::Text(coverage_words(row.coverage).to_owned()),
                ),
                ("tests", Field::Set(row.tests.clone())),
                ("declared checks", Field::Set(row.declared_checks.clone())),
                ("explanation", Field::Text(row.explanation.clone())),
            ],
        })
        .collect()
}

fn verdict_words(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Supported => "supported",
        Verdict::Contradicted => "contradicted",
        Verdict::Unverified => "unverified",
    }
}

fn keyed_verdicts(rows: &[BriefVerdict], labels: &CriterionLabels) -> Vec<Keyed> {
    rows.iter()
        .map(|row| Keyed {
            key: criterion_key(&row.task_id, &row.criterion_id),
            label: labels.label(&row.task_title, &row.criterion_id),
            summary: format!("{}: {}", verdict_words(row.verdict), row.explanation),
            fields: vec![
                (
                    "verdict",
                    Field::Text(verdict_words(row.verdict).to_owned()),
                ),
                ("checks", Field::Set(row.checks.clone())),
                ("explanation", Field::Text(row.explanation.clone())),
            ],
        })
        .collect()
}

fn keyed_checks(checks: &[BriefCheck]) -> Vec<Keyed> {
    checks
        .iter()
        .map(|check| Keyed {
            key: check.check_ref.clone(),
            label: check.check_ref.clone(),
            summary: check_words(check.status).to_owned(),
            fields: vec![("status", Field::Text(check_words(check.status).to_owned()))],
        })
        .collect()
}
