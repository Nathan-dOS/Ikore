//! Deterministic stage aggregation (SC-010, SC-018).
//!
//! Validated unit outputs and host-rendered documents become one
//! `StageAggregateV1` whose artifact order, evidence mapping and digest do not
//! depend on the order in which units completed. Exactly one task-evidence row
//! per scope task is emitted; a task without evidence is a defect, never a
//! silently empty success. The aggregate is then lowered to the legacy
//! `StageOutcome` + `TaskEvidence` rows the existing acceptance transaction
//! already understands, so one acceptance path serves v1-v4.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use super::references::ReferenceTable;
use super::{
    ArtifactKind, CompositionError, ErrorCode, EvidenceItemV1, Handle, Invariant, RelPath,
    Sha256Hex, StageAggregateV1, StageArtifactV1, StageExecutionPlanV1, Uuid, Validate,
    WorkUnitSpecV1, canonical_sha256, scalar_len,
};
use crate::feature_forge::TaskEvidence;
use crate::orchestration::{RelativeSourcePath, SchemaV1, StageArtifactDraft, StageOutcome};

/// A destination reconstructed from one unit's validated edits, or a document
/// a unit authored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconstructedArtifact {
    pub artifact_key: Handle,
    pub relative_path: Option<RelPath>,
    pub kind: ArtifactKind,
    pub content: String,
}

/// One validated unit's contribution to the aggregate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitContribution<'a> {
    pub unit: &'a WorkUnitSpecV1,
    pub evidence: Vec<EvidenceItemV1>,
    pub artifacts: Vec<ReconstructedArtifact>,
}

/// A document the host rendered (report, note, contract document).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostArtifact {
    pub artifact_key: Handle,
    pub relative_path: Option<RelPath>,
    pub kind: ArtifactKind,
    pub content: String,
}

fn rejection(code: ErrorCode, message: impl Into<String>) -> CompositionError {
    CompositionError::new(Invariant::Sc010, code, message)
}

const fn kind_rank(kind: ArtifactKind) -> u8 {
    match kind {
        ArtifactKind::Intake => 0,
        ArtifactKind::Specification => 1,
        ArtifactKind::Contract => 2,
        ArtifactKind::Code => 3,
        ArtifactKind::Test => 4,
        ArtifactKind::IntegrationReport => 5,
        ArtifactKind::Documentation => 6,
        ArtifactKind::ValidationReport => 7,
    }
}

pub const fn legacy_kind(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Intake => "intake_note",
        ArtifactKind::Specification => "specification",
        ArtifactKind::Contract => "contract",
        ArtifactKind::Code => "code",
        ArtifactKind::Test => "test",
        ArtifactKind::IntegrationReport => "integration_report",
        ArtifactKind::Documentation => "documentation",
        ArtifactKind::ValidationReport => "validation_report",
    }
}

/// Builds the aggregate from unit contributions and host artifacts.
pub fn build_aggregate(
    plan: &StageExecutionPlanV1,
    table: &ReferenceTable,
    contributions: &[UnitContribution<'_>],
    host_artifacts: Vec<HostArtifact>,
    validation_receipt_ids: Vec<Uuid>,
    findings: Vec<String>,
) -> Result<StageAggregateV1, CompositionError> {
    // Contributions are visited in plan order, never in completion order.
    let unit_position: HashMap<&Uuid, usize> = plan
        .units
        .iter()
        .enumerate()
        .map(|(index, unit)| (&unit.unit_id, index))
        .collect();
    let mut contributions = contributions.iter().collect::<Vec<_>>();
    for contribution in &contributions {
        if !unit_position.contains_key(&contribution.unit.unit_id) {
            return Err(rejection(
                ErrorCode::HostContractError,
                format!(
                    "Unit `{}` is not part of the plan.",
                    contribution.unit.logical_unit_key
                ),
            ));
        }
    }
    contributions.sort_by_key(|contribution| unit_position[&contribution.unit.unit_id]);
    let contributions = contributions;
    // Collect every artifact with its producing key, then order deterministically.
    struct Pending {
        origin: (Option<Handle>, Handle),
        relative_path: Option<RelPath>,
        kind: ArtifactKind,
        content: String,
    }
    let mut pending = Vec::new();
    for contribution in &contributions {
        for artifact in &contribution.artifacts {
            pending.push(Pending {
                origin: (
                    Some(contribution.unit.logical_unit_key.clone()),
                    artifact.artifact_key.clone(),
                ),
                relative_path: artifact.relative_path.clone(),
                kind: artifact.kind,
                content: artifact.content.clone(),
            });
        }
    }
    for artifact in host_artifacts {
        pending.push(Pending {
            origin: (None, artifact.artifact_key),
            relative_path: artifact.relative_path,
            kind: artifact.kind,
            content: artifact.content,
        });
    }
    pending.sort_by(|a, b| {
        (
            kind_rank(a.kind),
            a.relative_path.as_ref().map(RelPath::canonical_key),
            a.origin.0.as_ref().map(|h| h.as_str().to_owned()),
            a.origin.1.as_str().to_owned(),
        )
            .cmp(&(
                kind_rank(b.kind),
                b.relative_path.as_ref().map(RelPath::canonical_key),
                b.origin.0.as_ref().map(|h| h.as_str().to_owned()),
                b.origin.1.as_str().to_owned(),
            ))
    });
    let mut destinations = HashSet::new();
    let mut remap: HashMap<(Option<Handle>, Handle), Handle> = HashMap::new();
    let mut artifacts = Vec::with_capacity(pending.len());
    for (index, item) in pending.into_iter().enumerate() {
        if let Some(path) = &item.relative_path
            && !destinations.insert(path.canonical_key())
        {
            return Err(rejection(
                ErrorCode::OwnershipViolation,
                format!("Two artifacts target `{path}` under Windows identity."),
            ));
        }
        let key = Handle::new(format!("art_{}", index + 1)).expect("generated handle");
        if remap.insert(item.origin.clone(), key.clone()).is_some() {
            return Err(rejection(
                ErrorCode::HostContractError,
                format!(
                    "Artifact key `{}` is produced twice by the same origin.",
                    item.origin.1
                ),
            ));
        }
        artifacts.push(StageArtifactV1 {
            artifact_key: key,
            relative_path: item.relative_path,
            kind: item.kind,
            content_sha256: Sha256Hex::of(item.content.as_bytes()),
            content: item.content,
        });
    }

    // One evidence row per scope task, in scope order.
    let artifact_order: HashMap<&Handle, usize> = artifacts
        .iter()
        .enumerate()
        .map(|(index, artifact)| (&artifact.artifact_key, index))
        .collect();
    let mut task_evidence = Vec::with_capacity(plan.scope.len());
    for scope in &plan.scope {
        let criterion_order: HashMap<&Handle, usize> = scope
            .criterion_refs
            .iter()
            .enumerate()
            .map(|(index, criterion)| (criterion, index))
            .collect();
        let mut criteria = BTreeMap::new();
        let mut keys = BTreeMap::new();
        let mut inputs = BTreeSet::new();
        let mut explanations = Vec::new();
        let mut found = false;
        for contribution in &contributions {
            for item in contribution
                .evidence
                .iter()
                .filter(|item| item.task_ref == scope.task_ref)
            {
                found = true;
                for criterion in &item.criterion_refs {
                    let Some(position) = criterion_order.get(criterion) else {
                        return Err(rejection(
                            ErrorCode::EvidenceUnresolved,
                            format!(
                                "Evidence for `{}` cites criterion `{criterion}` outside the task.",
                                scope.task_ref
                            ),
                        ));
                    };
                    criteria.insert(*position, criterion.clone());
                }
                for key in &item.artifact_keys {
                    // A unit cites its own artifacts; host-synthesized
                    // evidence cites the host-rendered documents (origin
                    // `None`), which every unit of the stage may reference.
                    let origin = (
                        Some(contribution.unit.logical_unit_key.clone()),
                        key.clone(),
                    );
                    let Some(mapped) = remap
                        .get(&origin)
                        .or_else(|| remap.get(&(None, key.clone())))
                    else {
                        return Err(rejection(
                            ErrorCode::EvidenceUnresolved,
                            format!(
                                "Evidence cites artifact key `{key}` that unit `{}` did not produce.",
                                contribution.unit.logical_unit_key
                            ),
                        ));
                    };
                    keys.insert(artifact_order[mapped], mapped.clone());
                }
                inputs.extend(item.input_refs.iter().cloned());
                explanations.push(item.explanation.trim().to_owned());
            }
        }
        if !found {
            return Err(rejection(
                ErrorCode::EvidenceUnresolved,
                format!(
                    "Task `{}` has no evidence in the aggregate.",
                    scope.task_ref
                ),
            )
            .with_related([scope.task_ref.clone()]));
        }
        let mut explanation = explanations.join(" ");
        if scalar_len(&explanation) > 4_000 {
            explanation = explanation.chars().take(3_997).collect::<String>() + "...";
        }
        task_evidence.push(EvidenceItemV1 {
            task_ref: scope.task_ref.clone(),
            criterion_refs: criteria.into_values().collect(),
            artifact_keys: keys.into_values().collect(),
            input_refs: inputs.into_iter().collect(),
            explanation,
        });
    }
    let aggregate = StageAggregateV1 {
        schema_version: SchemaV1,
        stage_execution_id: plan.stage_execution_id.clone(),
        stage: plan.stage,
        plan_sha256: plan.plan_sha256.clone(),
        artifacts,
        task_evidence,
        validation_receipt_ids,
        findings,
    };
    aggregate.validate()?;
    for artifact in &aggregate.artifacts {
        artifact.check_content_identity()?;
    }
    let _ = table;
    Ok(aggregate)
}

pub fn aggregate_sha256(aggregate: &StageAggregateV1) -> Result<Sha256Hex, CompositionError> {
    canonical_sha256(aggregate, &[])
}

/// The legacy acceptance inputs derived from an aggregate: a completed stage
/// outcome and the normalized per-task evidence rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyAcceptance {
    pub outcome: StageOutcome,
    pub evidence: Vec<TaskEvidence>,
}

/// Lowers an aggregate to the legacy shapes. `extra_rows` are host bookkeeping
/// artifacts (typed payloads) appended after the aggregate artifacts so they
/// never shift evidence positions.
pub fn to_legacy(
    aggregate: &StageAggregateV1,
    table: &ReferenceTable,
    summary: String,
    extra_rows: Vec<StageArtifactDraft>,
) -> Result<LegacyAcceptance, CompositionError> {
    let positions: HashMap<&Handle, usize> = aggregate
        .artifacts
        .iter()
        .enumerate()
        .map(|(index, artifact)| (&artifact.artifact_key, index))
        .collect();
    let mut artifacts = Vec::with_capacity(aggregate.artifacts.len() + extra_rows.len());
    for artifact in &aggregate.artifacts {
        let name = match &artifact.relative_path {
            Some(path) => path.as_str().to_owned(),
            None => format!("{} {}", legacy_kind(artifact.kind), artifact.artifact_key),
        };
        artifacts.push(StageArtifactDraft {
            name,
            kind: legacy_kind(artifact.kind).to_owned(),
            relative_path: artifact
                .relative_path
                .as_ref()
                .map(|path| RelativeSourcePath::new(path.as_str()))
                .transpose()
                .map_err(|error| rejection(ErrorCode::HostContractError, error.to_string()))?,
            content: artifact.content.clone(),
        });
    }
    artifacts.extend(extra_rows);
    let mut evidence = Vec::with_capacity(aggregate.task_evidence.len());
    for item in &aggregate.task_evidence {
        let task = table.task(&item.task_ref).ok_or_else(|| {
            rejection(
                ErrorCode::HostContractError,
                format!("Task `{}` is not in the reference table.", item.task_ref),
            )
        })?;
        let mut criterion_ids = Vec::with_capacity(item.criterion_refs.len());
        for criterion in &item.criterion_refs {
            let binding = table.criterion(criterion).ok_or_else(|| {
                rejection(
                    ErrorCode::HostContractError,
                    format!("Criterion `{criterion}` is not in the reference table."),
                )
            })?;
            criterion_ids.push(binding.criterion_id.clone());
        }
        let mut artifact_positions = item
            .artifact_keys
            .iter()
            .map(|key| positions.get(key).copied())
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| {
                rejection(
                    ErrorCode::EvidenceUnresolved,
                    "Evidence cites an artifact outside the aggregate.",
                )
            })?;
        artifact_positions.sort_unstable();
        artifact_positions.dedup();
        evidence.push(TaskEvidence {
            task_id: task.task_id.clone(),
            criterion_ids,
            artifact_positions,
            summary: item.explanation.clone(),
        });
    }
    Ok(LegacyAcceptance {
        outcome: StageOutcome::Completed {
            summary,
            notes: aggregate.findings.clone(),
            artifacts,
        },
        evidence,
    })
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use super::*;

    fn h(text: &str) -> Handle {
        Handle::new(text).unwrap()
    }

    fn uuid(n: u8) -> Uuid {
        Uuid::new(format!("10000000-0000-4000-8000-0000000000{n:02}")).unwrap()
    }

    fn fixture() -> (StageExecutionPlanV1, ReferenceTable) {
        let mut builder = ReferenceTable::builder();
        builder
            .add_task(
                "t-1",
                1,
                "One",
                &[("AC-1".into(), "a".into()), ("AC-2".into(), "b".into())],
            )
            .unwrap();
        builder
            .add_task("t-2", 1, "Two", &[("AC-1".into(), "c".into())])
            .unwrap();
        builder
            .add_existing(
                RelPath::new("src/a.rs").unwrap(),
                Sha256Hex::of(b"a"),
                SourceRole::Source,
            )
            .unwrap();
        builder
            .add_existing(
                RelPath::new("src/b.rs").unwrap(),
                Sha256Hex::of(b"b"),
                SourceRole::Source,
            )
            .unwrap();
        let table = builder.build();
        let unit = |key: &str, criteria: &[&str], owned: &str| WorkUnitSpecV1 {
            schema_version: SchemaV1,
            unit_id: Uuid::generate(),
            logical_unit_key: h(key),
            stage: Stage::Code,
            executor_kind: ExecutorKind::CodeImplementer,
            task_refs: vec![h("task_1"), h("task_2")],
            criterion_refs: criteria.iter().map(|c| h(c)).collect(),
            dependencies: vec![],
            input_refs: vec![],
            owned_destination_refs: vec![h(owned)],
            required_check_refs: vec![],
            objective: "Implement.".into(),
            max_output_tokens: 100,
        };
        let plan = StageExecutionPlanV1 {
            schema_version: SchemaV1,
            stage_execution_id: uuid(1),
            budget_account_id: uuid(2),
            parent_fence: ParentFenceV1 {
                project_id: uuid(3),
                pulsation_id: uuid(4),
                input_revision: 1,
                job_id: uuid(5),
                generation: 1,
                state_version: 1,
            },
            stage: Stage::Code,
            recipe_version: 1,
            policy_sha256: Sha256Hex::of(b"p"),
            batch_sha256: Sha256Hex::of(b"b"),
            approved_contract_sha256: None,
            plan_sha256: Sha256Hex::of(b"plan"),
            scope: table.scope().to_vec(),
            references: table.entries().to_vec(),
            anchors: vec![],
            check_refs: vec![],
            units: vec![
                unit("alpha", &["crit_1_1", "crit_2_1"], "src_1"),
                unit("beta", &["crit_1_2", "crit_2_1"], "src_2"),
            ],
            profile_bindings: vec![],
            budget: StageBudgetV1 {
                initial_invocation_limit: 2,
                extra_invocation_limit: 2,
                per_unit_extra_invocation_limit: 1,
                initial_output_token_limit: 200,
                extra_output_token_limit: 200,
                initial_prompt_character_limit: 64_000,
                extra_prompt_character_limit: 64_000,
                max_in_flight_units: 1,
            },
        };
        (plan, table)
    }

    fn contribution<'a>(
        unit: &'a WorkUnitSpecV1,
        path: &str,
        evidence: Vec<(&str, &[&str], &[&str])>,
    ) -> UnitContribution<'a> {
        UnitContribution {
            unit,
            evidence: evidence
                .into_iter()
                .map(|(task, criteria, keys)| EvidenceItemV1 {
                    task_ref: h(task),
                    criterion_refs: criteria.iter().map(|c| h(c)).collect(),
                    artifact_keys: keys.iter().map(|k| h(k)).collect(),
                    input_refs: vec![],
                    explanation: format!("{} done.", unit.logical_unit_key),
                })
                .collect(),
            artifacts: vec![ReconstructedArtifact {
                artifact_key: h("out"),
                relative_path: Some(RelPath::new(path).unwrap()),
                kind: ArtifactKind::Code,
                content: format!("// {}\n", unit.logical_unit_key),
            }],
        }
    }

    #[test]
    fn completion_order_does_not_change_the_aggregate_or_its_digest() {
        let (plan, table) = fixture();
        let alpha = contribution(
            &plan.units[0],
            "src/a.rs",
            vec![
                ("task_1", &["crit_1_1"], &["out"]),
                ("task_2", &["crit_2_1"], &["out"]),
            ],
        );
        let beta = contribution(
            &plan.units[1],
            "src/b.rs",
            vec![
                ("task_1", &["crit_1_2"], &["out"]),
                ("task_2", &["crit_2_1"], &["out"]),
            ],
        );
        let host = || {
            vec![HostArtifact {
                artifact_key: h("report"),
                relative_path: None,
                kind: ArtifactKind::ValidationReport,
                content: "# Report\n".into(),
            }]
        };
        let first = build_aggregate(
            &plan,
            &table,
            &[alpha.clone(), beta.clone()],
            host(),
            vec![],
            vec!["ok".into()],
        )
        .unwrap();
        let second = build_aggregate(
            &plan,
            &table,
            &[beta, alpha],
            host(),
            vec![],
            vec!["ok".into()],
        )
        .unwrap();
        assert_eq!(first, second);
        assert_eq!(
            aggregate_sha256(&first).unwrap(),
            aggregate_sha256(&second).unwrap()
        );
        assert_eq!(
            first
                .artifacts
                .iter()
                .map(|a| a.artifact_key.as_str())
                .collect::<Vec<_>>(),
            ["art_1", "art_2", "art_3"]
        );
        assert_eq!(
            first.artifacts[0].relative_path.as_ref().unwrap().as_str(),
            "src/a.rs"
        );
        assert_eq!(first.artifacts[2].kind, ArtifactKind::ValidationReport);
        // One evidence row per task, criteria in scope order, keys in artifact order.
        assert_eq!(first.task_evidence.len(), 2);
        assert_eq!(
            first.task_evidence[0].criterion_refs,
            vec![h("crit_1_1"), h("crit_1_2")]
        );
        assert_eq!(
            first.task_evidence[0].artifact_keys,
            vec![h("art_1"), h("art_2")]
        );
        assert_eq!(first.task_evidence[1].criterion_refs, vec![h("crit_2_1")]);
        let legacy = to_legacy(
            &first,
            &table,
            "Done.".into(),
            vec![StageArtifactDraft {
                name: "payload".into(),
                kind: "stage_payload_v1".into(),
                relative_path: None,
                content: "{}".into(),
            }],
        )
        .unwrap();
        let StageOutcome::Completed { artifacts, .. } = &legacy.outcome else {
            panic!()
        };
        assert_eq!(artifacts.len(), 4);
        assert_eq!(artifacts[0].kind, "code");
        assert_eq!(artifacts[3].kind, "stage_payload_v1");
        assert_eq!(legacy.evidence[0].task_id, "t-1");
        assert_eq!(legacy.evidence[0].criterion_ids, vec!["AC-1", "AC-2"]);
        assert_eq!(legacy.evidence[0].artifact_positions, vec![0, 1]);
        assert_eq!(legacy.evidence[1].task_id, "t-2");
    }

    #[test]
    fn missing_task_evidence_unknown_keys_and_duplicate_destinations_are_rejected() {
        let (plan, table) = fixture();
        let alpha = contribution(
            &plan.units[0],
            "src/a.rs",
            vec![("task_1", &["crit_1_1"], &["out"])],
        );
        let error = build_aggregate(
            &plan,
            &table,
            std::slice::from_ref(&alpha),
            vec![],
            vec![],
            vec![],
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::EvidenceUnresolved);
        assert_eq!(error.related, vec![h("task_2")]);
        let ghost = contribution(
            &plan.units[0],
            "src/a.rs",
            vec![
                ("task_1", &["crit_1_1"], &["ghost"]),
                ("task_2", &["crit_2_1"], &[]),
            ],
        );
        assert_eq!(
            build_aggregate(&plan, &table, &[ghost], vec![], vec![], vec![])
                .unwrap_err()
                .code,
            ErrorCode::EvidenceUnresolved
        );
        let wrong_task = contribution(
            &plan.units[0],
            "src/a.rs",
            vec![
                ("task_1", &["crit_2_1"], &[]),
                ("task_2", &["crit_2_1"], &[]),
            ],
        );
        assert!(build_aggregate(&plan, &table, &[wrong_task], vec![], vec![], vec![]).is_err());
        let beta = contribution(
            &plan.units[1],
            "SRC/A.RS",
            vec![
                ("task_1", &["crit_1_2"], &["out"]),
                ("task_2", &["crit_2_1"], &["out"]),
            ],
        );
        assert_eq!(
            build_aggregate(&plan, &table, &[alpha, beta], vec![], vec![], vec![])
                .unwrap_err()
                .code,
            ErrorCode::OwnershipViolation
        );
    }
}
