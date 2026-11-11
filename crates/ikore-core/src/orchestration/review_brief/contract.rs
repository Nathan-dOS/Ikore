//! The Contract brief: every interface group with its signature, intent and
//! invariants, and the approved work units with the interfaces and
//! dependencies the contract binds to them.

use super::{
    Approved, BriefInterfaceGroup, BriefUnit, ContractVisual, JudgementItem, JudgementKind,
    ReviewBrief, StageVisual, criterion_id, path_of, plural, split_count,
};
use crate::orchestration::composition::recipes::StagePayloadRecord;
use crate::orchestration::composition::references::ReferenceTable;
use crate::orchestration::composition::{
    CompletedPayloadV1, GroupIntent, InterfaceContractV1, UnitModelResponseV1,
};
use crate::persistence_service::PulsationStage;

fn contract_of(record: &StagePayloadRecord) -> Option<&InterfaceContractV1> {
    match record.primary()? {
        UnitModelResponseV1::Completed {
            payload: CompletedPayloadV1::Contract(contract),
            ..
        } => Some(contract),
        _ => None,
    }
}

pub(super) fn build(
    record: &StagePayloadRecord,
    table: &ReferenceTable,
    approved: Approved<'_>,
) -> Option<ReviewBrief> {
    let contract = contract_of(record)?;
    let interfaces: Vec<BriefInterfaceGroup> = contract
        .groups
        .iter()
        .map(|group| BriefInterfaceGroup {
            key: group.interface_key.to_string(),
            intent: group.intent,
            signature: group.signature.clone(),
            invariants: group.invariants.clone(),
            sources: group
                .source_refs
                .iter()
                .map(|handle| path_of(record, handle))
                .collect(),
            criteria: group
                .criterion_refs
                .iter()
                .map(|handle| criterion_id(table, handle))
                .collect(),
            units: contract
                .unit_bindings
                .iter()
                .filter(|binding| binding.interface_keys.contains(&group.interface_key))
                .map(|binding| binding.unit_key.to_string())
                .collect(),
        })
        .collect();

    // The same rows the Contract document's "Planned units" table shows.
    let units: Vec<BriefUnit> = approved
        .map(|(spec, spec_record)| {
            spec.unit_proposals
                .iter()
                .map(|proposal| {
                    let binding = contract
                        .unit_bindings
                        .iter()
                        .find(|binding| binding.unit_key == proposal.unit_key);
                    let mut depends_on = proposal.depends_on.clone();
                    for dependency in binding.iter().flat_map(|binding| &binding.depends_on) {
                        if !depends_on.contains(dependency) {
                            depends_on.push(dependency.clone());
                        }
                    }
                    BriefUnit {
                        key: proposal.unit_key.to_string(),
                        kind: proposal.kind,
                        destinations: proposal
                            .destination_refs
                            .iter()
                            .map(|handle| path_of(spec_record, handle))
                            .collect(),
                        criteria: proposal
                            .criterion_refs
                            .iter()
                            .map(|handle| criterion_id(table, handle))
                            .collect(),
                        depends_on: depends_on.iter().map(ToString::to_string).collect(),
                        objective: proposal.objective.clone(),
                        interfaces: binding
                            .map(|binding| {
                                binding
                                    .interface_keys
                                    .iter()
                                    .map(ToString::to_string)
                                    .collect()
                            })
                            .unwrap_or_default(),
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    let judgement = interfaces
        .iter()
        .filter_map(|group| {
            let kind = match group.intent {
                GroupIntent::Change => JudgementKind::InterfaceChange,
                GroupIntent::New => JudgementKind::NewInterface,
                GroupIntent::Preserve => return None,
            };
            Some(JudgementItem {
                kind,
                label: group.key.clone(),
                text: group.signature.clone(),
                detail: (!group.invariants.is_empty())
                    .then(|| format!("Invariants: {}", group.invariants.join(" · "))),
                refs: group
                    .sources
                    .iter()
                    .chain(group.criteria.iter())
                    .cloned()
                    .collect(),
            })
        })
        .collect();

    let count = |intent: GroupIntent| {
        interfaces
            .iter()
            .filter(|group| group.intent == intent)
            .count()
    };
    let mut counts = vec![if interfaces.is_empty() {
        "No new or changed interface".to_owned()
    } else {
        split_count(
            plural(interfaces.len(), "interface group", "interface groups"),
            &[
                (count(GroupIntent::Change), "changed"),
                (count(GroupIntent::New), "new"),
                (count(GroupIntent::Preserve), "preserved"),
            ],
        )
    }];
    if !units.is_empty() {
        let bound = units
            .iter()
            .filter(|unit| !unit.interfaces.is_empty())
            .count();
        counts.push(split_count(
            plural(units.len(), "work unit", "work units"),
            &[(bound, "bound to an interface")],
        ));
    }

    Some(ReviewBrief {
        stage: PulsationStage::Contract,
        next_stage: Some(PulsationStage::Code),
        objective: approved.map(|(spec, _)| spec.objective.clone()),
        counts,
        revision: None,
        judgement,
        verified: verified(contract),
        visual: StageVisual::Contract(ContractVisual { interfaces, units }),
    })
}

/// What host validation guarantees for an accepted Contract payload
/// (`composition::validate`). Only facts that hold by construction appear.
fn verified(contract: &InterfaceContractV1) -> Vec<String> {
    let mut lines = Vec::new();
    if !contract.groups.is_empty() {
        lines.push(
            "Every interface key is defined once and cites at least one criterion in scope."
                .to_owned(),
        );
        if contract
            .groups
            .iter()
            .any(|group| !group.source_refs.is_empty())
        {
            lines.push(
                "Every source an interface cites was delivered to the contract author.".to_owned(),
            );
        }
    }
    if !contract.unit_bindings.is_empty() {
        lines.push(
            "Every binding names an approved work unit once and only interfaces defined here."
                .to_owned(),
        );
        lines.push("The bound work units' dependencies form no cycle.".to_owned());
    }
    lines
}
