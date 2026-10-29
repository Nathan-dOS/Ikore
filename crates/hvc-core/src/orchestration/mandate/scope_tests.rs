use std::collections::BTreeSet;

use super::tests::{grant, issue, issued_by};
use super::*;
use crate::orchestration::SchemaV1;
use crate::orchestration::composition::references::ReferenceTable;
use crate::orchestration::composition::{
    ExecutorKind, Handle, ParentFenceV1, RelPath, Risk, Sha256Hex, SourceRole, Stage,
    StageBudgetV1, StageExecutionPlanV1, Uuid, WorkUnitSpecV1,
};

fn h(value: &str) -> Handle {
    Handle::new(value).unwrap()
}

/// A Code stage of two units. `alpha` owns `src/auth.rs` and reads every
/// other file; `beta` owns `src/beta.rs`. Each entry's handle is in
/// `handles` by path.
struct Fixture {
    plan: StageExecutionPlanV1,
    handles: std::collections::BTreeMap<&'static str, Handle>,
}

const FILES: &[(&str, SourceRole)] = &[
    ("src/auth.rs", SourceRole::Source),
    ("src/util.rs", SourceRole::Source),
    ("Cargo.toml", SourceRole::Configuration),
    ("src/beta.rs", SourceRole::Source),
    ("tests/auth_test.rs", SourceRole::Test),
    ("config/.env.local", SourceRole::Configuration),
    ("REVIEW.md", SourceRole::Document),
    (
        "knowledge/obsidian/features/expiry.md",
        SourceRole::Document,
    ),
    ("src/published.rs", SourceRole::Source),
];

fn fixture(stage: Stage) -> Fixture {
    let mut builder = ReferenceTable::builder();
    builder
        .add_task("t-1", 1, "One", &[("AC-1".into(), "a".into())])
        .unwrap();
    let mut handles = std::collections::BTreeMap::new();
    for (path, role) in FILES {
        let handle = builder
            .add_existing(
                RelPath::new(*path).unwrap(),
                Sha256Hex::of(path.as_bytes()),
                *role,
            )
            .unwrap();
        handles.insert(*path, handle);
    }
    let candidate = builder
        .add_candidate(
            RelPath::new("src/sibling.rs").unwrap(),
            Sha256Hex::of(b"sibling"),
            Uuid::generate(),
            SourceRole::Source,
        )
        .unwrap();
    handles.insert("src/sibling.rs", candidate);
    let planned = builder
        .add_planned(RelPath::new("src/new.rs").unwrap(), SourceRole::Source)
        .unwrap();
    handles.insert("src/new.rs", planned);
    let table = builder.build();
    let kind = if stage == Stage::Test {
        ExecutorKind::TestAuthor
    } else {
        ExecutorKind::CodeImplementer
    };
    let unit = |key: &str, owned: Vec<Handle>, inputs: Vec<Handle>| WorkUnitSpecV1 {
        schema_version: SchemaV1,
        unit_id: Uuid::generate(),
        logical_unit_key: h(key),
        stage,
        executor_kind: kind,
        task_refs: vec![h("task_1")],
        criterion_refs: vec![h("crit_1_1")],
        dependencies: vec![],
        input_refs: inputs,
        owned_destination_refs: owned,
        required_check_refs: vec![],
        objective: "Implement.".into(),
        max_output_tokens: 100,
    };
    let alpha_owned = if stage == Stage::Test {
        handles["tests/auth_test.rs"].clone()
    } else {
        handles["src/auth.rs"].clone()
    };
    let alpha_inputs = handles
        .iter()
        .filter(|(path, _)| **path != "src/new.rs")
        .map(|(_, handle)| handle.clone())
        .collect();
    let plan = StageExecutionPlanV1 {
        schema_version: SchemaV1,
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
        stage,
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
            unit("alpha", vec![alpha_owned], alpha_inputs),
            unit("beta", vec![handles["src/beta.rs"].clone()], vec![]),
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
    Fixture { plan, handles }
}

struct Ask<'a> {
    refs: Vec<Handle>,
    mandate: Option<(&'a ExecutionMandateV1, &'a str)>,
    earlier: usize,
    budget: bool,
}

fn ask<'a>(fixture: &Fixture, paths: &[&str]) -> Ask<'a> {
    Ask {
        refs: paths
            .iter()
            .map(|path| fixture.handles[path].clone())
            .collect(),
        mandate: None,
        earlier: 0,
        budget: true,
    }
}

fn derive(fixture: &Fixture, ask: &Ask<'_>) -> ScopeRequestDerivationV1 {
    let documents = BTreeSet::from(["knowledge/obsidian/features/expiry.md".to_owned()]);
    let foreign = BTreeSet::from(["src/published.rs".to_owned()]);
    let sensitive = |path: &str| path.contains("auth");
    derive_scope_request(
        &ScopeFacts {
            plan: &fixture.plan,
            unit_key: "alpha",
            related_refs: &ask.refs,
            stage_documents: &documents,
            foreign_paths: &foreign,
            mandate: ask.mandate,
            earlier_requests: ask.earlier,
            budget_allows: ask.budget,
            risk_before: Risk::Low,
            is_sensitive: &sensitive,
        },
        |accepted| {
            if accepted.iter().any(|path| path.sensitive) {
                Risk::High
            } else {
                Risk::Medium
            }
        },
    )
    .unwrap()
}

#[test]
fn each_refusal_rule_names_its_code() {
    let code = Fixture::code();
    for (path, expected) in [
        ("src/new.rs", ScopeRefusalCode::ScopeRefNotDeliveredFile),
        ("tests/auth_test.rs", ScopeRefusalCode::ScopeRoleNotWritable),
        ("config/.env.local", ScopeRefusalCode::ScopeProtectedPath),
        ("REVIEW.md", ScopeRefusalCode::ScopeProtectedPath),
        (
            "knowledge/obsidian/features/expiry.md",
            ScopeRefusalCode::ScopeStageDocument,
        ),
        ("src/published.rs", ScopeRefusalCode::ScopeForeignCandidate),
        ("src/sibling.rs", ScopeRefusalCode::ScopeForeignCandidate),
        ("src/beta.rs", ScopeRefusalCode::ScopeOwnedByOtherUnit),
        ("src/auth.rs", ScopeRefusalCode::ScopeAlreadyGranted),
    ] {
        let derived = derive(&code, &ask(&code, &[path]));
        assert!(derived.accepted.is_empty(), "{path}: {derived:?}");
        assert_eq!(derived.refused.len(), 1, "{path}");
        assert_eq!(derived.refused[0].code, expected, "{path}");
        assert!(!derived.grantable(), "{path}");
        assert_eq!(
            derived.request_refusal, None,
            "nothing accepted means no request at all"
        );
        assert_eq!(derived.risk_after, derived.risk_before);
    }
}

impl Fixture {
    fn code() -> Self {
        fixture(Stage::Code)
    }
}

#[test]
fn a_mixed_request_keeps_only_the_admissible_files_and_their_delivered_bytes() {
    let code = Fixture::code();
    let mut request = ask(
        &code,
        &[
            "src/util.rs",
            "src/beta.rs",
            "Cargo.toml",
            "REVIEW.md",
            "src/util.rs",
        ],
    );
    request.refs.push(h("not_a_ref"));
    let derived = derive(&code, &request);
    assert!(derived.grantable());
    assert_eq!(
        derived
            .accepted
            .iter()
            .map(|path| path.path.as_str())
            .collect::<Vec<_>>(),
        ["Cargo.toml", "src/util.rs"],
        "sorted by identity, a repeated ref counted once"
    );
    assert_eq!(
        derived.accepted[1].sha256,
        Sha256Hex::of(b"src/util.rs").as_str()
    );
    assert_eq!(derived.accepted[1].role, SourceRole::Source);
    assert_eq!(
        derived
            .refused
            .iter()
            .map(|refusal| refusal.code)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            ScopeRefusalCode::ScopeOwnedByOtherUnit,
            ScopeRefusalCode::ScopeProtectedPath,
            ScopeRefusalCode::ScopeRefNotDeliveredFile,
        ])
    );
    assert_eq!(derived.unit_key, "alpha");
    assert_eq!(derived.stage, Stage::Code);
}

#[test]
fn the_request_is_offered_within_its_limits_and_the_budget_only() {
    let code = Fixture::code();
    let base = ask(&code, &["src/util.rs"]);
    assert!(derive(&code, &base).grantable());
    let mut again = ask(&code, &["src/util.rs"]);
    again.earlier = MAX_SCOPE_REQUESTS - 1;
    assert!(derive(&code, &again).grantable(), "the second request");
    again.earlier = MAX_SCOPE_REQUESTS;
    let third = derive(&code, &again);
    assert_eq!(
        third.request_refusal,
        Some(ScopeRefusalCode::ScopeRequestLimit)
    );
    assert!(!third.grantable());
    let mut broke = ask(&code, &["src/util.rs"]);
    broke.budget = false;
    assert_eq!(
        derive(&code, &broke).request_refusal,
        Some(ScopeRefusalCode::ScopeBudgetInsufficient)
    );

    // Nine grantable files: one more than a request may carry.
    let mut wide = fixture(Stage::Code);
    let mut builder = ReferenceTable::builder();
    builder
        .add_task("t-1", 1, "One", &[("AC-1".into(), "a".into())])
        .unwrap();
    let mut refs = Vec::new();
    for index in 0..=MAX_SCOPE_PATHS {
        refs.push(
            builder
                .add_existing(
                    RelPath::new(format!("src/extra_{index}.rs")).unwrap(),
                    Sha256Hex::of(b"x"),
                    SourceRole::Source,
                )
                .unwrap(),
        );
    }
    wide.plan.references = builder.build().entries().to_vec();
    wide.plan.units[0].input_refs = refs.clone();
    wide.plan.units[0].owned_destination_refs = vec![];
    wide.plan.units[1].owned_destination_refs = vec![];
    let many = Ask {
        refs: refs.clone(),
        mandate: None,
        earlier: 0,
        budget: true,
    };
    let derived = derive(&wide, &many);
    assert_eq!(derived.accepted.len(), MAX_SCOPE_PATHS + 1);
    assert_eq!(
        derived.request_refusal,
        Some(ScopeRefusalCode::ScopePathLimit)
    );
    let fits = Ask {
        refs: refs[..MAX_SCOPE_PATHS].to_vec(),
        ..many
    };
    assert!(derive(&wide, &fits).grantable(), "exactly eight fit");
}

#[test]
fn the_risk_after_counts_the_granted_files() {
    let code = Fixture::code();
    let plain = derive(&code, &ask(&code, &["src/util.rs"]));
    assert_eq!(plain.risk_before, Risk::Low);
    assert_eq!(plain.risk_after, Risk::Medium);
    assert!(!plain.accepted[0].sensitive);
}

#[test]
fn a_test_unit_is_granted_test_files_only() {
    let test = fixture(Stage::Test);
    for path in ["src/util.rs", "Cargo.toml", "src/auth.rs"] {
        let derived = derive(&test, &ask(&test, &[path]));
        assert_eq!(
            derived.refused[0].code,
            ScopeRefusalCode::ScopeRoleNotWritable,
            "{path}"
        );
    }
    assert_eq!(
        derive(&test, &ask(&test, &["tests/auth_test.rs"])).refused[0].code,
        ScopeRefusalCode::ScopeAlreadyGranted
    );
}

#[test]
fn only_code_and_test_take_scope_requests() {
    let mut other = Fixture::code();
    other.plan.stage = Stage::Integrator;
    let documents = BTreeSet::new();
    let sensitive = |_: &str| false;
    assert!(
        derive_scope_request(
            &ScopeFacts {
                plan: &other.plan,
                unit_key: "alpha",
                related_refs: &[],
                stage_documents: &documents,
                foreign_paths: &documents,
                mandate: None,
                earlier_requests: 0,
                budget_allows: true,
                risk_before: Risk::Low,
                is_sensitive: &sensitive,
            },
            |_| Risk::Low,
        )
        .is_none()
    );
}

#[test]
fn a_granted_path_becomes_an_amendment_of_the_unit_and_is_not_asked_again() {
    let code = Fixture::code();
    let mandate = issue();
    let sha = mandate.sha256().unwrap();
    let derived = derive(
        &code,
        &Ask {
            mandate: Some((&mandate, sha.as_str())),
            ..ask(&code, &["src/util.rs"])
        },
    );
    assert_eq!(derived.mandate_revision, Some(1));
    assert_eq!(derived.mandate_sha256.as_deref(), Some(sha.as_str()));
    let amended = amend_scope(
        &mandate,
        sha.as_str(),
        "alpha",
        &["src/util.rs".to_owned(), "src/auth.rs".to_owned()],
        issued_by("cp-failure"),
    );
    assert_eq!(amended.revision, 2);
    assert_eq!(amended.cause, MandateCause::ScopeGrant);
    assert_eq!(amended.previous_sha256.as_deref(), Some(sha.as_str()));
    let new = grant(&amended, "src/util.rs");
    assert_eq!(new.origins, [WriteClass::Amendment]);
    assert_eq!(new.unit_key.as_deref(), Some("alpha"));
    assert_eq!(new.amendment_revision, Some(2));
    let kept = grant(&amended, "src/auth.rs");
    assert!(kept.origins.contains(&WriteClass::Amendment));
    assert!(
        kept.origins.len() > 1,
        "the path keeps the origins it already had"
    );
    let keys = amended
        .scope
        .grants
        .iter()
        .map(|grant| path_key(&grant.path))
        .collect::<Vec<_>>();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted);

    // The next plan gives the unit the granted file.
    let mut regenerated = Fixture::code();
    let util = regenerated.handles["src/util.rs"].clone();
    regenerated.plan.units[0].owned_destination_refs.push(util);
    let amended_sha = amended.sha256().unwrap();
    let after = derive(
        &regenerated,
        &Ask {
            mandate: Some((&amended, amended_sha.as_str())),
            ..ask(&regenerated, &["src/util.rs"])
        },
    );
    assert_eq!(after.refused[0].code, ScopeRefusalCode::ScopeAlreadyGranted);
    assert_ne!(
        after.sha256().unwrap(),
        derived.sha256().unwrap(),
        "a new mandate revision changes the derivation"
    );
    assert_eq!(
        derived.sha256().unwrap(),
        derive(
            &code,
            &Ask {
                mandate: Some((&mandate, sha.as_str())),
                ..ask(&code, &["src/util.rs"])
            }
        )
        .sha256()
        .unwrap(),
        "the same facts give the same digest"
    );
}
