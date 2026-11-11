#![allow(
    clippy::unwrap_used,
    reason = "Test-only integration helpers fail immediately when fixture setup is invalid"
)]
//! Host checks inside a composed stage (ADR-022, M4; SC-011, SC-012,
//! SC-022): a configured command runs against the assembled candidate
//! after every unit validated; a required failure attributable to one
//! unit's owned files gets one repair by the repair specialty, a
//! non-attributable one parks the stage for a human with a baseline
//! classification, a missing tool is terminal, and disabled validation
//! reports `not_run` without blocking the human review.
mod composition_support;

use composition_support::*;
use ikore_core::orchestration::SchemaV1;
use ikore_core::orchestration::composition::settings::CompositionSettings;
use ikore_core::orchestration::composition::{CheckKind, Handle, Uuid, ValidationCommandProfileV1};
use ikore_test_support::IsolatedTestEnvironment;
use std::fs;

const FIXTURE: &str = env!("CARGO_BIN_EXE_check_fixture");
const BUGGY_SOURCE: &str =
    "pub fn validate_token(expires_at: u64, now: u64) -> bool {\n    expires_at >= now\n}\n";

fn fixture_profile(argv: &[&str], executable: &str) -> ValidationCommandProfileV1 {
    ValidationCommandProfileV1 {
        schema_version: SchemaV1,
        profile_id: Uuid::generate(),
        profile_version: 1,
        check_ref: Handle::new("fixture_compile").unwrap(),
        kind: CheckKind::Compile,
        required: true,
        executable: executable.to_owned(),
        argv: argv.iter().map(|s| (*s).to_owned()).collect(),
        cwd_relative_path: ".".into(),
        input_globs: vec!["**/*.rs".into()],
        environment_names: vec![],
        toolchain_identity: "fixture".into(),
        timeout_seconds: 30,
        max_log_bytes: 65_536,
    }
}

fn enable_validation(harness: &Harness, profile: ValidationCommandProfileV1, enabled: bool) {
    harness
        .service
        .upsert_validation_profile(&harness.project, profile, true)
        .unwrap();
    harness
        .service
        .update_composition_settings(
            &harness.project,
            CompositionSettings {
                enabled: true,
                validation_enabled: enabled,
                ..CompositionSettings::default()
            },
        )
        .unwrap();
}

fn runs(harness: &Harness, stage: &str) -> Vec<(String, String, Option<String>)> {
    harness
        .db()
        .prepare("SELECT r.check_ref,r.status,r.baseline_receipt_id FROM host_validation_runs r JOIN stage_executions e ON e.id=r.execution_id WHERE e.pulsation_id=?1 AND e.stage=?2 ORDER BY r.created_at,r.invocation_ordinal")
        .unwrap()
        .query_map([&harness.pulse, stage], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn env_with_source() -> IsolatedTestEnvironment {
    let env = IsolatedTestEnvironment::new();
    fs::create_dir_all(env.workspace_dir().join("src")).unwrap();
    fs::write(env.workspace_dir().join("src/auth.rs"), ORIGINAL_SOURCE).unwrap();
    env
}

#[test]
fn a_passing_required_check_runs_once_per_stage_candidate_and_lands_in_the_aggregate() {
    let harness = Harness::start_in(env_with_source(), vec![expiry_task()], |facts, _, _| {
        completed_reply(facts)
    });
    enable_validation(&harness, fixture_profile(&["pass"], FIXTURE), true);
    for stage in ["SPECIFICATION", "Contract", "Test", "Integrator"] {
        harness.wait_for_checkpoint(stage);
        harness.approve();
    }
    wait_until(&harness.service, &harness.pulse, |db| {
        db.query_row(
            "SELECT status='done' FROM pulsations WHERE id=?1",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap()
    });
    assert_eq!(harness.calls().len(), 6, "checks cost no model call");
    assert_eq!(
        runs(&harness, "Code"),
        vec![("fixture_compile".into(), "passed".into(), None)]
    );
    assert_eq!(
        runs(&harness, "Test"),
        vec![("fixture_compile".into(), "passed".into(), None)]
    );
    let db = harness.db();
    let plan_checks: String = db
        .query_row(
            "SELECT json_extract(plan_json,'$.checkRefs') FROM stage_executions WHERE pulsation_id=?1 AND stage='Code'",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        plan_checks, r#"["fixture_compile"]"#,
        "the frozen plan names the check"
    );
    let report: String = db
        .query_row(
            "SELECT content FROM artifacts WHERE pulsation_id=?1 AND kind='validation_report'",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap();
    assert!(report.contains("fixture_compile"), "{report}");
    assert!(report.to_ascii_lowercase().contains("passed"), "{report}");
    // A frozen snapshot of the profile exists and the candidate was staged
    // under the data directory, never in the workspace.
    let snapshots: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM composition_policy_snapshots WHERE kind='validation_profile'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(snapshots >= 1);
    assert!(!harness.workspace().join("build").exists());
}

#[test]
fn an_attributable_failure_gets_one_repair_by_the_repair_specialty_and_then_passes() {
    let harness = Harness::start_in(
        env_with_source(),
        vec![expiry_task()],
        |facts, _, request| {
            if facts.executor == "code_implementer" {
                let mut options = ReplyOptions::default();
                options
                    .contents
                    .insert("src/auth.rs".into(), BUGGY_SOURCE.into());
                return provider_reply(unit_response_with(facts, &options));
            }
            if facts.executor == "code_repairer" {
                assert!(
                    request.body.contains("## Correction"),
                    "the repair carries the diagnostics"
                );
                assert!(
                    request.body.contains("[fixture_compile]"),
                    "the check that failed is named"
                );
                assert!(
                    request.body.contains("expected `expires_at > now`"),
                    "the tool diagnostic reaches the repairer"
                );
            }
            completed_reply(facts)
        },
    );
    enable_validation(
        &harness,
        fixture_profile(
            &[
                "fail-unless",
                "src/auth.rs",
                "expires_at > now",
                "--message-format=json",
            ],
            FIXTURE,
        ),
        true,
    );
    for stage in ["SPECIFICATION", "Contract", "Test"] {
        harness.wait_for_checkpoint(stage);
        harness.approve();
    }
    assert_eq!(
        &harness.calls()[..5],
        [
            "intake_classifier",
            "specification_author",
            "contract_author",
            "code_implementer",
            "code_repairer"
        ]
    );
    let code_runs = runs(&harness, "Code");
    assert_eq!(code_runs.len(), 2, "{code_runs:?}");
    assert_eq!(code_runs[0].1, "failed");
    assert_eq!(code_runs[1].1, "passed");
    assert!(
        code_runs.iter().all(|(_, _, baseline)| baseline.is_none()),
        "no baseline run was needed"
    );
    let db = harness.db();
    let (generation, state, repairs): (i64, String, i64) = db
        .query_row(
            "SELECT u.generation,u.state,(SELECT COUNT(*) FROM stage_work_unit_attempts a WHERE a.unit_id=u.id AND a.executor_kind='code_repairer' AND a.state='succeeded') FROM stage_work_units u JOIN stage_executions e ON e.id=u.execution_id WHERE e.pulsation_id=?1 AND e.stage='Code'",
            [&harness.pulse],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!((generation, state.as_str(), repairs), (2, "validated", 1));
    let pools: Vec<String> = db
        .prepare("SELECT r.pool FROM stage_invocation_reservations r JOIN stage_executions e ON e.id=r.execution_id WHERE e.pulsation_id=?1 AND e.stage='Code' ORDER BY r.created_at")
        .unwrap()
        .query_map([&harness.pulse], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        pools,
        ["initial", "extra"],
        "the repair spends the unit's extra permit"
    );
    assert_eq!(
        fs::read_to_string(harness.workspace().join("src/auth.rs")).unwrap(),
        IMPLEMENTED_SOURCE,
        "the repaired output is what gets published"
    );
}

#[test]
fn a_non_attributable_failure_parks_for_a_human_with_a_baseline_classification() {
    let harness = Harness::start_in(env_with_source(), vec![expiry_task()], |facts, _, _| {
        completed_reply(facts)
    });
    enable_validation(&harness, fixture_profile(&["fail-text"], FIXTURE), true);
    harness.wait_for_checkpoint("SPECIFICATION");
    harness.approve();
    harness.wait_for_checkpoint("Contract");
    harness.approve();
    let checkpoint = harness.wait_for_failure_checkpoint();
    assert_eq!(
        harness.calls().len(),
        4,
        "no repair is attempted without an owner for the diagnostic"
    );
    let db = harness.db();
    let (class, allow_retry, target, diagnostic): (String, bool, Option<String>, String) = db
        .query_row(
            "SELECT failure_class,allow_retry,revision_stage,diagnostic FROM stage_failure_requests WHERE checkpoint_id=?1",
            [&checkpoint],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(class, "validation_failed");
    assert!(
        !allow_retry,
        "an identical rerun cannot pass the same check"
    );
    assert_eq!(target.as_deref(), Some("Code"));
    assert!(diagnostic.contains("fixture_compile"), "{diagnostic}");
    assert!(
        diagnostic.contains("(pre_existing)"),
        "the baseline fails the same way: {diagnostic}"
    );
    let code_runs = runs(&harness, "Code");
    assert_eq!(code_runs.len(), 2, "{code_runs:?}");
    assert_eq!(code_runs[0].1, "failed");
    assert!(
        code_runs[1].2.is_some(),
        "the baseline run links the candidate receipt it explains"
    );
    let status: String = db
        .query_row(
            "SELECT status FROM stage_executions WHERE pulsation_id=?1 AND stage='Code'",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(status, "parked");
    assert_eq!(
        fs::read_to_string(harness.workspace().join("src/auth.rs")).unwrap(),
        ORIGINAL_SOURCE,
        "nothing is published from a failed candidate"
    );
}

#[test]
fn a_missing_tool_is_terminal_and_disabled_validation_reports_not_run() {
    let harness = Harness::start_in(env_with_source(), vec![expiry_task()], |facts, _, _| {
        completed_reply(facts)
    });
    enable_validation(
        &harness,
        fixture_profile(&["pass"], "hvc-tool-that-does-not-exist"),
        true,
    );
    harness.wait_for_checkpoint("SPECIFICATION");
    harness.approve();
    harness.wait_for_checkpoint("Contract");
    harness.approve();
    wait_until(&harness.service, &harness.pulse, |db| {
        db.query_row(
            "SELECT status='failed' FROM pulsations WHERE id=?1",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap()
    });
    let db = harness.db();
    let (status, code): (String, Option<String>) = db
        .query_row(
            "SELECT status,blocked_code FROM stage_executions WHERE pulsation_id=?1 AND stage='Code'",
            [&harness.pulse],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (status.as_str(), code.as_deref()),
        ("failed", Some("validation_unavailable"))
    );
    assert_eq!(runs(&harness, "Code")[0].1, "unavailable");
    drop(db);

    // Same project layout, validation configured but disabled.
    let harness = Harness::start_in(env_with_source(), vec![expiry_task()], |facts, _, _| {
        completed_reply(facts)
    });
    enable_validation(&harness, fixture_profile(&["fail-text"], FIXTURE), false);
    for stage in ["SPECIFICATION", "Contract", "Test", "Integrator"] {
        harness.wait_for_checkpoint(stage);
        harness.approve();
    }
    wait_until(&harness.service, &harness.pulse, |db| {
        db.query_row(
            "SELECT status='done' FROM pulsations WHERE id=?1",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap()
    });
    assert!(
        runs(&harness, "Code").is_empty(),
        "nothing runs while validation is disabled"
    );
    let report: String = harness
        .db()
        .query_row(
            "SELECT content FROM artifacts WHERE pulsation_id=?1 AND kind='validation_report'",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        report.contains("| `fixture_compile` | yes | not run | — |"),
        "{report}"
    );
}

/// The detected Cargo preset against a real crate: `cargo check --offline`
/// runs inside the disposable candidate with a host-owned target cache and
/// never touches the project's own `target`. Needs the cargo that runs this
/// suite.
#[test]
fn the_cargo_check_preset_validates_a_real_crate_candidate_offline() {
    use ikore_core::orchestration::composition::validation::CargoValidationPolicyV1;

    let env = IsolatedTestEnvironment::new();
    let root = env.workspace_dir().to_path_buf();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"fixture_crate\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\npath = \"src/lib.rs\"\n",
    )
    .unwrap();
    fs::write(root.join("src/lib.rs"), "pub mod auth;\n").unwrap();
    fs::write(root.join("src/auth.rs"), ORIGINAL_SOURCE).unwrap();
    fs::write(
        root.join("Cargo.lock"),
        "# This file is automatically @generated by Cargo.\nversion = 4\n\n[[package]]\nname = \"fixture_crate\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    let data_dir = env.data_dir().to_path_buf();
    let harness = Harness::start_in(env, vec![expiry_task()], |facts, _, _| {
        if matches!(facts.executor.as_str(), "test_author" | "test_repairer") {
            let mut options = ReplyOptions::default();
            options.contents.insert(
                "tests/auth_test.rs".into(),
                format!("use fixture_crate::auth;\n\n{AUTHORED_TEST}"),
            );
            provider_reply(unit_response_with(facts, &options))
        } else {
            completed_reply(facts)
        }
    });
    let presets = harness
        .service
        .detect_validation_presets(&harness.project)
        .unwrap();
    let check = presets
        .into_iter()
        .find(|preset| preset.check_ref.as_str() == "cargo_check")
        .expect("cargo preset detected from Cargo.toml");
    enable_validation(&harness, check.clone(), true);
    harness
        .service
        .save_cargo_validation_profile(
            &harness.project,
            check,
            CargoValidationPolicyV1::default(),
            true,
        )
        .unwrap();
    for stage in ["SPECIFICATION", "Contract"] {
        harness.wait_for_checkpoint(stage);
        harness.approve();
    }
    harness.wait_for_checkpoint("Test");
    let code_runs = runs(&harness, "Code");
    assert_eq!(
        code_runs,
        vec![("cargo_check".into(), "passed".into(), None)],
        "{code_runs:?}"
    );
    let receipt: String = harness
        .db()
        .query_row(
            "SELECT r.receipt_json FROM host_validation_runs r JOIN stage_executions e ON e.id=r.execution_id WHERE e.pulsation_id=?1 AND e.stage='Code'",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap();
    let receipt: serde_json::Value = serde_json::from_str(&receipt).unwrap();
    assert_eq!(receipt["exitCode"], 0);
    let details = harness
        .service
        .get_validation_evidence(receipt["receiptId"].as_str().unwrap())
        .unwrap()
        .expect("the qualified run stores executable evidence beside the V1 receipt");
    assert!(details.qualifies(), "{details:?}");
    assert!(
        receipt["stdout"]["text"]
            .as_str()
            .unwrap()
            .contains("compiler-artifact"),
        "{receipt}"
    );
    assert!(
        data_dir.join("stage_candidates").join("cache").exists(),
        "the build cache lives under the data dir"
    );
    assert!(
        !root.join("target").exists(),
        "the project's own target directory is never created"
    );
    assert_eq!(
        fs::read_to_string(root.join("src/auth.rs")).unwrap(),
        IMPLEMENTED_SOURCE
    );
}
