//! `ExecutorPolicy`: the single host source for what a specialty is told, what
//! it may return and what the host will enforce (SC-005).
//!
//! Every rule stated to the model in `role_brief` has a matching executable
//! field consumed by `validate.rs`; a prohibition that exists only as prose is
//! a bug. The policies are versioned as a whole through [`recipe_policy_sha256`]
//! so a plan records exactly which rules governed its workers.

use serde::{Deserialize, Serialize};

use hvc_contracts::UnitContextScope;

use super::{
    CompositionError, EditOperationV1, ExecutorKind, Sha256Hex, SourceRole, Stage, canonical_sha256,
};

pub const RECIPE_POLICY_ID: &str = "stage-composition-recipes-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditOp {
    CreateFile,
    ReplaceFile,
    ReplaceAnchor,
}

impl EditOp {
    pub const ALL: [Self; 3] = [Self::CreateFile, Self::ReplaceFile, Self::ReplaceAnchor];

    pub fn of(edit: &EditOperationV1) -> Self {
        match edit {
            EditOperationV1::CreateFile { .. } => Self::CreateFile,
            EditOperationV1::ReplaceFile { .. } => Self::ReplaceFile,
            EditOperationV1::ReplaceAnchor { .. } => Self::ReplaceAnchor,
        }
    }
}

/// What a transport may expose to the worker. Project command execution is a
/// separate host facility and never appears here (ADR-010/ADR-011). Every
/// variant is enforced by the transport; ADR-034 T1 removed the declarative
/// `none` that a CLI worker did not honor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPolicy {
    /// Read-only inspection: a CLI worker runs in the sanitized workspace
    /// snapshot under the pipeline hardening (read tools, no writes, no
    /// shell). The API transport has no tools.
    ReadOnlyScoped,
    /// The worker reads only the host-selected context of its prompt. A CLI
    /// worker runs in an empty directory under the Companion hardening; a
    /// custom CLI adapter is refused (ADR-032).
    HostContextOnly,
}

impl ToolPolicy {
    /// The scope the transport applies for this policy. The only mapping
    /// between the two; the transport refuses any other scope for the
    /// specialty (`UnitContextScope::required_for`).
    pub const fn context_scope(self) -> UnitContextScope {
        match self {
            Self::ReadOnlyScoped => UnitContextScope::Workspace,
            Self::HostContextOnly => UnitContextScope::HostContextOnly,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecutorPolicy {
    pub kind: ExecutorKind,
    pub stage: Stage,
    /// ROLE / DO NOT / DELIVER contract, in English, handle-based.
    pub role_brief: &'static str,
    /// The registry definition the `completed` payload must satisfy.
    pub payload: &'static str,
    /// Edit operations the payload may carry. Empty: the payload has no edits.
    pub allowed_edit_operations: &'static [EditOp],
    /// Source roles of destinations this specialty may write.
    pub writable_roles: &'static [SourceRole],
    /// Whether `needs_input` is an admissible outcome (only where the stage
    /// has a clarification checkpoint).
    pub may_ask: bool,
    /// Whether a completed payload must carry task evidence.
    pub requires_evidence: bool,
    /// Whether this specialty is a bounded correction of an existing unit.
    pub repair: bool,
    pub tools: ToolPolicy,
}

impl ExecutorPolicy {
    pub fn allows_edit(&self, op: EditOp) -> bool {
        self.allowed_edit_operations.contains(&op)
    }

    pub fn may_write(&self, role: SourceRole) -> bool {
        self.writable_roles.contains(&role)
    }
}

const COMMON_RULES: &str = concat!(
    "REFERENCES: Every input, file, task, criterion, anchor and check is identified by a short handle listed in the prompt (for example `task_1`, `crit_1_2`, `src_3`, `anchor_2`, `check_1`). Use handles exactly as listed. Never invent handles, file paths, identifiers, hashes, line numbers or search strings; if something you need is not listed, say so through the outcome the contract allows for your role.
OUTPUT: Return one JSON object matching the supplied response contract and nothing else - no Markdown outside the JSON, no status updates, no publication or approval instructions, no commands. All newly authored prose is English; existing source identifiers and exact bytes are preserved.
",
    // ADR-037 M1: the provenance legend replaces the old trust sentence.
    crate::exchange::render::provenance_legend!(),
    "
USED REFS: `usedRefs` accepts ONLY handles that appear in the References table AND are readable inputs (availability column shows \"readable\" or \"delivered\"; these are existing or candidate sources). Task handles (e.g. `task_1`), criterion handles (e.g. `crit_1_2`), anchor handles, check handles and planned-destination handles are NEVER valid in `usedRefs`. When no readable inputs appear in the References table, set `usedRefs` to `[]`."
);

const INTAKE_BRIEF: &str = "ROLE: Classify each listed task (feature, bugfix, refactor, test, documentation, maintenance or unknown) and assess its risk (low, medium, high or unknown) from the delivered facts. Summarize each task in one or two English sentences.
DO NOT: design APIs, plan implementation, propose files or edits, or guess a safe default - use `unknown` when the facts do not support a classification or risk.
DELIVER: `tasks` with one entry per listed task handle, `issues` naming missing or contradictory facts the next stage must know about, and `usedRefs` listing only the readable source handles from the References table that you actually relied on (never task, criterion, anchor, check or planned-destination handles; use `[]` when the table has no readable inputs).";

const SPECIFICATION_BRIEF: &str = "ROLE: Turn the approved intent for the listed tasks into ONE specification: an objective, exclusions, decisions, one requirement per listed criterion with its verification mode, which delivered files are involved, which planned destinations are needed, explicit interface intent (preserve, change, new or none) and cohesive unit proposals for Code and Test.
DO NOT: implement code, return edits, invent files outside the delivered existing/planned handles, or fill a missing human decision with an assumption - when a decision is missing return outcome `needs_input` with bounded questions that name the affected task handles. If `needs_input` is the only viable outcome because too few planned-destination handles exist to anchor this specification, you may suggest candidate file paths in prose inside the `question` text itself - this is a suggestion for a human to confirm, never a handle citation, and must not appear in `usedRefs` or any other handle-typed field.
DELIVER: a `SpecificationContractV1` payload. Every criterion handle appears in exactly one requirement. `not_applicable` verification requires a reason. Unit proposals reference only listed task/criterion/file handles and use local keys for `dependsOn`. A proposal's `inputRefs` holds delivered sources or a planned file another proposal writes (the host then orders the two and delivers that file); `dependsOn` holds proposal `unitKey` values only, never file handles. An interface intent may cite a planned file in `sourceRefs` only when its intent is `new`.";

const CONTRACT_BRIEF: &str = "ROLE: Define only the NEW or CHANGED interfaces the approved specification requires - signatures, payloads, events, invariants - and bind each proposed unit to the interface groups it depends on.
DO NOT: restate interfaces marked preserve (the host already holds them), implement logic, author tests, add requirements, or widen destinations beyond the delivered handles.
DELIVER: an `InterfaceContractV1` payload: `groups` (each with an exact signature and at least one invariant), `unitBindings` for the proposed unit keys, and `usedRefs`.";

const CODE_BRIEF: &str = "ROLE: Implement exactly the assigned operation against the approved specification and interfaces, editing only the destinations this unit owns.
DO NOT: change an approved interface (return outcome `failed` with code `contract_conflict` instead), touch files you do not own, author tests, delete or rename files, or describe tests as executed.
DELIVER: an `EditSetV1` payload. Use `create_file` with complete content for a planned destination, `replace_file` with complete content only for a small owned file the prompt marks as replaceable, and `replace_anchor` with the exact replacement for a delivered anchor. Every edit carries an `artifactKey`; `evidence` links each assigned task and criterion to the artifact keys that satisfy it.";

const CODE_REPAIR_BRIEF: &str = "ROLE: Correct the diagnostics listed for this unit's own output and return the unit's complete result again; it replaces the rejected one whole.
DO NOT: change approved interfaces, widen scope, touch other units' files, weaken behavior to silence a diagnostic, or change behavior the diagnostics do not concern.
DELIVER: a complete `EditSetV1` payload, exactly as the implementer would: edits for every owned destination this unit delivers (corrected where diagnosed) and evidence for every criterion the unit must evidence.";

const TEST_BRIEF: &str = "ROLE: Author the tests that prove the assigned criteria against the approved interfaces and the delivered production candidate, and declare coverage per criterion.
DO NOT: modify production code (report a defect through outcome `failed` with code `implementation_mismatch`), claim a test ran or passed, or mark a criterion `not_applicable` unless the approved specification already did.
DELIVER: a `TestPayloadV1` payload: test edits for owned test destinations, one `coverage` entry per assigned criterion (`authored`, `existing` or `not_applicable`), evidence and `usedRefs`.";

const TEST_REPAIR_BRIEF: &str = "ROLE: Fix the listed diagnostics in this unit's own unpublished tests and return the unit's complete result again; it replaces the rejected one whole.
DO NOT: change what a test asserts, remove assertions, touch production code, or alter coverage declarations to obtain a green result.
DELIVER: a complete `TestPayloadV1` payload, exactly as the test author would: edits for every owned test destination (corrected where diagnosed), one `coverage` entry per assigned criterion, evidence and `usedRefs`.";

const INTEGRATOR_BRIEF: &str = "ROLE: Assess, per listed criterion, whether the candidate files, their base → candidate changes, the approved contracts and the host evidence support it (`supported`), contradict it (`contradicted`) or leave it unverified (`unverified`), citing the handles you relied on. Apply the project's review policy when one is delivered.
DO NOT: state that compilation or tests passed unless the host evidence reports it, treat author claims as proof, mark `supported` a criterion that depends on an evidence gap, rewrite code or tests, edit candidate files, or invent a check result.
DELIVER: an `IntegrationPayloadV1` payload with one assessment per listed criterion, `blockers` for anything that prevents completion, optional `documentationEdits` limited to delivered document handles marked owned, and `usedRefs`.";

const NO_EDITS: &[EditOp] = &[];
const NO_ROLES: &[SourceRole] = &[];
const CODE_ROLES: &[SourceRole] = &[
    SourceRole::Source,
    SourceRole::Configuration,
    SourceRole::Document,
];
const TEST_ROLES: &[SourceRole] = &[SourceRole::Test, SourceRole::Document];
const DOCUMENT_ROLES: &[SourceRole] = &[SourceRole::Document];

static POLICIES: [ExecutorPolicy; 8] = [
    ExecutorPolicy {
        kind: ExecutorKind::IntakeClassifier,
        stage: Stage::Intake,
        role_brief: INTAKE_BRIEF,
        payload: "IntakePayloadV1",
        allowed_edit_operations: NO_EDITS,
        writable_roles: NO_ROLES,
        may_ask: false,
        requires_evidence: false,
        repair: false,
        tools: ToolPolicy::ReadOnlyScoped,
    },
    ExecutorPolicy {
        kind: ExecutorKind::SpecificationAuthor,
        stage: Stage::Specification,
        role_brief: SPECIFICATION_BRIEF,
        payload: "SpecificationContractV1",
        allowed_edit_operations: NO_EDITS,
        writable_roles: NO_ROLES,
        may_ask: true,
        requires_evidence: false,
        repair: false,
        tools: ToolPolicy::ReadOnlyScoped,
    },
    ExecutorPolicy {
        kind: ExecutorKind::ContractAuthor,
        stage: Stage::Contract,
        role_brief: CONTRACT_BRIEF,
        payload: "InterfaceContractV1",
        allowed_edit_operations: NO_EDITS,
        writable_roles: NO_ROLES,
        may_ask: false,
        requires_evidence: false,
        repair: false,
        tools: ToolPolicy::ReadOnlyScoped,
    },
    ExecutorPolicy {
        kind: ExecutorKind::CodeImplementer,
        stage: Stage::Code,
        role_brief: CODE_BRIEF,
        payload: "EditSetV1",
        allowed_edit_operations: &EditOp::ALL,
        writable_roles: CODE_ROLES,
        may_ask: false,
        requires_evidence: true,
        repair: false,
        tools: ToolPolicy::ReadOnlyScoped,
    },
    ExecutorPolicy {
        kind: ExecutorKind::CodeRepairer,
        stage: Stage::Code,
        role_brief: CODE_REPAIR_BRIEF,
        payload: "EditSetV1",
        allowed_edit_operations: &EditOp::ALL,
        writable_roles: CODE_ROLES,
        may_ask: false,
        requires_evidence: true,
        repair: true,
        tools: ToolPolicy::ReadOnlyScoped,
    },
    ExecutorPolicy {
        kind: ExecutorKind::TestAuthor,
        stage: Stage::Test,
        role_brief: TEST_BRIEF,
        payload: "TestPayloadV1",
        allowed_edit_operations: &EditOp::ALL,
        writable_roles: TEST_ROLES,
        may_ask: false,
        requires_evidence: true,
        repair: false,
        tools: ToolPolicy::ReadOnlyScoped,
    },
    ExecutorPolicy {
        kind: ExecutorKind::TestRepairer,
        stage: Stage::Test,
        role_brief: TEST_REPAIR_BRIEF,
        payload: "TestPayloadV1",
        allowed_edit_operations: &EditOp::ALL,
        writable_roles: TEST_ROLES,
        may_ask: false,
        requires_evidence: true,
        repair: true,
        tools: ToolPolicy::ReadOnlyScoped,
    },
    ExecutorPolicy {
        kind: ExecutorKind::IntegrationReviewer,
        stage: Stage::Integrator,
        role_brief: INTEGRATOR_BRIEF,
        payload: "IntegrationPayloadV1",
        allowed_edit_operations: &EditOp::ALL,
        writable_roles: DOCUMENT_ROLES,
        may_ask: false,
        requires_evidence: false,
        repair: false,
        tools: ToolPolicy::HostContextOnly,
    },
];

pub fn policy_for(kind: ExecutorKind) -> &'static ExecutorPolicy {
    POLICIES
        .iter()
        .find(|policy| policy.kind == kind)
        .expect("every specialty has a policy")
}

pub fn all_policies() -> &'static [ExecutorPolicy] {
    &POLICIES
}

/// The rules shared by every specialty, appended after the role brief.
pub const fn common_rules() -> &'static str {
    COMMON_RULES
}

/// Digest of the complete versioned policy set. A plan records it as
/// `policySha256`; changing any brief, permission or limit changes the digest.
pub fn recipe_policy_sha256() -> Result<Sha256Hex, CompositionError> {
    #[derive(Serialize)]
    struct Snapshot<'a> {
        recipe: &'static str,
        common_rules: &'static str,
        policies: &'a [ExecutorPolicy],
    }
    canonical_sha256(
        &Snapshot {
            recipe: RECIPE_POLICY_ID,
            common_rules: COMMON_RULES,
            policies: &POLICIES,
        },
        &[],
    )
}

#[cfg(test)]
mod tests {
    use super::super::schema::payload_definition;
    use super::*;

    #[test]
    fn every_specialty_has_a_distinct_brief_with_role_prohibitions_and_deliverable() {
        let mut seen = std::collections::HashSet::new();
        for kind in ExecutorKind::ALL {
            let policy = policy_for(kind);
            assert_eq!(policy.kind, kind);
            assert_eq!(policy.stage, kind.stage());
            assert_eq!(policy.repair, kind.is_repair());
            for marker in ["ROLE:", "DO NOT:", "DELIVER:"] {
                assert!(
                    policy.role_brief.contains(marker),
                    "{kind:?} lacks {marker}"
                );
            }
            assert!(seen.insert(policy.role_brief), "{kind:?} reuses a brief");
            assert_eq!(policy.payload, payload_definition(kind));
        }
    }

    #[test]
    fn prose_prohibitions_have_executable_counterparts() {
        // Intake/Specification/Contract never edit; the brief says so and the
        // policy leaves nothing to write with.
        for kind in [
            ExecutorKind::IntakeClassifier,
            ExecutorKind::SpecificationAuthor,
            ExecutorKind::ContractAuthor,
        ] {
            let policy = policy_for(kind);
            assert!(policy.allowed_edit_operations.is_empty());
            assert!(policy.writable_roles.is_empty());
            assert!(!policy.requires_evidence);
        }
        // Code cannot author tests; Test cannot touch production; Integrator
        // may only touch documents.
        assert!(!policy_for(ExecutorKind::CodeImplementer).may_write(SourceRole::Test));
        assert!(policy_for(ExecutorKind::CodeImplementer).may_write(SourceRole::Source));
        assert!(!policy_for(ExecutorKind::TestAuthor).may_write(SourceRole::Source));
        assert!(policy_for(ExecutorKind::TestAuthor).may_write(SourceRole::Test));
        assert!(!policy_for(ExecutorKind::TestRepairer).may_write(SourceRole::Source));
        assert_eq!(
            policy_for(ExecutorKind::IntegrationReviewer).writable_roles,
            &[SourceRole::Document]
        );
        // Only Specification has a clarification checkpoint.
        for kind in ExecutorKind::ALL {
            assert_eq!(
                policy_for(kind).may_ask,
                kind == ExecutorKind::SpecificationAuthor,
                "{kind:?}"
            );
        }
        // The reviewer reads only host-selected context (ADR-032), and it is
        // the only specialty whose scope the transport enforces that way.
        // The others honestly declare the workspace snapshot their CLI reads
        // (ADR-034 T1) until a measurement moves one of them.
        for kind in ExecutorKind::ALL {
            assert_eq!(
                policy_for(kind).tools == ToolPolicy::HostContextOnly,
                kind == ExecutorKind::IntegrationReviewer,
                "{kind:?}"
            );
        }
        // No policy mentions publication, approval or shell in its deliverable.
        for kind in ExecutorKind::ALL {
            let brief = policy_for(kind).role_brief.to_ascii_lowercase();
            assert!(!brief.contains("statusupdate"));
            assert!(!brief.contains("relativepath"));
            assert!(!brief.contains("shell"));
        }
        assert!(common_rules().contains("Never invent handles"));
    }

    #[test]
    fn every_declared_tool_policy_is_the_scope_the_transport_requires() {
        // ADR-034 T1: one table in the shared contracts says which scope the
        // transport accepts for each specialty; a policy that disagrees would
        // be refused at dispatch, so the two can never drift apart silently.
        for kind in ExecutorKind::ALL {
            assert_eq!(
                Some(policy_for(kind).tools.context_scope()),
                UnitContextScope::required_for(kind.as_str()),
                "{kind:?}"
            );
        }
        assert_eq!(
            ToolPolicy::ReadOnlyScoped.context_scope(),
            UnitContextScope::Workspace
        );
        assert_eq!(
            ToolPolicy::HostContextOnly.context_scope(),
            UnitContextScope::HostContextOnly
        );
        // The retired declarative policy no longer parses.
        assert!(serde_json::from_value::<ToolPolicy>(serde_json::json!("none")).is_err());
    }

    #[test]
    fn policy_digest_is_stable_and_sensitive_to_rules() {
        let first = recipe_policy_sha256().unwrap();
        let second = recipe_policy_sha256().unwrap();
        assert_eq!(first, second);
        let snapshot = serde_json::to_value(all_policies()).unwrap();
        assert_eq!(snapshot.as_array().unwrap().len(), 8);
        // The digest is over the serialized policies: any textual change moves it.
        let altered =
            canonical_sha256(&(RECIPE_POLICY_ID, "different", all_policies()), &[]).unwrap();
        assert_ne!(first, altered);
    }
}
