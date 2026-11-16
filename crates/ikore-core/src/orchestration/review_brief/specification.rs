//! The Specification brief: criteria and how each will be proven, the files
//! it touches with their interface intent, and the work-unit graph.

use std::collections::BTreeMap;

use super::{
    BriefCriterion, BriefFacts, BriefFile, BriefFileGroup, BriefInterface, BriefTask, BriefUnit,
    JudgementItem, JudgementKind, ReviewBrief, SpecificationVisual, StageVisual, criterion_id,
    path_of, plural, split_root,
};
use crate::orchestration::composition::recipes::StagePayloadRecord;
use crate::orchestration::composition::references::ReferenceTable;
use crate::orchestration::composition::{
    Handle, InterfaceIntent, SpecificationContractV1, VerificationMode,
};
use crate::persistence_service::PulsationStage;

pub(super) fn build(
    spec: &SpecificationContractV1,
    record: &StagePayloadRecord,
    table: &ReferenceTable,
) -> ReviewBrief {
    let criterion_id = |handle: &Handle| criterion_id(table, handle);
    let path = |handle: &Handle| path_of(record, handle);

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
            interfaces: Vec::new(),
        })
        .collect();

    ReviewBrief {
        stage: PulsationStage::Specification,
        next_stage: Some(PulsationStage::Contract),
        objective: Some(spec.objective.clone()),
        counts: counts(&facts),
        revision: None,
        judgement,
        verified: verified(&facts, spec),
        visual: StageVisual::Specification(SpecificationVisual {
            facts,
            exclusions: spec.exclusions.clone(),
            tasks,
            files,
            unattached_interfaces,
            units,
        }),
    }
}

/// The decision line's key counts.
fn counts(facts: &BriefFacts) -> Vec<String> {
    let mut proof = vec![format!("{} executed", facts.executed)];
    if facts.by_reading > 0 {
        proof.push(format!("{} by reading", facts.by_reading));
    }
    if facts.not_applicable > 0 {
        proof.push(format!("{} not applicable", facts.not_applicable));
    }
    let mut counts = vec![
        plural(facts.tasks, "task", "tasks"),
        format!(
            "{} · {}",
            plural(facts.criteria, "criterion", "criteria"),
            proof.join(", ")
        ),
        format!(
            "{} · {} new",
            plural(facts.existing_files + facts.planned_files, "file", "files"),
            facts.planned_files
        ),
    ];
    if facts.changed_interfaces > 0 {
        counts.push(format!(
            "{} changed",
            plural(
                facts.changed_interfaces,
                "existing interface",
                "existing interfaces"
            )
        ));
    }
    if facts.units > 0 {
        counts.push(plural(facts.units, "work unit", "work units"));
    }
    counts
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
