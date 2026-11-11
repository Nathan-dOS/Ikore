//! Freezing a `StageExecutionPlanV1` (SC-006).
//!
//! A recipe proposes unit drafts with local keys; the host assigns unit IDs,
//! resolves dependencies, checks ownership, coverage and capacity, orders the
//! units topologically and digests the result. Everything is validated before
//! any reservation or request exists. M2 recipes produce one unit; the same
//! admission rules govern the multi-unit plans of M3.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use super::budget::{UnitEstimate, derive_budget};
use super::references::ReferenceTable;
use super::{
    CompositionError, ErrorCode, ExecutorKind, Handle, Invariant, MAX_UNITS_PER_PLAN,
    ParentFenceV1, ProfileBindingV1, RECIPE_VERSION, ReferenceEntryV1, Sha256Hex, Stage,
    StageExecutionPlanV1, Uuid, Validate, WorkUnitSpecV1, canonical_sha256,
};
use crate::orchestration::SchemaV1;

/// A recipe's proposal for one work unit, before the host freezes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitDraft {
    pub logical_unit_key: Handle,
    pub executor_kind: ExecutorKind,
    pub task_refs: Vec<Handle>,
    pub criterion_refs: Vec<Handle>,
    /// Logical keys of other drafts this unit consumes.
    pub depends_on: Vec<Handle>,
    pub input_refs: Vec<Handle>,
    pub owned_destination_refs: Vec<Handle>,
    pub required_check_refs: Vec<Handle>,
    pub objective: String,
    pub estimated_output_tokens: u32,
    /// Pre-assigned identity (the planner mints one when candidate references
    /// must name their producer before the plan is frozen); `None` lets the
    /// host generate it.
    pub unit_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct PlanInputs<'a> {
    pub stage_execution_id: Uuid,
    pub budget_account_id: Uuid,
    pub parent_fence: ParentFenceV1,
    pub stage: Stage,
    pub policy_sha256: Sha256Hex,
    pub batch_sha256: Sha256Hex,
    pub approved_contract_sha256: Option<Sha256Hex>,
    pub table: &'a ReferenceTable,
    pub units: Vec<UnitDraft>,
    pub stage_output_limit: u32,
    pub context_chars_limit: usize,
    pub max_in_flight_units: u32,
    /// Profile snapshot per executor kind (initial and repair specialties).
    pub profile_snapshots: BTreeMap<ExecutorKind, Uuid>,
}

fn admission(code: ErrorCode, message: impl Into<String>) -> CompositionError {
    CompositionError::new(Invariant::Sc006, code, message)
}

/// Freezes and validates a plan. Units are emitted in a deterministic
/// topological order (dependencies first, then draft order).
pub fn freeze_plan(inputs: PlanInputs<'_>) -> Result<StageExecutionPlanV1, CompositionError> {
    let table = inputs.table;
    if inputs.units.len() > MAX_UNITS_PER_PLAN {
        return Err(admission(
            ErrorCode::CapacityExceeded,
            format!(
                "A plan admits at most {MAX_UNITS_PER_PLAN} units; got {}.",
                inputs.units.len()
            ),
        ));
    }
    let mut keys = HashSet::new();
    for draft in &inputs.units {
        if !keys.insert(&draft.logical_unit_key) {
            return Err(admission(
                ErrorCode::HostContractError,
                format!("Unit key `{}` is proposed twice.", draft.logical_unit_key),
            ));
        }
        if draft.executor_kind.stage() != inputs.stage || draft.executor_kind.is_repair() {
            return Err(admission(
                ErrorCode::HostContractError,
                format!(
                    "Unit `{}` uses `{}`, which is not an initial specialty of `{}`.",
                    draft.logical_unit_key,
                    draft.executor_kind.as_str(),
                    inputs.stage.as_str()
                ),
            ));
        }
    }
    // Reference identity: every handle resolves in the frozen table.
    for draft in &inputs.units {
        for task in &draft.task_refs {
            if table.task(task).is_none() {
                return Err(admission(
                    ErrorCode::ReferenceConflict,
                    format!(
                        "Unit `{}` cites unknown task `{task}`.",
                        draft.logical_unit_key
                    ),
                ));
            }
        }
        for criterion in &draft.criterion_refs {
            let Some(binding) = table.criterion(criterion) else {
                return Err(admission(
                    ErrorCode::ReferenceConflict,
                    format!(
                        "Unit `{}` cites unknown criterion `{criterion}`.",
                        draft.logical_unit_key
                    ),
                ));
            };
            if !draft.task_refs.contains(&binding.task) {
                return Err(admission(
                    ErrorCode::ReferenceConflict,
                    format!(
                        "Unit `{}` cites criterion `{criterion}` of a task it does not own.",
                        draft.logical_unit_key
                    ),
                ));
            }
        }
        for input in &draft.input_refs {
            if table.entry(input).is_none() {
                return Err(admission(
                    ErrorCode::ReferenceConflict,
                    format!(
                        "Unit `{}` cites unknown input `{input}`.",
                        draft.logical_unit_key
                    ),
                ));
            }
        }
        for owned in &draft.owned_destination_refs {
            let Some(entry) = table.entry(owned) else {
                return Err(admission(
                    ErrorCode::ReferenceConflict,
                    format!(
                        "Unit `{}` owns unknown destination `{owned}`.",
                        draft.logical_unit_key
                    ),
                ));
            };
            if !entry.is_writable_destination() {
                return Err(admission(
                    ErrorCode::OwnershipViolation,
                    format!(
                        "Unit `{}` owns `{owned}`, which is not writable.",
                        draft.logical_unit_key
                    ),
                ));
            }
        }
        for check in &draft.required_check_refs {
            if !table.has_check(check) {
                return Err(admission(
                    ErrorCode::ReferenceConflict,
                    format!(
                        "Unit `{}` requires unknown check `{check}`.",
                        draft.logical_unit_key
                    ),
                ));
            }
        }
        for dependency in &draft.depends_on {
            if dependency == &draft.logical_unit_key || !keys.contains(dependency) {
                return Err(admission(
                    ErrorCode::ReferenceConflict,
                    format!(
                        "Unit `{}` depends on `{dependency}`, which is not another unit.",
                        draft.logical_unit_key
                    ),
                ));
            }
        }
    }
    // File-level ownership is exclusive under Windows identity (SC-006 step 1).
    let mut owners: HashMap<String, &Handle> = HashMap::new();
    for draft in &inputs.units {
        for owned in &draft.owned_destination_refs {
            let key = table
                .entry(owned)
                .and_then(ReferenceEntryV1::relative_path)
                .map(|path| path.canonical_key())
                .unwrap_or_else(|| owned.as_str().to_owned());
            if let Some(previous) = owners.insert(key, &draft.logical_unit_key)
                && previous != &draft.logical_unit_key
            {
                return Err(admission(
                    ErrorCode::OwnershipViolation,
                    format!(
                        "Destination `{owned}` is owned by both `{previous}` and `{}`; same-file writers must be one unit.",
                        draft.logical_unit_key
                    ),
                ));
            }
        }
    }
    // Code and Test plans must cover every criterion of the scope.
    if matches!(inputs.stage, Stage::Code | Stage::Test) && !inputs.units.is_empty() {
        let covered = inputs
            .units
            .iter()
            .flat_map(|draft| draft.criterion_refs.iter())
            .collect::<HashSet<_>>();
        for scope in table.scope() {
            if let Some(missing) = scope.criterion_refs.iter().find(|c| !covered.contains(c)) {
                return Err(admission(
                    ErrorCode::ScopeInsufficient,
                    format!("No unit covers criterion `{missing}`."),
                )
                .with_related([missing.clone()]));
            }
        }
    }
    // A candidate input names its producer, which must be a predecessor of
    // the citing unit (SC-004: a candidate requires a dependency edge).
    let ids: BTreeMap<&Handle, Uuid> = inputs
        .units
        .iter()
        .map(|draft| {
            (
                &draft.logical_unit_key,
                draft.unit_id.clone().unwrap_or_else(Uuid::generate),
            )
        })
        .collect();
    let key_of_id: HashMap<&Uuid, &Handle> = ids.iter().map(|(key, id)| (id, *key)).collect();
    for draft in &inputs.units {
        for input in &draft.input_refs {
            let Some(ReferenceEntryV1::CandidateSource {
                producer_unit_id, ..
            }) = table.entry(input)
            else {
                continue;
            };
            let Some(producer) = key_of_id.get(producer_unit_id) else {
                return Err(admission(
                    ErrorCode::ReferenceConflict,
                    format!(
                        "Unit `{}` reads candidate `{input}` whose producer is not in the plan.",
                        draft.logical_unit_key
                    ),
                ));
            };
            if !depends_transitively(&inputs.units, &draft.logical_unit_key, producer) {
                return Err(admission(
                    ErrorCode::ReferenceConflict,
                    format!(
                        "Unit `{}` reads candidate `{input}` without depending on its producer `{producer}`.",
                        draft.logical_unit_key
                    ),
                ));
            }
        }
    }
    let order = topological_order(&inputs.units)?;
    let (budget, reservations) = derive_budget(
        inputs.stage_output_limit,
        inputs.context_chars_limit,
        &inputs
            .units
            .iter()
            .map(|draft| UnitEstimate {
                logical_unit_key: draft.logical_unit_key.clone(),
                estimated_output_tokens: draft.estimated_output_tokens,
            })
            .collect::<Vec<_>>(),
        inputs.max_in_flight_units,
    )?;
    let mut units = Vec::with_capacity(inputs.units.len());
    let mut bindings = Vec::new();
    for index in order {
        let draft = &inputs.units[index];
        let unit_id = ids[&draft.logical_unit_key].clone();
        units.push(WorkUnitSpecV1 {
            schema_version: SchemaV1,
            unit_id: unit_id.clone(),
            logical_unit_key: draft.logical_unit_key.clone(),
            stage: inputs.stage,
            executor_kind: draft.executor_kind,
            task_refs: draft.task_refs.clone(),
            criterion_refs: draft.criterion_refs.clone(),
            dependencies: draft
                .depends_on
                .iter()
                .map(|key| ids[key].clone())
                .collect(),
            input_refs: draft.input_refs.clone(),
            owned_destination_refs: draft.owned_destination_refs.clone(),
            required_check_refs: draft.required_check_refs.clone(),
            objective: draft.objective.clone(),
            max_output_tokens: reservations[index],
        });
        let mut kinds = vec![draft.executor_kind];
        if let Some(repair) = repair_kind(draft.executor_kind) {
            kinds.push(repair);
        }
        for kind in kinds {
            if let Some(snapshot) = inputs.profile_snapshots.get(&kind) {
                bindings.push(ProfileBindingV1 {
                    unit_id: unit_id.clone(),
                    executor_kind: kind,
                    profile_snapshot_id: snapshot.clone(),
                });
            }
        }
    }
    let mut plan = StageExecutionPlanV1 {
        schema_version: SchemaV1,
        stage_execution_id: inputs.stage_execution_id,
        budget_account_id: inputs.budget_account_id,
        parent_fence: inputs.parent_fence,
        stage: inputs.stage,
        recipe_version: RECIPE_VERSION,
        policy_sha256: inputs.policy_sha256,
        batch_sha256: inputs.batch_sha256,
        approved_contract_sha256: inputs.approved_contract_sha256,
        plan_sha256: Sha256Hex::of(b"pending"),
        scope: table.scope().to_vec(),
        references: table.entries().to_vec(),
        anchors: table.anchors().to_vec(),
        check_refs: table.checks().to_vec(),
        units,
        profile_bindings: bindings,
        budget,
    };
    plan.plan_sha256 = canonical_sha256(&plan, &["planSha256"])?;
    plan.validate()?;
    Ok(plan)
}

/// The correction specialty admitted for an initial one.
pub const fn repair_kind(kind: ExecutorKind) -> Option<ExecutorKind> {
    match kind {
        ExecutorKind::CodeImplementer => Some(ExecutorKind::CodeRepairer),
        ExecutorKind::TestAuthor => Some(ExecutorKind::TestRepairer),
        // These specialties correct themselves within the same allowance.
        ExecutorKind::IntakeClassifier
        | ExecutorKind::SpecificationAuthor
        | ExecutorKind::ContractAuthor
        | ExecutorKind::IntegrationReviewer => Some(kind),
        ExecutorKind::CodeRepairer | ExecutorKind::TestRepairer => None,
    }
}

/// Whether `from` reaches `to` through `depends_on` edges.
fn depends_transitively(drafts: &[UnitDraft], from: &Handle, to: &Handle) -> bool {
    let by_key: HashMap<&Handle, &UnitDraft> =
        drafts.iter().map(|d| (&d.logical_unit_key, d)).collect();
    let mut seen = HashSet::new();
    let mut stack = vec![from];
    while let Some(key) = stack.pop() {
        if !seen.insert(key) {
            continue;
        }
        if let Some(draft) = by_key.get(key) {
            for dependency in &draft.depends_on {
                if dependency == to {
                    return true;
                }
                stack.push(dependency);
            }
        }
    }
    false
}

/// Kahn ordering with stable tie-breaking on draft position; cycles fail.
fn topological_order(units: &[UnitDraft]) -> Result<Vec<usize>, CompositionError> {
    let index_of: HashMap<&Handle, usize> = units
        .iter()
        .enumerate()
        .map(|(index, draft)| (&draft.logical_unit_key, index))
        .collect();
    let mut indegree = vec![0usize; units.len()];
    let mut successors: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); units.len()];
    for (index, draft) in units.iter().enumerate() {
        for dependency in &draft.depends_on {
            let predecessor = index_of[dependency];
            if successors[predecessor].insert(index) {
                indegree[index] += 1;
            }
        }
    }
    let mut ready: BTreeSet<usize> = (0..units.len()).filter(|i| indegree[*i] == 0).collect();
    let mut order = Vec::with_capacity(units.len());
    while let Some(&next) = ready.iter().next() {
        ready.remove(&next);
        order.push(next);
        for &successor in &successors[next] {
            indegree[successor] -= 1;
            if indegree[successor] == 0 {
                ready.insert(successor);
            }
        }
    }
    if order.len() != units.len() {
        return Err(admission(
            ErrorCode::ReferenceConflict,
            "Unit dependencies form a cycle; an inseparable cycle must be one unit.",
        ));
    }
    Ok(order)
}

/// Verifies a frozen plan's digest against its content (SC-003).
pub fn verify_plan_digest(plan: &StageExecutionPlanV1) -> Result<(), CompositionError> {
    let expected = canonical_sha256(plan, &["planSha256"])?;
    if expected != plan.plan_sha256 {
        return Err(CompositionError::new(
            Invariant::Sc003,
            ErrorCode::HostContractError,
            "The persisted plan does not match its digest.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::composition::{RelPath, SourceRole};

    fn h(text: &str) -> Handle {
        Handle::new(text).unwrap()
    }

    fn table() -> ReferenceTable {
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
        builder.add_check(h("cargo_check")).unwrap();
        builder.build()
    }

    fn draft(key: &str, criteria: &[&str], owned: &[&str], depends: &[&str]) -> UnitDraft {
        UnitDraft {
            logical_unit_key: h(key),
            executor_kind: ExecutorKind::CodeImplementer,
            task_refs: vec![h("task_1")],
            criterion_refs: criteria.iter().map(|c| h(c)).collect(),
            depends_on: depends.iter().map(|d| h(d)).collect(),
            input_refs: vec![h("src_1")],
            owned_destination_refs: owned.iter().map(|o| h(o)).collect(),
            required_check_refs: vec![h("cargo_check")],
            objective: "Implement.".into(),
            estimated_output_tokens: 500,
            unit_id: None,
        }
    }

    fn inputs<'a>(table: &'a ReferenceTable, units: Vec<UnitDraft>) -> PlanInputs<'a> {
        let mut snapshots = BTreeMap::new();
        snapshots.insert(ExecutorKind::CodeImplementer, Uuid::generate());
        snapshots.insert(ExecutorKind::CodeRepairer, Uuid::generate());
        PlanInputs {
            stage_execution_id: Uuid::generate(),
            budget_account_id: Uuid::generate(),
            parent_fence: ParentFenceV1 {
                project_id: Uuid::generate(),
                pulsation_id: Uuid::generate(),
                input_revision: 1,
                job_id: Uuid::generate(),
                generation: 1,
                state_version: 1,
            },
            stage: Stage::Code,
            policy_sha256: Sha256Hex::of(b"policy"),
            batch_sha256: Sha256Hex::of(b"batch"),
            approved_contract_sha256: None,
            table,
            units,
            stage_output_limit: 16_000,
            context_chars_limit: 16_000,
            max_in_flight_units: 1,
            profile_snapshots: snapshots,
        }
    }

    #[test]
    fn a_plan_is_frozen_in_dependency_order_with_digest_and_bindings() {
        let table = table();
        let plan = freeze_plan(inputs(
            &table,
            vec![
                draft("second", &["crit_1_2"], &["src_2"], &["first"]),
                draft("first", &["crit_1_1"], &["src_1"], &[]),
            ],
        ))
        .unwrap();
        assert_eq!(plan.units[0].logical_unit_key, h("first"));
        assert_eq!(plan.units[1].logical_unit_key, h("second"));
        assert_eq!(
            plan.units[1].dependencies,
            vec![plan.units[0].unit_id.clone()]
        );
        assert!(plan.units.iter().map(|u| u.max_output_tokens).sum::<u32>() <= 16_000);
        assert_eq!(plan.profile_bindings.len(), 4);
        verify_plan_digest(&plan).unwrap();
        let mut tampered = plan.clone();
        tampered.units[0].objective = "Changed.".into();
        assert!(verify_plan_digest(&tampered).is_err());
        assert_eq!(plan.budget.initial_invocation_limit, 2);
    }

    #[test]
    fn ownership_cycles_coverage_and_specialties_are_admitted_before_dispatch() {
        let table = table();
        // Same-file writers must be one unit.
        assert_eq!(
            freeze_plan(inputs(
                &table,
                vec![
                    draft("a", &["crit_1_1"], &["src_1"], &[]),
                    draft("b", &["crit_1_2"], &["src_1"], &[])
                ]
            ))
            .unwrap_err()
            .code,
            ErrorCode::OwnershipViolation
        );
        // Cycle.
        assert_eq!(
            freeze_plan(inputs(
                &table,
                vec![
                    draft("a", &["crit_1_1"], &["src_1"], &["b"]),
                    draft("b", &["crit_1_2"], &["src_2"], &["a"])
                ]
            ))
            .unwrap_err()
            .code,
            ErrorCode::ReferenceConflict
        );
        // Missing criterion coverage for Code.
        assert_eq!(
            freeze_plan(inputs(
                &table,
                vec![draft("a", &["crit_1_1"], &["src_1"], &[])]
            ))
            .unwrap_err()
            .code,
            ErrorCode::ScopeInsufficient
        );
        // Repair specialty cannot be an initial unit.
        let mut repair = draft("a", &["crit_1_1", "crit_1_2"], &["src_1"], &[]);
        repair.executor_kind = ExecutorKind::CodeRepairer;
        assert!(freeze_plan(inputs(&table, vec![repair])).is_err());
        // Unknown handle.
        let mut unknown = draft("a", &["crit_1_1", "crit_1_2"], &["src_9"], &[]);
        unknown.owned_destination_refs = vec![h("src_9")];
        assert!(freeze_plan(inputs(&table, vec![unknown])).is_err());
        // Over capacity.
        let mut huge = draft("a", &["crit_1_1", "crit_1_2"], &["src_1"], &[]);
        huge.estimated_output_tokens = 20_000;
        assert_eq!(
            freeze_plan(inputs(&table, vec![huge])).unwrap_err().code,
            ErrorCode::CapacityExceeded
        );
        // An empty plan (host-only outcome) is admissible.
        let empty = freeze_plan(inputs(&table, vec![])).unwrap();
        assert!(empty.units.is_empty());
        assert_eq!(empty.budget.initial_invocation_limit, 0);
    }
}
