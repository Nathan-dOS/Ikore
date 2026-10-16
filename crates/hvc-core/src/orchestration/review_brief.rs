//! The derived review brief of a developer checkpoint (ADR-027).
//!
//! A brief is built from the same validated typed payload that produced the
//! checkpoint's Markdown, never from the Markdown itself. It is a view model:
//! nothing here is persisted, and the candidate documents stay the record.
//! Every count, status and judgement rule is decided here by the host; text a
//! model wrote (objective, decisions, requirements, descriptions) is carried
//! as data and labelled by the renderer.

use std::collections::BTreeMap;

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
    /// Points only the developer can settle, in a fixed rule order.
    pub judgement: Vec<JudgementItem>,
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
        judgement,
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
}
