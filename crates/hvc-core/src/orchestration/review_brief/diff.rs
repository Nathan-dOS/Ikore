//! Diff between revisions (ADR-024 part 3): the previous candidate's brief
//! next to the new one, keyed by what stays stable across revisions.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::{
    BriefCheck, BriefCoverage, BriefCriterion, BriefFileGroup, BriefInterface, BriefInterfaceGroup,
    BriefUnit, BriefVerdict, JudgementKind, ReviewBrief, SpecificationVisual, StageVisual,
    check_words,
};
use crate::orchestration::composition::{
    CoverageKind, GroupIntent, InterfaceIntent, ProposalKind, Verdict, VerificationMode,
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
    /// The key compared: a criterion id, a path, an interface, unit or check
    /// key. Decisions and blockers carry no id, so theirs is the label they
    /// have in the revision they appear in. Empty for the objective,
    /// exclusions and interfaces tied to no file.
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
/// different stages compare only their objective.
pub fn compare(previous: &ReviewBrief, current: &ReviewBrief) -> Vec<BriefChange> {
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
            compare_criteria(&mut changes, before, after);
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
            compare_keyed(
                &mut changes,
                ChangeSubject::Criterion,
                &keyed_coverage(&before.coverage),
                &keyed_coverage(&after.coverage),
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
            compare_keyed(
                &mut changes,
                ChangeSubject::Criterion,
                &keyed_verdicts(&before.verdicts),
                &keyed_verdicts(&after.verdicts),
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

fn listed(items: &[String]) -> String {
    if items.is_empty() {
        "none".to_owned()
    } else {
        items.join(", ")
    }
}

/// Criteria by id. A criterion whose work units changed is reported here,
/// once, as moved.
fn compare_criteria(
    changes: &mut Vec<BriefChange>,
    previous: &SpecificationVisual,
    current: &SpecificationVisual,
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
    let units_of = |brief: &SpecificationVisual, id: &str| -> Vec<String> {
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
            (
                "interfaces",
                listed(&old.interfaces),
                listed(&unit.interfaces),
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

/// An item with a stable key, the one line shown when it is added or
/// removed, and the fields compared when it stays.
struct Keyed {
    key: String,
    summary: String,
    fields: Vec<(&'static str, String)>,
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
                BriefChange::new(ChangeKind::Added, subject, item.key.clone())
                    .after(item.summary.clone()),
            );
            continue;
        };
        for ((field, was), (_, now)) in old.fields.iter().zip(&item.fields) {
            if was != now {
                changes.push(
                    BriefChange::new(ChangeKind::Changed, subject, item.key.clone())
                        .field(field)
                        .before(was.clone())
                        .after(now.clone()),
                );
            }
        }
    }
    for old in previous {
        if !current.iter().any(|item| item.key == old.key) {
            changes.push(
                BriefChange::new(ChangeKind::Removed, subject, old.key.clone())
                    .before(old.summary.clone()),
            );
        }
    }
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
            summary: format!("{}: {}", group_intent_words(group.intent), group.signature),
            fields: vec![
                ("intent", group_intent_words(group.intent).to_owned()),
                ("signature", group.signature.clone()),
                ("invariants", group.invariants.join(" · ")),
                ("sources", listed(&group.sources)),
                ("criteria", listed(&group.criteria)),
                ("bound units", listed(&group.units)),
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

fn keyed_coverage(rows: &[BriefCoverage]) -> Vec<Keyed> {
    rows.iter()
        .map(|row| Keyed {
            key: row.criterion_id.clone(),
            summary: format!("{}: {}", coverage_words(row.coverage), row.explanation),
            fields: vec![
                ("coverage", coverage_words(row.coverage).to_owned()),
                ("tests", listed(&row.tests)),
                ("declared checks", listed(&row.declared_checks)),
                ("explanation", row.explanation.clone()),
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

fn keyed_verdicts(rows: &[BriefVerdict]) -> Vec<Keyed> {
    rows.iter()
        .map(|row| Keyed {
            key: row.criterion_id.clone(),
            summary: format!("{}: {}", verdict_words(row.verdict), row.explanation),
            fields: vec![
                ("verdict", verdict_words(row.verdict).to_owned()),
                ("checks", listed(&row.checks)),
                ("explanation", row.explanation.clone()),
            ],
        })
        .collect()
}

fn keyed_checks(checks: &[BriefCheck]) -> Vec<Keyed> {
    checks
        .iter()
        .map(|check| Keyed {
            key: check.check_ref.clone(),
            summary: check_words(check.status).to_owned(),
            fields: vec![("status", check_words(check.status).to_owned())],
        })
        .collect()
}
