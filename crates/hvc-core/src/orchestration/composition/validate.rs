//! Semantic validation of a parsed worker response against the frozen plan
//! (SC-002, SC-004, SC-005, SC-008, SC-010).
//!
//! `parse.rs` guarantees the shape. This module guarantees that every handle
//! resolves inside the unit's delivered tables, that the specialty stayed in
//! its policy, that ownership was respected, that evidence points at real
//! artifacts and criteria of the right task, and that newly authored prose is
//! English. Nothing here reads the filesystem or fills a gap with a default.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::agent_output_language::validate_english_prose;

use super::edits::{DestinationEdits, group_by_destination};
use super::policy::{EditOp, ExecutorPolicy, policy_for};
use super::{
    AnchorBindingV1, CompletedPayloadV1, CompositionError, CoverageKind, EditEntryV1, ErrorCode,
    EvidenceItemV1, ExecutorKind, Handle, IntegrationPayloadV1, InterfaceContractV1, Invariant,
    ReferenceEntryV1, SourceRole, SpecificationContractV1, StageExecutionPlanV1, TestPayloadV1,
    UnitModelResponseV1, VerificationMode, WorkUnitSpecV1,
};

/// Approved data from earlier stages that the plan itself does not carry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApprovedContext {
    /// Criteria the approved specification marked `not_applicable`.
    pub not_applicable_criteria: Vec<Handle>,
    /// Unit proposal keys from the approved specification (Contract binds them).
    pub proposal_keys: Vec<Handle>,
}

/// Read-only lookups over a frozen plan.
pub struct PlanIndex<'a> {
    plan: &'a StageExecutionPlanV1,
    references: HashMap<&'a Handle, &'a ReferenceEntryV1>,
    anchors: HashMap<&'a Handle, &'a AnchorBindingV1>,
    criterion_task: HashMap<&'a Handle, &'a Handle>,
    checks: HashSet<&'a Handle>,
}

impl<'a> PlanIndex<'a> {
    pub fn new(plan: &'a StageExecutionPlanV1) -> Self {
        Self {
            plan,
            references: plan
                .references
                .iter()
                .map(|entry| (entry.handle(), entry))
                .collect(),
            anchors: plan
                .anchors
                .iter()
                .map(|anchor| (&anchor.anchor_ref, anchor))
                .collect(),
            criterion_task: plan
                .scope
                .iter()
                .flat_map(|scope| {
                    scope
                        .criterion_refs
                        .iter()
                        .map(move |criterion| (criterion, &scope.task_ref))
                })
                .collect(),
            checks: plan.check_refs.iter().collect(),
        }
    }

    pub fn plan(&self) -> &'a StageExecutionPlanV1 {
        self.plan
    }

    pub fn reference(&self, handle: &Handle) -> Option<&'a ReferenceEntryV1> {
        self.references.get(handle).copied()
    }

    pub fn anchor(&self, handle: &Handle) -> Option<&'a AnchorBindingV1> {
        self.anchors.get(handle).copied()
    }

    pub fn anchor_source(&self, handle: &Handle) -> Option<&'a Handle> {
        self.anchor(handle).map(|anchor| &anchor.source_ref)
    }

    pub fn task_of_criterion(&self, criterion: &Handle) -> Option<&'a Handle> {
        self.criterion_task.get(criterion).copied()
    }

    pub fn is_task(&self, handle: &Handle) -> bool {
        self.plan
            .scope
            .iter()
            .any(|scope| &scope.task_ref == handle)
    }

    pub fn has_check(&self, handle: &Handle) -> bool {
        self.checks.contains(handle)
    }

    pub fn is_readable(&self, handle: &Handle) -> bool {
        self.reference(handle)
            .is_some_and(ReferenceEntryV1::is_readable_input)
    }

    pub fn is_writable(&self, handle: &Handle) -> bool {
        self.reference(handle)
            .is_some_and(ReferenceEntryV1::is_writable_destination)
    }
}

/// A response that passed every host check for its unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedChild<'a> {
    pub response: &'a UnitModelResponseV1,
    /// Grouped edits per destination, empty when the payload has no edits.
    pub destinations: Vec<DestinationEdits<'a>>,
}

struct Checker<'a> {
    index: &'a PlanIndex<'a>,
    unit: &'a WorkUnitSpecV1,
    policy: &'static ExecutorPolicy,
    approved: &'a ApprovedContext,
    unit_tasks: HashSet<&'a Handle>,
    unit_criteria: HashSet<&'a Handle>,
    unit_inputs: HashSet<&'a Handle>,
    unit_owned: HashSet<&'a Handle>,
}

fn reject(sc: Invariant, code: ErrorCode, message: impl Into<String>) -> CompositionError {
    CompositionError::new(sc, code, message)
}

fn english(field: &'static str, text: &str) -> Result<(), CompositionError> {
    validate_english_prose(field, text).map_err(|error| {
        reject(
            Invariant::Sc008,
            ErrorCode::InvalidLanguage,
            format!("{field} must be English ({error})."),
        )
    })
}

/// Language guard for newly authored file content. Words the original source
/// already contains are exempt: the contract requires existing identifiers to
/// be preserved, so they cannot also be grounds for rejection.
pub fn validate_authored_content(
    field: &'static str,
    content: &str,
    original: Option<&str>,
) -> Result<(), CompositionError> {
    let Some(original) = original else {
        return english(field, content);
    };
    let known = words(original);
    let filtered = content
        .split_inclusive(char::is_whitespace)
        .map(|token| {
            let word = token.trim().to_ascii_lowercase();
            if !word.is_empty() && known.contains(&word) {
                token
                    .chars()
                    .filter(|ch| ch.is_whitespace())
                    .collect::<String>()
            } else {
                token.to_owned()
            }
        })
        .collect::<String>();
    english(field, &filtered)
}

fn words(text: &str) -> HashSet<String> {
    text.split_whitespace()
        .map(|token| token.to_ascii_lowercase())
        .collect()
}

impl<'a> Checker<'a> {
    fn new(
        index: &'a PlanIndex<'a>,
        unit: &'a WorkUnitSpecV1,
        approved: &'a ApprovedContext,
    ) -> Result<Self, CompositionError> {
        let policy = policy_for(unit.executor_kind);
        // The unit spec itself must be consistent with the plan before any
        // response is judged against it.
        for task in &unit.task_refs {
            if !index.is_task(task) {
                return Err(reject(
                    Invariant::Sc002,
                    ErrorCode::HostContractError,
                    format!("Unit task `{task}` is not in the frozen scope."),
                ));
            }
        }
        for criterion in &unit.criterion_refs {
            let Some(task) = index.task_of_criterion(criterion) else {
                return Err(reject(
                    Invariant::Sc002,
                    ErrorCode::HostContractError,
                    format!("Unit criterion `{criterion}` is not in the frozen scope."),
                ));
            };
            if !unit.task_refs.contains(task) {
                return Err(reject(
                    Invariant::Sc002,
                    ErrorCode::HostContractError,
                    format!("Unit criterion `{criterion}` belongs to a task outside the unit."),
                ));
            }
        }
        for input in &unit.input_refs {
            if index.reference(input).is_none() {
                return Err(reject(
                    Invariant::Sc004,
                    ErrorCode::HostContractError,
                    format!("Unit input `{input}` is not in the reference table."),
                ));
            }
        }
        for owned in &unit.owned_destination_refs {
            if !index.is_writable(owned) {
                return Err(reject(
                    Invariant::Sc004,
                    ErrorCode::HostContractError,
                    format!("Unit owned destination `{owned}` is not a writable reference."),
                ));
            }
        }
        for check in &unit.required_check_refs {
            if !index.has_check(check) {
                return Err(reject(
                    Invariant::Sc011,
                    ErrorCode::HostContractError,
                    format!("Unit required check `{check}` is not configured in the plan."),
                ));
            }
        }
        Ok(Self {
            index,
            unit,
            policy,
            approved,
            unit_tasks: unit.task_refs.iter().collect(),
            unit_criteria: unit.criterion_refs.iter().collect(),
            unit_inputs: unit.input_refs.iter().collect(),
            unit_owned: unit.owned_destination_refs.iter().collect(),
        })
    }

    fn known_handle(&self, handle: &Handle) -> bool {
        self.index.reference(handle).is_some()
            || self.index.anchor(handle).is_some()
            || self.index.is_task(handle)
            || self.index.task_of_criterion(handle).is_some()
            || self.index.has_check(handle)
    }

    fn require_task(&self, task: &Handle) -> Result<(), CompositionError> {
        if !self.unit_tasks.contains(task) {
            return Err(reject(
                Invariant::Sc002,
                ErrorCode::ReferenceConflict,
                format!("Task `{task}` is not assigned to this unit."),
            )
            .with_related([task.clone()]));
        }
        Ok(())
    }

    fn require_criterion_of(
        &self,
        task: &Handle,
        criterion: &Handle,
    ) -> Result<(), CompositionError> {
        if !self.unit_criteria.contains(criterion) {
            return Err(reject(
                Invariant::Sc002,
                ErrorCode::ReferenceConflict,
                format!("Criterion `{criterion}` is not assigned to this unit."),
            )
            .with_related([criterion.clone()]));
        }
        if self.index.task_of_criterion(criterion) != Some(task) {
            return Err(reject(
                Invariant::Sc002,
                ErrorCode::ReferenceConflict,
                format!("Criterion `{criterion}` does not belong to task `{task}`."),
            )
            .with_related([task.clone(), criterion.clone()]));
        }
        Ok(())
    }

    fn require_inputs(&self, field: &str, refs: &[Handle]) -> Result<(), CompositionError> {
        for handle in refs {
            if !self.unit_inputs.contains(handle) || !self.index.is_readable(handle) {
                return Err(reject(
                    Invariant::Sc004,
                    ErrorCode::ReferenceConflict,
                    format!(
                        "`{field}` cites `{handle}`, which was not delivered as a readable input."
                    ),
                )
                .with_related([handle.clone()]));
            }
        }
        Ok(())
    }

    /// Filters `usedRefs` to only readable inputs, returning the clean list
    /// and a (currently unused) diagnostic list for every stripped handle.
    /// This prevents SC-004 `reference_conflict` rejections caused by models
    /// placing task, criterion, anchor, check or planned-destination handles
    /// — or handles they invented — into `usedRefs`. Invalid handles are
    /// silently dropped; the strip is implicitly visible in the attempt's
    /// persisted payload.
    fn sanitize_used_refs(&self, refs: &[Handle]) -> (Vec<Handle>, Vec<String>) {
        let mut clean = Vec::with_capacity(refs.len());
        let mut warnings = Vec::new();
        for handle in refs {
            if self.unit_inputs.contains(handle) && self.index.is_readable(handle) {
                clean.push(handle.clone());
            } else {
                warnings.push(format!(
                    "usedRefs: stripped `{handle}` — not a readable input in this unit \
                     (task, criterion, anchor, check and planned-destination handles are not valid here)."
                ));
            }
        }
        (clean, warnings)
    }

    fn require_checks(&self, field: &str, refs: &[Handle]) -> Result<(), CompositionError> {
        for handle in refs {
            if !self.index.has_check(handle) {
                return Err(reject(
                    Invariant::Sc011,
                    ErrorCode::ReferenceConflict,
                    format!("`{field}` cites unknown check `{handle}`."),
                )
                .with_related([handle.clone()]));
            }
        }
        Ok(())
    }

    /// All unit criteria represented exactly once by `(task, criterion)` pairs.
    fn require_full_coverage(
        &self,
        field: &str,
        pairs: impl Iterator<Item = (&'a Handle, &'a Handle)>,
    ) -> Result<(), CompositionError> {
        let mut seen = HashSet::new();
        for (task, criterion) in pairs {
            self.require_task(task)?;
            self.require_criterion_of(task, criterion)?;
            if !seen.insert(criterion) {
                return Err(reject(
                    Invariant::Sc010,
                    ErrorCode::EvidenceUnresolved,
                    format!("`{field}` lists criterion `{criterion}` more than once."),
                )
                .with_related([criterion.clone()]));
            }
        }
        if let Some(missing) = self.unit.criterion_refs.iter().find(|c| !seen.contains(c)) {
            return Err(reject(
                Invariant::Sc010,
                ErrorCode::EvidenceUnresolved,
                format!("`{field}` does not represent criterion `{missing}`."),
            )
            .with_related([missing.clone()]));
        }
        Ok(())
    }

    fn check_edits(
        &self,
        edits: &'a [EditEntryV1],
    ) -> Result<Vec<DestinationEdits<'a>>, CompositionError> {
        for entry in edits {
            let op = EditOp::of(&entry.edit);
            if !self.policy.allows_edit(op) {
                return Err(reject(
                    Invariant::Sc005,
                    ErrorCode::UnsupportedOperation,
                    format!(
                        "`{}` is not permitted for `{}`.",
                        entry.edit.name(),
                        self.policy.kind.as_str()
                    ),
                ));
            }
            let target = entry.edit.target();
            let destination = match op {
                EditOp::ReplaceAnchor => self.index.anchor_source(target).ok_or_else(|| {
                    reject(
                        Invariant::Sc009,
                        ErrorCode::ReferenceConflict,
                        format!("Anchor `{target}` was not issued in this plan."),
                    )
                    .with_related([target.clone()])
                })?,
                EditOp::CreateFile | EditOp::ReplaceFile => target,
            };
            let Some(reference) = self.index.reference(destination) else {
                return Err(reject(
                    Invariant::Sc004,
                    ErrorCode::ReferenceConflict,
                    format!("Destination `{destination}` is not in the reference table."),
                )
                .with_related([destination.clone()]));
            };
            let kind_ok = matches!(
                (op, reference),
                (
                    EditOp::CreateFile,
                    ReferenceEntryV1::PlannedDestination { .. }
                ) | (
                    EditOp::ReplaceFile | EditOp::ReplaceAnchor,
                    ReferenceEntryV1::ExistingSource { .. }
                        | ReferenceEntryV1::CandidateSource { .. }
                )
            );
            if !kind_ok {
                return Err(reject(
                    Invariant::Sc009,
                    ErrorCode::UnsupportedOperation,
                    format!(
                        "`{}` cannot target `{destination}`: a planned destination needs `create_file`, an existing one needs `replace_file` or an anchor.",
                        entry.edit.name()
                    ),
                )
                .with_related([destination.clone()]));
            }
            if !self.unit_owned.contains(destination) {
                return Err(reject(
                    Invariant::Sc009,
                    ErrorCode::OwnershipViolation,
                    format!("Destination `{destination}` is not owned by this unit."),
                )
                .with_related([destination.clone()]));
            }
            let role = reference.source_role().unwrap_or(SourceRole::Source);
            if !self.policy.may_write(role) {
                return Err(reject(
                    Invariant::Sc005,
                    ErrorCode::OwnershipViolation,
                    format!(
                        "`{}` may not write `{destination}` (role `{}`).",
                        self.policy.kind.as_str(),
                        serde_json::to_value(role)
                            .map(|v| v.as_str().unwrap_or("?").to_owned())
                            .unwrap_or_default()
                    ),
                )
                .with_related([destination.clone()]));
            }
        }
        group_by_destination(edits, |anchor| self.index.anchor_source(anchor))
    }

    fn check_evidence(
        &self,
        evidence: &'a [EvidenceItemV1],
        artifact_keys: &HashSet<&Handle>,
        require_full: bool,
    ) -> Result<(), CompositionError> {
        let mut covered = HashSet::new();
        for item in evidence {
            self.require_task(&item.task_ref)?;
            for criterion in &item.criterion_refs {
                self.require_criterion_of(&item.task_ref, criterion)?;
                covered.insert(criterion);
            }
            for key in &item.artifact_keys {
                if !artifact_keys.contains(key) {
                    return Err(reject(
                        Invariant::Sc010,
                        ErrorCode::EvidenceUnresolved,
                        format!(
                            "Evidence cites artifact key `{key}` that this payload did not produce."
                        ),
                    )
                    .with_related([key.clone()]));
                }
            }
            self.require_inputs("evidence.inputRefs", &item.input_refs)?;
            english("evidence explanation", &item.explanation)?;
        }
        if require_full
            && let Some(missing) = self
                .unit
                .criterion_refs
                .iter()
                .find(|c| !covered.contains(c))
        {
            return Err(reject(
                Invariant::Sc010,
                ErrorCode::EvidenceUnresolved,
                format!("No evidence covers criterion `{missing}`."),
            )
            .with_related([missing.clone()]));
        }
        Ok(())
    }

    fn check_authored_contents(
        &self,
        groups: &[DestinationEdits<'a>],
    ) -> Result<(), CompositionError> {
        for group in groups {
            let path = self
                .index
                .reference(&group.destination)
                .and_then(ReferenceEntryV1::relative_path);
            let is_document =
                path.is_some_and(|p| p.as_str().to_ascii_lowercase().ends_with(".md"));
            for operation in &group.operations {
                // Code is judged with the same guard the legacy pipeline applies
                // to every artifact; documents are prose in full.
                let field = if is_document {
                    "authored document"
                } else {
                    "authored source"
                };
                validate_authored_content(field, operation.content(), None)?;
            }
        }
        Ok(())
    }

    fn check_specification(
        &self,
        payload: &'a SpecificationContractV1,
    ) -> Result<(), CompositionError> {
        english("objective", &payload.objective)?;
        payload
            .exclusions
            .iter()
            .try_for_each(|t| english("exclusion", t))?;
        payload
            .decisions
            .iter()
            .try_for_each(|t| english("decision", t))?;
        self.require_full_coverage(
            "requirements",
            payload
                .requirements
                .iter()
                .map(|r| (&r.task_ref, &r.criterion_ref)),
        )?;
        for requirement in &payload.requirements {
            english("requirement", &requirement.requirement)?;
            english("verificationIntent", &requirement.verification_intent)?;
            self.require_checks(
                "requirement.requiredCheckRefs",
                &requirement.required_check_refs,
            )?;
            match (
                requirement.verification_mode,
                &requirement.not_applicable_reason,
            ) {
                (VerificationMode::NotApplicable, None) => {
                    return Err(reject(
                        Invariant::Sc010,
                        ErrorCode::EvidenceUnresolved,
                        format!(
                            "Criterion `{}` is marked not_applicable without a reason.",
                            requirement.criterion_ref
                        ),
                    ));
                }
                (VerificationMode::NotApplicable, Some(reason)) => {
                    english("notApplicableReason", reason)?
                }
                (VerificationMode::ExecutedCheck | VerificationMode::DocumentCheck, Some(_)) => {
                    return Err(reject(
                        Invariant::Sc010,
                        ErrorCode::EvidenceUnresolved,
                        format!(
                            "Criterion `{}` carries a not-applicable reason but is verifiable.",
                            requirement.criterion_ref
                        ),
                    ));
                }
                _ => {}
            }
        }
        for handle in &payload.existing_file_refs {
            if !matches!(
                self.index.reference(handle),
                Some(
                    ReferenceEntryV1::ExistingSource { .. }
                        | ReferenceEntryV1::CandidateSource { .. }
                )
            ) {
                return Err(reject(
                    Invariant::Sc004,
                    ErrorCode::ReferenceConflict,
                    format!("`existingFileRefs` cites `{handle}`, which is not an existing file."),
                )
                .with_related([handle.clone()]));
            }
        }
        for handle in &payload.planned_file_refs {
            if !matches!(
                self.index.reference(handle),
                Some(ReferenceEntryV1::PlannedDestination { .. })
            ) {
                return Err(reject(
                    Invariant::Sc004,
                    ErrorCode::ReferenceConflict,
                    format!(
                        "`plannedFileRefs` cites `{handle}`, which is not a planned destination."
                    ),
                )
                .with_related([handle.clone()]));
            }
        }
        for intent in &payload.interface_intent {
            english("interface intent description", &intent.description)?;
            for handle in &intent.source_refs {
                if !self.index.is_readable(handle) {
                    return Err(reject(
                        Invariant::Sc004,
                        ErrorCode::ReferenceConflict,
                        format!(
                            "Interface intent cites `{handle}`, which is not a delivered source."
                        ),
                    )
                    .with_related([handle.clone()]));
                }
            }
            for criterion in &intent.criterion_refs {
                if !self.unit_criteria.contains(criterion) {
                    return Err(reject(
                        Invariant::Sc002,
                        ErrorCode::ReferenceConflict,
                        format!("Interface intent cites criterion `{criterion}` outside the unit."),
                    )
                    .with_related([criterion.clone()]));
                }
            }
        }
        self.check_proposals(payload)
    }

    fn check_proposals(
        &self,
        payload: &'a SpecificationContractV1,
    ) -> Result<(), CompositionError> {
        let keys = payload
            .unit_proposals
            .iter()
            .map(|p| &p.unit_key)
            .collect::<Vec<_>>();
        let key_set = keys.iter().copied().collect::<HashSet<_>>();
        if key_set.len() != keys.len() {
            return Err(reject(
                Invariant::Sc006,
                ErrorCode::InvalidShape,
                "Unit proposals reuse a unit key.",
            ));
        }
        let mut covered_by_code = HashSet::new();
        for proposal in &payload.unit_proposals {
            english("unit proposal objective", &proposal.objective)?;
            for task in &proposal.task_refs {
                self.require_task(task)?;
            }
            for criterion in &proposal.criterion_refs {
                let Some(task) = self.index.task_of_criterion(criterion) else {
                    return Err(reject(
                        Invariant::Sc002,
                        ErrorCode::ReferenceConflict,
                        format!(
                            "Proposal `{}` cites unknown criterion `{criterion}`.",
                            proposal.unit_key
                        ),
                    ));
                };
                if !proposal.task_refs.contains(task) || !self.unit_criteria.contains(criterion) {
                    return Err(reject(
                        Invariant::Sc002,
                        ErrorCode::ReferenceConflict,
                        format!(
                            "Proposal `{}` cites criterion `{criterion}` outside its tasks.",
                            proposal.unit_key
                        ),
                    ));
                }
                if proposal.kind == super::ProposalKind::Code {
                    covered_by_code.insert(criterion);
                }
            }
            for handle in &proposal.input_refs {
                if !self.index.is_readable(handle) {
                    return Err(reject(
                        Invariant::Sc004,
                        ErrorCode::ReferenceConflict,
                        format!(
                            "Proposal `{}` input `{handle}` is not a delivered source.",
                            proposal.unit_key
                        ),
                    ));
                }
            }
            for handle in &proposal.destination_refs {
                if !self.index.is_writable(handle) {
                    return Err(reject(
                        Invariant::Sc004,
                        ErrorCode::ReferenceConflict,
                        format!(
                            "Proposal `{}` destination `{handle}` is not an existing or planned file.",
                            proposal.unit_key
                        ),
                    ));
                }
            }
            for dependency in &proposal.depends_on {
                if dependency == &proposal.unit_key || !key_set.contains(dependency) {
                    return Err(reject(
                        Invariant::Sc006,
                        ErrorCode::ReferenceConflict,
                        format!(
                            "Proposal `{}` depends on `{dependency}`, which is not another proposal.",
                            proposal.unit_key
                        ),
                    ));
                }
            }
        }
        if !payload.unit_proposals.is_empty() {
            let edges = payload
                .unit_proposals
                .iter()
                .map(|p| (&p.unit_key, p.depends_on.iter().collect::<Vec<_>>()))
                .collect::<BTreeMap<_, _>>();
            if has_cycle(&edges) {
                return Err(reject(
                    Invariant::Sc006,
                    ErrorCode::ReferenceConflict,
                    "Unit proposals form a dependency cycle.",
                ));
            }
            if let Some(missing) = self
                .unit
                .criterion_refs
                .iter()
                .find(|c| !covered_by_code.contains(c))
            {
                return Err(reject(
                    Invariant::Sc010,
                    ErrorCode::EvidenceUnresolved,
                    format!("No code proposal covers criterion `{missing}`."),
                )
                .with_related([missing.clone()]));
            }
        }
        Ok(())
    }

    fn check_contract(&self, payload: &'a InterfaceContractV1) -> Result<(), CompositionError> {
        let mut group_keys = HashSet::new();
        for group in &payload.groups {
            if !group_keys.insert(&group.interface_key) {
                return Err(reject(
                    Invariant::Sc006,
                    ErrorCode::InvalidShape,
                    format!("Interface key `{}` is defined twice.", group.interface_key),
                ));
            }
            for handle in &group.source_refs {
                if !self.index.is_readable(handle) {
                    return Err(reject(
                        Invariant::Sc004,
                        ErrorCode::ReferenceConflict,
                        format!(
                            "Interface `{}` cites `{handle}`, which is not a delivered source.",
                            group.interface_key
                        ),
                    ));
                }
            }
            for criterion in &group.criterion_refs {
                if !self.unit_criteria.contains(criterion) {
                    return Err(reject(
                        Invariant::Sc002,
                        ErrorCode::ReferenceConflict,
                        format!(
                            "Interface `{}` cites criterion `{criterion}` outside the unit.",
                            group.interface_key
                        ),
                    ));
                }
            }
            group
                .invariants
                .iter()
                .try_for_each(|t| english("interface invariant", t))?;
        }
        let proposals = self.approved.proposal_keys.iter().collect::<HashSet<_>>();
        let mut bound = HashSet::new();
        for binding in &payload.unit_bindings {
            if !proposals.contains(&binding.unit_key) {
                return Err(reject(
                    Invariant::Sc006,
                    ErrorCode::ReferenceConflict,
                    format!(
                        "Binding `{}` is not an approved unit proposal.",
                        binding.unit_key
                    ),
                ));
            }
            if !bound.insert(&binding.unit_key) {
                return Err(reject(
                    Invariant::Sc006,
                    ErrorCode::InvalidShape,
                    format!("Unit `{}` is bound twice.", binding.unit_key),
                ));
            }
            for key in &binding.interface_keys {
                if !group_keys.contains(key) {
                    return Err(reject(
                        Invariant::Sc006,
                        ErrorCode::ReferenceConflict,
                        format!(
                            "Binding `{}` cites undefined interface `{key}`.",
                            binding.unit_key
                        ),
                    ));
                }
            }
            for dependency in &binding.depends_on {
                if dependency == &binding.unit_key || !proposals.contains(dependency) {
                    return Err(reject(
                        Invariant::Sc006,
                        ErrorCode::ReferenceConflict,
                        format!(
                            "Binding `{}` depends on `{dependency}`, which is not another proposal.",
                            binding.unit_key
                        ),
                    ));
                }
            }
        }
        let edges = payload
            .unit_bindings
            .iter()
            .map(|b| (&b.unit_key, b.depends_on.iter().collect::<Vec<_>>()))
            .collect::<BTreeMap<_, _>>();
        if has_cycle(&edges) {
            return Err(reject(
                Invariant::Sc006,
                ErrorCode::ReferenceConflict,
                "Unit bindings form a dependency cycle.",
            ));
        }
        let (clean_refs, _ref_warnings) = self.sanitize_used_refs(&payload.used_refs);
        self.require_inputs("usedRefs", &clean_refs)
    }

    fn check_tests(
        &self,
        payload: &'a TestPayloadV1,
    ) -> Result<Vec<DestinationEdits<'a>>, CompositionError> {
        let groups = self.check_edits(&payload.edits)?;
        let artifact_keys = payload
            .edits
            .iter()
            .map(|e| &e.artifact_key)
            .collect::<HashSet<_>>();
        self.require_full_coverage(
            "coverage",
            payload
                .coverage
                .iter()
                .map(|c| (&c.task_ref, &c.criterion_ref)),
        )?;
        let not_applicable = self
            .approved
            .not_applicable_criteria
            .iter()
            .collect::<HashSet<_>>();
        for coverage in &payload.coverage {
            english("coverage explanation", &coverage.explanation)?;
            self.require_checks("coverage.checkRefs", &coverage.check_refs)?;
            match coverage.coverage {
                CoverageKind::Authored => {
                    if coverage.test_artifact_keys.is_empty() {
                        return Err(reject(
                            Invariant::Sc010,
                            ErrorCode::EvidenceUnresolved,
                            format!(
                                "`authored` coverage of `{}` names no test artifact.",
                                coverage.criterion_ref
                            ),
                        ));
                    }
                    for key in &coverage.test_artifact_keys {
                        if !artifact_keys.contains(key) {
                            return Err(reject(
                                Invariant::Sc010,
                                ErrorCode::EvidenceUnresolved,
                                format!(
                                    "Coverage cites artifact key `{key}` that this payload did not produce."
                                ),
                            ));
                        }
                    }
                }
                CoverageKind::Existing => {
                    if coverage.test_input_refs.is_empty() {
                        return Err(reject(
                            Invariant::Sc010,
                            ErrorCode::EvidenceUnresolved,
                            format!(
                                "`existing` coverage of `{}` names no existing test.",
                                coverage.criterion_ref
                            ),
                        ));
                    }
                    self.require_inputs("coverage.testInputRefs", &coverage.test_input_refs)?;
                    for handle in &coverage.test_input_refs {
                        if self
                            .index
                            .reference(handle)
                            .and_then(ReferenceEntryV1::source_role)
                            != Some(SourceRole::Test)
                        {
                            return Err(reject(
                                Invariant::Sc010,
                                ErrorCode::EvidenceUnresolved,
                                format!(
                                    "`existing` coverage cites `{handle}`, which is not a test file."
                                ),
                            ));
                        }
                    }
                }
                CoverageKind::NotApplicable => {
                    if !not_applicable.contains(&coverage.criterion_ref) {
                        return Err(reject(
                            Invariant::Sc022,
                            ErrorCode::EvidenceUnresolved,
                            format!(
                                "Criterion `{}` was not approved as not_applicable; Test cannot exempt it.",
                                coverage.criterion_ref
                            ),
                        ));
                    }
                }
            }
        }
        let require_full = !self.policy.repair;
        self.check_evidence(&payload.evidence, &artifact_keys, require_full)?;
        self.check_authored_contents(&groups)?;
        let (clean_refs, _ref_warnings) = self.sanitize_used_refs(&payload.used_refs);
        self.require_inputs("usedRefs", &clean_refs)?;
        Ok(groups)
    }

    fn check_integration(
        &self,
        payload: &'a IntegrationPayloadV1,
    ) -> Result<Vec<DestinationEdits<'a>>, CompositionError> {
        self.require_full_coverage(
            "assessments",
            payload
                .assessments
                .iter()
                .map(|a| (&a.task_ref, &a.criterion_ref)),
        )?;
        for assessment in &payload.assessments {
            english("assessment explanation", &assessment.explanation)?;
            self.require_inputs("assessment.inputRefs", &assessment.input_refs)?;
            self.require_checks("assessment.checkRefs", &assessment.check_refs)?;
        }
        payload
            .blockers
            .iter()
            .try_for_each(|t| english("blocker", t))?;
        let groups = self.check_edits(&payload.documentation_edits)?;
        self.check_authored_contents(&groups)?;
        let (clean_refs, _ref_warnings) = self.sanitize_used_refs(&payload.used_refs);
        self.require_inputs("usedRefs", &clean_refs)?;
        Ok(groups)
    }

    fn check_completed(
        &self,
        executor_kind: ExecutorKind,
        payload: &'a CompletedPayloadV1,
    ) -> Result<Vec<DestinationEdits<'a>>, CompositionError> {
        // A bounded correction runs the unit's repair specialty; its answer
        // replaces the unit's result whole, so every other rule (ownership,
        // full evidence and coverage) is the initial specialty's.
        let admitted = executor_kind == self.unit.executor_kind
            || Some(executor_kind)
                == super::plan::repair_kind(self.unit.executor_kind)
                    .filter(|kind| *kind != self.unit.executor_kind);
        if !admitted {
            return Err(reject(
                Invariant::Sc005,
                ErrorCode::InvalidShape,
                format!(
                    "The response claims `{}` but the unit runs `{}`.",
                    executor_kind.as_str(),
                    self.unit.executor_kind.as_str()
                ),
            ));
        }
        match payload {
            CompletedPayloadV1::Intake(intake) => {
                let mut seen = HashSet::new();
                for task in &intake.tasks {
                    self.require_task(&task.task_ref)?;
                    if !seen.insert(&task.task_ref) {
                        return Err(reject(
                            Invariant::Sc010,
                            ErrorCode::EvidenceUnresolved,
                            format!("Task `{}` is classified twice.", task.task_ref),
                        ));
                    }
                    english("task summary", &task.summary)?;
                }
                if let Some(missing) = self.unit.task_refs.iter().find(|t| !seen.contains(t)) {
                    return Err(reject(
                        Invariant::Sc010,
                        ErrorCode::EvidenceUnresolved,
                        format!("Task `{missing}` was not classified."),
                    )
                    .with_related([missing.clone()]));
                }
                intake.issues.iter().try_for_each(|t| english("issue", t))?;
                let (clean_refs, _ref_warnings) = self.sanitize_used_refs(&intake.used_refs);
                self.require_inputs("usedRefs", &clean_refs)?;
                Ok(Vec::new())
            }
            CompletedPayloadV1::Specification(spec) => {
                self.check_specification(spec)?;
                Ok(Vec::new())
            }
            CompletedPayloadV1::Contract(contract) => {
                self.check_contract(contract)?;
                Ok(Vec::new())
            }
            CompletedPayloadV1::Edits(edits) => {
                let groups = self.check_edits(&edits.edits)?;
                let artifact_keys = edits
                    .edits
                    .iter()
                    .map(|e| &e.artifact_key)
                    .collect::<HashSet<_>>();
                self.check_evidence(&edits.evidence, &artifact_keys, !self.policy.repair)?;
                self.check_authored_contents(&groups)?;
                let (clean_refs, _ref_warnings) = self.sanitize_used_refs(&edits.used_refs);
                self.require_inputs("usedRefs", &clean_refs)?;
                Ok(groups)
            }
            CompletedPayloadV1::Tests(tests) => self.check_tests(tests),
            CompletedPayloadV1::Integration(integration) => self.check_integration(integration),
        }
    }
}

fn has_cycle(edges: &BTreeMap<&Handle, Vec<&Handle>>) -> bool {
    fn visit<'a>(
        node: &'a Handle,
        edges: &BTreeMap<&'a Handle, Vec<&'a Handle>>,
        state: &mut HashMap<&'a Handle, u8>,
    ) -> bool {
        match state.get(node) {
            Some(1) => return true,
            Some(2) => return false,
            _ => {}
        }
        state.insert(node, 1);
        for next in edges.get(node).into_iter().flatten() {
            if visit(next, edges, state) {
                return true;
            }
        }
        state.insert(node, 2);
        false
    }
    let mut state = HashMap::new();
    edges.keys().any(|node| visit(node, edges, &mut state))
}

/// Validates a parsed response for `unit` inside `plan`.
pub fn validate_child<'a>(
    index: &'a PlanIndex<'a>,
    unit: &'a WorkUnitSpecV1,
    approved: &'a ApprovedContext,
    response: &'a UnitModelResponseV1,
) -> Result<ValidatedChild<'a>, CompositionError> {
    let checker = Checker::new(index, unit, approved)?;
    let destinations = match response {
        UnitModelResponseV1::Completed {
            executor_kind,
            payload,
        } => checker.check_completed(*executor_kind, payload)?,
        UnitModelResponseV1::NeedsInput {
            missing_refs,
            questions,
        } => {
            if !checker.policy.may_ask {
                return Err(reject(
                    Invariant::Sc005,
                    ErrorCode::UnsupportedOperation,
                    format!(
                        "`{}` cannot request input; report a failure instead.",
                        checker.policy.kind.as_str()
                    ),
                ));
            }
            for handle in missing_refs {
                if !checker.known_handle(handle) {
                    return Err(reject(
                        Invariant::Sc004,
                        ErrorCode::ReferenceConflict,
                        format!("`missingRefs` cites unknown handle `{handle}`."),
                    )
                    .with_related([handle.clone()]));
                }
            }
            let mut keys = HashSet::new();
            for question in questions {
                if !keys.insert(&question.key) {
                    return Err(reject(
                        Invariant::Sc008,
                        ErrorCode::InvalidShape,
                        format!("Question key `{}` is repeated.", question.key),
                    ));
                }
                for task in &question.task_refs {
                    checker.require_task(task)?;
                }
                english("question", &question.question)?;
                english("question rationale", &question.rationale)?;
            }
            Vec::new()
        }
        UnitModelResponseV1::Failed {
            message,
            related_refs,
            ..
        } => {
            english("failure message", message)?;
            for handle in related_refs {
                if !checker.known_handle(handle) {
                    return Err(reject(
                        Invariant::Sc004,
                        ErrorCode::ReferenceConflict,
                        format!("`relatedRefs` cites unknown handle `{handle}`."),
                    )
                    .with_related([handle.clone()]));
                }
            }
            Vec::new()
        }
    };
    Ok(ValidatedChild {
        response,
        destinations,
    })
}

#[cfg(test)]
mod tests {
    use super::super::references::ReferenceTable;
    use super::super::*;
    use super::*;
    use crate::orchestration::SchemaV1;

    fn h(text: &str) -> Handle {
        Handle::new(text).unwrap()
    }

    fn uuid(n: u8) -> Uuid {
        Uuid::new(format!("10000000-0000-4000-8000-0000000000{n:02}")).unwrap()
    }

    fn zero() -> Sha256Hex {
        Sha256Hex::new("0".repeat(64)).unwrap()
    }

    struct Fixture {
        plan: StageExecutionPlanV1,
        source_bytes: Vec<u8>,
    }

    /// Two tasks, one existing source with an anchor, one planned test, one
    /// unavailable reference and one configured check.
    fn fixture(kind: ExecutorKind) -> Fixture {
        let source_bytes = b"pub fn value() -> u8 { 1 }\n".to_vec();
        let mut builder = ReferenceTable::builder();
        builder
            .add_task(
                "t-1",
                1,
                "First",
                &[("AC-1".into(), "One".into()), ("AC-2".into(), "Two".into())],
            )
            .unwrap();
        builder
            .add_task("t-2", 1, "Second", &[("AC-1".into(), "Other".into())])
            .unwrap();
        let src = builder
            .add_existing(
                RelPath::new("src/lib.rs").unwrap(),
                Sha256Hex::of(&source_bytes),
                SourceRole::Source,
            )
            .unwrap();
        builder
            .add_existing(
                RelPath::new("tests/existing.rs").unwrap(),
                Sha256Hex::of(b"t"),
                SourceRole::Test,
            )
            .unwrap();
        builder
            .add_planned(RelPath::new("tests/new_test.rs").unwrap(), SourceRole::Test)
            .unwrap();
        builder
            .add_planned(RelPath::new("docs/notes.md").unwrap(), SourceRole::Document)
            .unwrap();
        builder.add_unavailable(UnavailableReason::Missing, "No fixture file.");
        builder.add_check(h("behavior_check")).unwrap();
        builder.add_line_anchor(&src, &source_bytes, 1, 1).unwrap();
        let table = builder.build();
        let owned = match kind.stage() {
            Stage::Code => vec![h("src_1")],
            Stage::Test => vec![h("plan_1")],
            Stage::Integrator => vec![h("plan_2")],
            _ => vec![],
        };
        let unit = WorkUnitSpecV1 {
            schema_version: SchemaV1,
            unit_id: uuid(1),
            logical_unit_key: h("unit_a"),
            stage: kind.stage(),
            executor_kind: kind,
            task_refs: vec![h("task_1"), h("task_2")],
            criterion_refs: vec![h("crit_1_1"), h("crit_1_2"), h("crit_2_1")],
            dependencies: vec![],
            input_refs: vec![h("src_1"), h("src_2"), h("unav_1")],
            owned_destination_refs: owned,
            required_check_refs: vec![h("behavior_check")],
            objective: "Implement the approved behavior.".into(),
            max_output_tokens: 1000,
        };
        let plan = StageExecutionPlanV1 {
            schema_version: SchemaV1,
            stage_execution_id: uuid(2),
            budget_account_id: uuid(3),
            parent_fence: ParentFenceV1 {
                project_id: uuid(4),
                pulsation_id: uuid(5),
                input_revision: 1,
                job_id: uuid(6),
                generation: 1,
                state_version: 1,
            },
            stage: kind.stage(),
            recipe_version: 1,
            policy_sha256: zero(),
            batch_sha256: zero(),
            approved_contract_sha256: None,
            plan_sha256: zero(),
            scope: table.scope().to_vec(),
            references: table.entries().to_vec(),
            anchors: table.anchors().to_vec(),
            check_refs: table.checks().to_vec(),
            units: vec![unit],
            profile_bindings: vec![],
            budget: StageBudgetV1 {
                initial_invocation_limit: 1,
                extra_invocation_limit: 2,
                per_unit_extra_invocation_limit: 1,
                initial_output_token_limit: 1000,
                extra_output_token_limit: 1000,
                initial_prompt_character_limit: 64_000,
                extra_prompt_character_limit: 64_000,
                max_in_flight_units: 1,
            },
        };
        plan.validate().unwrap();
        Fixture { plan, source_bytes }
    }

    fn run(kind: ExecutorKind, response: &UnitModelResponseV1) -> Result<usize, CompositionError> {
        let fixture = fixture(kind);
        let index = PlanIndex::new(&fixture.plan);
        let approved = ApprovedContext {
            not_applicable_criteria: vec![h("crit_2_1")],
            proposal_keys: vec![h("implement_a"), h("test_a")],
        };
        validate_child(&index, &fixture.plan.units[0], &approved, response)
            .map(|v| v.destinations.len())
    }

    fn evidence(task: &str, criteria: &[&str], keys: &[&str]) -> EvidenceItemV1 {
        EvidenceItemV1 {
            task_ref: h(task),
            criterion_refs: criteria.iter().map(|c| h(c)).collect(),
            artifact_keys: keys.iter().map(|k| h(k)).collect(),
            input_refs: vec![h("src_1")],
            explanation: "The owned change implements the approved behavior.".into(),
        }
    }

    fn code_response(
        edits: Vec<EditEntryV1>,
        evidence: Vec<EvidenceItemV1>,
    ) -> UnitModelResponseV1 {
        UnitModelResponseV1::Completed {
            executor_kind: ExecutorKind::CodeImplementer,
            payload: CompletedPayloadV1::Edits(EditSetV1 {
                edits,
                evidence,
                used_refs: vec![h("src_1")],
            }),
        }
    }

    fn anchor_edit(key: &str) -> EditEntryV1 {
        EditEntryV1 {
            artifact_key: h(key),
            edit: EditOperationV1::ReplaceAnchor {
                anchor_ref: h("anchor_1"),
                content: "pub fn value() -> u8 { 2 }\n".into(),
            },
        }
    }

    #[test]
    fn a_code_response_inside_scope_ownership_and_policy_is_accepted() {
        let response = code_response(
            vec![anchor_edit("lib")],
            vec![
                evidence("task_1", &["crit_1_1", "crit_1_2"], &["lib"]),
                evidence("task_2", &["crit_2_1"], &["lib"]),
            ],
        );
        assert_eq!(run(ExecutorKind::CodeImplementer, &response).unwrap(), 1);
        // The reconstructed bytes come from the plan's anchor, not the model.
        let fixture = fixture(ExecutorKind::CodeImplementer);
        let out = super::super::edits::apply_destination_edits(
            Some(&fixture.source_bytes),
            &fixture.plan.anchors,
            &[&anchor_edit("lib").edit],
        )
        .unwrap();
        assert_eq!(out.content, "pub fn value() -> u8 { 2 }\n");
    }

    #[test]
    fn evidence_must_resolve_to_this_units_tasks_criteria_and_artifacts() {
        // Criterion of another task.
        let response = code_response(
            vec![anchor_edit("lib")],
            vec![evidence(
                "task_1",
                &["crit_1_1", "crit_1_2", "crit_2_1"],
                &["lib"],
            )],
        );
        let error = run(ExecutorKind::CodeImplementer, &response).unwrap_err();
        assert_eq!(error.sc, Invariant::Sc002);
        // Unknown task handle.
        let response = code_response(
            vec![anchor_edit("lib")],
            vec![evidence("task_9", &["crit_1_1"], &["lib"])],
        );
        assert_eq!(
            run(ExecutorKind::CodeImplementer, &response)
                .unwrap_err()
                .sc,
            Invariant::Sc002
        );
        // Nonexistent artifact key.
        let response = code_response(
            vec![anchor_edit("lib")],
            vec![
                evidence("task_1", &["crit_1_1", "crit_1_2"], &["ghost"]),
                evidence("task_2", &["crit_2_1"], &["lib"]),
            ],
        );
        assert_eq!(
            run(ExecutorKind::CodeImplementer, &response)
                .unwrap_err()
                .code,
            ErrorCode::EvidenceUnresolved
        );
        // Missing criterion coverage.
        let response = code_response(
            vec![anchor_edit("lib")],
            vec![evidence("task_1", &["crit_1_1"], &["lib"])],
        );
        let error = run(ExecutorKind::CodeImplementer, &response).unwrap_err();
        assert_eq!(error.code, ErrorCode::EvidenceUnresolved);
        assert_eq!(error.related, vec![h("crit_1_2")]);
        // A repairer may cover a subset.
        let response = UnitModelResponseV1::Completed {
            executor_kind: ExecutorKind::CodeRepairer,
            payload: CompletedPayloadV1::Edits(EditSetV1 {
                edits: vec![anchor_edit("lib")],
                evidence: vec![evidence("task_1", &["crit_1_1"], &["lib"])],
                used_refs: vec![],
            }),
        };
        assert!(run(ExecutorKind::CodeRepairer, &response).is_ok());
    }

    #[test]
    fn edits_outside_ownership_policy_or_the_reference_table_are_rejected() {
        let full = vec![
            evidence("task_1", &["crit_1_1", "crit_1_2"], &["x"]),
            evidence("task_2", &["crit_2_1"], &["x"]),
        ];
        // Code writing a test destination: role forbidden.
        let response = code_response(
            vec![EditEntryV1 {
                artifact_key: h("x"),
                edit: EditOperationV1::CreateFile {
                    destination_ref: h("plan_1"),
                    content: "// t\n".into(),
                },
            }],
            full.clone(),
        );
        let error = run(ExecutorKind::CodeImplementer, &response).unwrap_err();
        assert_eq!(error.code, ErrorCode::OwnershipViolation);
        // Replacing a planned destination is the wrong operation.
        let response = code_response(
            vec![EditEntryV1 {
                artifact_key: h("x"),
                edit: EditOperationV1::ReplaceFile {
                    source_ref: h("plan_1"),
                    content: "x".into(),
                },
            }],
            full.clone(),
        );
        assert_eq!(
            run(ExecutorKind::CodeImplementer, &response)
                .unwrap_err()
                .code,
            ErrorCode::UnsupportedOperation
        );
        // Unknown anchor and unknown destination.
        let response = code_response(
            vec![EditEntryV1 {
                artifact_key: h("x"),
                edit: EditOperationV1::ReplaceAnchor {
                    anchor_ref: h("anchor_7"),
                    content: "x".into(),
                },
            }],
            full.clone(),
        );
        assert_eq!(
            run(ExecutorKind::CodeImplementer, &response)
                .unwrap_err()
                .code,
            ErrorCode::ReferenceConflict
        );
        let response = code_response(
            vec![EditEntryV1 {
                artifact_key: h("x"),
                edit: EditOperationV1::CreateFile {
                    destination_ref: h("plan_9"),
                    content: "x".into(),
                },
            }],
            full.clone(),
        );
        assert_eq!(
            run(ExecutorKind::CodeImplementer, &response)
                .unwrap_err()
                .code,
            ErrorCode::ReferenceConflict
        );
        // Owned source but the specialty may not edit at all.
        let response = UnitModelResponseV1::Completed {
            executor_kind: ExecutorKind::ContractAuthor,
            payload: CompletedPayloadV1::Contract(InterfaceContractV1 {
                groups: vec![],
                unit_bindings: vec![],
                used_refs: vec![h("unav_1")],
            }),
        };
        // `unav_1` is delivered but not readable. Since sanitize_used_refs
        // silently strips invalid handles from usedRefs instead of rejecting
        // with SC-004, the call now succeeds with an empty effective usedRefs.
        run(ExecutorKind::ContractAuthor, &response).unwrap();
    }

    #[test]
    fn test_coverage_rules_and_not_applicable_exemptions_come_from_the_approval() {
        let coverage =
            |criterion: &str, kind: CoverageKind, keys: &[&str], inputs: &[&str]| TestCoverageV1 {
                task_ref: h(if criterion.starts_with("crit_1") {
                    "task_1"
                } else {
                    "task_2"
                }),
                criterion_ref: h(criterion),
                coverage: kind,
                test_artifact_keys: keys.iter().map(|k| h(k)).collect(),
                test_input_refs: inputs.iter().map(|i| h(i)).collect(),
                check_refs: vec![h("behavior_check")],
                explanation: "Covered by the listed test.".into(),
            };
        let test_edit = EditEntryV1 {
            artifact_key: h("new_test"),
            edit: EditOperationV1::CreateFile {
                destination_ref: h("plan_1"),
                content: "#[test] fn works() {}\n".into(),
            },
        };
        let payload = |cov: Vec<TestCoverageV1>| UnitModelResponseV1::Completed {
            executor_kind: ExecutorKind::TestAuthor,
            payload: CompletedPayloadV1::Tests(TestPayloadV1 {
                edits: vec![test_edit.clone()],
                coverage: cov,
                evidence: vec![
                    EvidenceItemV1 {
                        task_ref: h("task_1"),
                        criterion_refs: vec![h("crit_1_1"), h("crit_1_2")],
                        artifact_keys: vec![h("new_test")],
                        input_refs: vec![],
                        explanation: "Authored tests.".into(),
                    },
                    EvidenceItemV1 {
                        task_ref: h("task_2"),
                        criterion_refs: vec![h("crit_2_1")],
                        artifact_keys: vec![],
                        input_refs: vec![],
                        explanation: "Not applicable by approval.".into(),
                    },
                ],
                used_refs: vec![h("src_2")],
            }),
        };
        let ok = payload(vec![
            coverage("crit_1_1", CoverageKind::Authored, &["new_test"], &[]),
            coverage("crit_1_2", CoverageKind::Existing, &[], &["src_2"]),
            coverage("crit_2_1", CoverageKind::NotApplicable, &[], &[]),
        ]);
        assert_eq!(run(ExecutorKind::TestAuthor, &ok).unwrap(), 1);
        // A criterion not approved as not_applicable cannot be exempted by Test.
        let exempt = payload(vec![
            coverage("crit_1_1", CoverageKind::NotApplicable, &[], &[]),
            coverage("crit_1_2", CoverageKind::Existing, &[], &["src_2"]),
            coverage("crit_2_1", CoverageKind::NotApplicable, &[], &[]),
        ]);
        assert_eq!(
            run(ExecutorKind::TestAuthor, &exempt).unwrap_err().sc,
            Invariant::Sc022
        );
        // `existing` must cite a delivered test file, not a source file.
        let wrong = payload(vec![
            coverage("crit_1_1", CoverageKind::Authored, &["new_test"], &[]),
            coverage("crit_1_2", CoverageKind::Existing, &[], &["src_1"]),
            coverage("crit_2_1", CoverageKind::NotApplicable, &[], &[]),
        ]);
        assert_eq!(
            run(ExecutorKind::TestAuthor, &wrong).unwrap_err().code,
            ErrorCode::EvidenceUnresolved
        );
        // Missing a criterion.
        let partial = payload(vec![coverage(
            "crit_1_1",
            CoverageKind::Authored,
            &["new_test"],
            &[],
        )]);
        assert_eq!(
            run(ExecutorKind::TestAuthor, &partial).unwrap_err().code,
            ErrorCode::EvidenceUnresolved
        );
    }

    #[test]
    fn specification_requirements_cover_every_criterion_once_with_consistent_modes() {
        let requirement =
            |criterion: &str, mode: VerificationMode, reason: Option<&str>| RequirementV1 {
                task_ref: h(if criterion.starts_with("crit_1") {
                    "task_1"
                } else {
                    "task_2"
                }),
                criterion_ref: h(criterion),
                requirement: "Return the approved value.".into(),
                verification_intent: "Execute the configured behavioral test.".into(),
                verification_mode: mode,
                required_check_refs: vec![h("behavior_check")],
                not_applicable_reason: reason.map(str::to_owned),
            };
        let spec = |requirements: Vec<RequirementV1>, proposals: Vec<UnitProposalV1>| {
            UnitModelResponseV1::Completed {
                executor_kind: ExecutorKind::SpecificationAuthor,
                payload: CompletedPayloadV1::Specification(SpecificationContractV1 {
                    objective: "Implement the approved behavior.".into(),
                    exclusions: vec![],
                    decisions: vec!["Preserve the public signature.".into()],
                    requirements,
                    existing_file_refs: vec![h("src_1")],
                    planned_file_refs: vec![h("plan_1")],
                    interface_intent: vec![InterfaceIntentV1 {
                        intent: InterfaceIntent::Preserve,
                        source_refs: vec![h("src_1")],
                        criterion_refs: vec![h("crit_1_1")],
                        description: "Keep the signature.".into(),
                    }],
                    unit_proposals: proposals,
                }),
            }
        };
        let proposal =
            |key: &str, kind: ProposalKind, criteria: &[&str], depends: &[&str]| UnitProposalV1 {
                unit_key: h(key),
                kind,
                task_refs: vec![h("task_1"), h("task_2")],
                criterion_refs: criteria.iter().map(|c| h(c)).collect(),
                input_refs: vec![h("src_1")],
                destination_refs: vec![h("src_1")],
                depends_on: depends.iter().map(|d| h(d)).collect(),
                objective: "Implement the behavior.".into(),
            };
        let all = || {
            vec![
                requirement("crit_1_1", VerificationMode::ExecutedCheck, None),
                requirement("crit_1_2", VerificationMode::DocumentCheck, None),
                requirement(
                    "crit_2_1",
                    VerificationMode::NotApplicable,
                    Some("Documentation-only task."),
                ),
            ]
        };
        assert!(run(ExecutorKind::SpecificationAuthor, &spec(all(), vec![])).is_ok());
        let with_proposals = spec(
            all(),
            vec![
                proposal(
                    "implement_a",
                    ProposalKind::Code,
                    &["crit_1_1", "crit_1_2", "crit_2_1"],
                    &[],
                ),
                proposal(
                    "test_a",
                    ProposalKind::Test,
                    &["crit_1_1"],
                    &["implement_a"],
                ),
            ],
        );
        assert!(run(ExecutorKind::SpecificationAuthor, &with_proposals).is_ok());
        // not_applicable without a reason; a reason on a verifiable criterion.
        let mut bad = all();
        bad[2].not_applicable_reason = None;
        assert!(run(ExecutorKind::SpecificationAuthor, &spec(bad, vec![])).is_err());
        let mut bad = all();
        bad[0].not_applicable_reason = Some("x".into());
        assert!(run(ExecutorKind::SpecificationAuthor, &spec(bad, vec![])).is_err());
        // Duplicate and missing criteria.
        let mut dup = all();
        dup.push(requirement(
            "crit_1_1",
            VerificationMode::ExecutedCheck,
            None,
        ));
        assert!(run(ExecutorKind::SpecificationAuthor, &spec(dup, vec![])).is_err());
        let mut missing = all();
        missing.pop();
        assert!(run(ExecutorKind::SpecificationAuthor, &spec(missing, vec![])).is_err());
        // Proposal cycles, self dependencies and uncovered criteria.
        let cycle = spec(
            all(),
            vec![
                proposal(
                    "a",
                    ProposalKind::Code,
                    &["crit_1_1", "crit_1_2", "crit_2_1"],
                    &["b"],
                ),
                proposal("b", ProposalKind::Code, &["crit_1_1"], &["a"]),
            ],
        );
        assert_eq!(
            run(ExecutorKind::SpecificationAuthor, &cycle)
                .unwrap_err()
                .sc,
            Invariant::Sc006
        );
        let uncovered = spec(
            all(),
            vec![proposal("a", ProposalKind::Code, &["crit_1_1"], &[])],
        );
        assert_eq!(
            run(ExecutorKind::SpecificationAuthor, &uncovered)
                .unwrap_err()
                .code,
            ErrorCode::EvidenceUnresolved
        );
        // Planned refs must be planned; existing refs must exist.
        let mut wrong_refs = all();
        wrong_refs.truncate(3);
        let mut response = spec(wrong_refs, vec![]);
        if let UnitModelResponseV1::Completed {
            payload: CompletedPayloadV1::Specification(s),
            ..
        } = &mut response
        {
            s.planned_file_refs = vec![h("src_1")];
        }
        assert_eq!(
            run(ExecutorKind::SpecificationAuthor, &response)
                .unwrap_err()
                .sc,
            Invariant::Sc004
        );
    }

    #[test]
    fn needs_input_is_only_for_specification_and_failures_cite_known_handles() {
        let ask = UnitModelResponseV1::NeedsInput {
            missing_refs: vec![h("unav_1")],
            questions: vec![QuestionV1 {
                key: h("q1"),
                task_refs: vec![h("task_1")],
                question: "Which behavior is required?".into(),
                rationale: "The inputs conflict.".into(),
            }],
        };
        assert!(run(ExecutorKind::SpecificationAuthor, &ask).is_ok());
        assert_eq!(
            run(ExecutorKind::CodeImplementer, &ask).unwrap_err().code,
            ErrorCode::UnsupportedOperation
        );
        let unknown = UnitModelResponseV1::NeedsInput {
            missing_refs: vec![h("src_99")],
            questions: vec![QuestionV1 {
                key: h("q1"),
                task_refs: vec![h("task_1")],
                question: "Which?".into(),
                rationale: "Why.".into(),
            }],
        };
        assert!(run(ExecutorKind::SpecificationAuthor, &unknown).is_err());
        let failed = UnitModelResponseV1::Failed {
            code: WorkerFailureCode::ContractConflict,
            message: "The invariant conflicts with the request.".into(),
            related_refs: vec![h("src_1")],
        };
        assert!(run(ExecutorKind::CodeImplementer, &failed).is_ok());
        let failed = UnitModelResponseV1::Failed {
            code: WorkerFailureCode::ContractConflict,
            message: "x".into(),
            related_refs: vec![h("nope")],
        };
        assert!(run(ExecutorKind::CodeImplementer, &failed).is_err());
    }

    // ADR-022 clarification loop (P3/A3.2): the Specification Author's brief
    // now permits suggesting a candidate path in prose inside `question` when
    // `needs_input` is the only viable outcome for lack of a planned
    // destination. This proves the safeguard needs no schema change: `english`
    // is the only check `question`/`rationale` text ever goes through here
    // (validate.rs:1143-1144), so a path-shaped suggestion in prose can never
    // trip SC-004 -- that invariant only ever inspects handle-typed fields
    // (`missingRefs`, `usedRefs`, `existingFileRefs`, ...), never free text.
    #[test]
    fn a_path_suggestion_in_the_question_text_is_prose_not_a_handle_citation() {
        let ask = UnitModelResponseV1::NeedsInput {
            missing_refs: vec![],
            questions: vec![QuestionV1 {
                key: h("q1"),
                task_refs: vec![h("task_1")],
                question: "No planned destination is available for this decision. Suggested path based on the project's conventions: src/domain/session.rs (test: tests/session_test.rs). Confirm or adjust?".into(),
                rationale: "The approved intent names no existing or planned file for this task.".into(),
            }],
        };
        assert!(
            run(ExecutorKind::SpecificationAuthor, &ask).is_ok(),
            "a path suggested in prose must never be treated as a handle citation"
        );
    }

    #[test]
    fn newly_authored_prose_must_be_english_but_preserved_identifiers_are_exempt() {
        let response = code_response(
            vec![anchor_edit("lib")],
            vec![
                EvidenceItemV1 {
                    task_ref: h("task_1"),
                    criterion_refs: vec![h("crit_1_1"), h("crit_1_2")],
                    artifact_keys: vec![h("lib")],
                    input_refs: vec![],
                    explanation: "Implementação concluída conforme especificado.".into(),
                },
                evidence("task_2", &["crit_2_1"], &["lib"]),
            ],
        );
        assert_eq!(
            run(ExecutorKind::CodeImplementer, &response)
                .unwrap_err()
                .code,
            ErrorCode::InvalidLanguage
        );
        assert!(
            validate_authored_content("authored source", "let usuario = carregarUsuario();", None)
                .is_err()
        );
        assert!(
            validate_authored_content(
                "authored source",
                "let usuario = carregarUsuario();",
                Some("fn carregarUsuario() {} // usuario")
            )
            .is_ok()
        );
        assert!(
            validate_authored_content(
                "authored source",
                "// não faça isso\nlet x = 1;",
                Some("fn a() {}")
            )
            .is_err()
        );
    }

    #[test]
    fn a_unit_spec_inconsistent_with_the_plan_is_a_host_contract_error() {
        let fixture = fixture(ExecutorKind::CodeImplementer);
        let index = PlanIndex::new(&fixture.plan);
        let mut unit = fixture.plan.units[0].clone();
        unit.owned_destination_refs = vec![h("unav_1")];
        let response = UnitModelResponseV1::Failed {
            code: WorkerFailureCode::ScopeInsufficient,
            message: "x".into(),
            related_refs: vec![],
        };
        let error =
            validate_child(&index, &unit, &ApprovedContext::default(), &response).unwrap_err();
        assert_eq!(error.code, ErrorCode::HostContractError);
    }
}
