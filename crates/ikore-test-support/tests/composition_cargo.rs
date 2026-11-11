//! Qualified Cargo admission over the real v4 host pipeline. These tests use
//! an intentionally absent toolchain and a loopback model: freezing, policy
//! changes and unavailable receipts must not depend on a machine installation.
#![allow(
    clippy::unwrap_used,
    reason = "Integration fixtures fail immediately when host state is invalid"
)]

mod composition_support;

use composition_support::*;
use ikore_core::orchestration::composition::recipes::StagePayloadRecord;
use ikore_core::orchestration::composition::review_boundary::{
    Freshness, GapCode, GapScope, ReviewInputManifestV1,
};
use ikore_core::orchestration::composition::risk::{CoverageRowKind, CoverageState};
use ikore_core::orchestration::composition::settings::CompositionSettings;
use ikore_core::orchestration::composition::validation::CargoValidationPolicyV1;
use ikore_core::orchestration::composition::validation::candidate::load_baseline;
use ikore_core::orchestration::composition::validation::evidence::{CheckResult, ValidationPhase};
use ikore_core::orchestration::composition::validation::profile::detect_presets;
use ikore_core::orchestration::composition::{
    CompletedPayloadV1, ReceiptStatus, Stage, UnitModelResponseV1, Verdict,
};
use ikore_core::persistence_service::PersistenceService;
use ikore_test_support::IsolatedTestEnvironment;
use rusqlite::Connection;
use serde_json::Value;
use std::fs;
use std::sync::{Arc, Mutex};

const ORIGINAL_ASSET: &[u8] = &[0, 1, 2, 255];

fn cargo_workspace() -> IsolatedTestEnvironment {
    let env = IsolatedTestEnvironment::new();
    let root = env.workspace_dir();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("assets")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname=\"admission_fixture\"\nversion=\"0.1.0\"\nedition=\"2024\"\n[lib]\npath=\"src/auth.rs\"\n",
    )
    .unwrap();
    fs::write(
        root.join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"admission_fixture\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(root.join("src/auth.rs"), ORIGINAL_SOURCE).unwrap();
    fs::write(root.join("assets/logo.bin"), ORIGINAL_ASSET).unwrap();
    env
}

fn settings() -> CompositionSettings {
    CompositionSettings {
        enabled: true,
        validation_enabled: true,
        ..CompositionSettings::default()
    }
    .with_all_recipes_qualified()
}

#[derive(Clone)]
struct ProjectAccess {
    service: PersistenceService,
    project: String,
    pulse: String,
}

#[derive(Debug)]
struct FrozenObservation {
    execution: String,
    baseline_sha256: String,
    profiles_json: String,
}

#[test]
fn cargo_inputs_and_policy_are_frozen_before_dispatch_and_an_unavailable_run_keeps_evidence() {
    let env = cargo_workspace();
    let workspace = env.workspace_dir().to_path_buf();
    let data = env.data_dir().to_path_buf();
    let access: Arc<Mutex<Option<ProjectAccess>>> = Arc::default();
    let provider_access = access.clone();
    let observed: Arc<Mutex<Option<FrozenObservation>>> = Arc::default();
    let provider_observed = observed.clone();
    let provider_data = data.clone();
    let provider_workspace = workspace.clone();
    let harness = Harness::start_prepared(
        env,
        vec![expiry_task()],
        settings(),
        move |facts, _, _| {
            if facts.executor == "code_implementer" {
                let access = provider_access.lock().unwrap().clone().unwrap();
                let db = Connection::open(access.service.database_path()).unwrap();
                let (execution, baseline_sha256, profiles_json): (String, String, String) = db
                    .query_row(
                        "SELECT e.id,c.baseline_sha256,c.profiles_json
                         FROM stage_cargo_validation_contexts c JOIN stage_executions e ON e.id=c.execution_id
                         WHERE e.pulsation_id=?1 AND e.stage='Code' ORDER BY e.created_at DESC LIMIT 1",
                        [&access.pulse],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .unwrap();
                let baseline = load_baseline(
                    &provider_data
                        .join("stage_candidates")
                        .join(&execution)
                        .join("baseline"),
                )
                .unwrap();
                assert_eq!(baseline.candidate_sha256.as_str(), baseline_sha256);
                assert_eq!(
                    fs::read(baseline.dir.join("assets/logo.bin")).unwrap(),
                    ORIGINAL_ASSET
                );
                let frozen: Value = serde_json::from_str(&profiles_json).unwrap();
                assert_eq!(
                    frozen[0]["snapshot"]["cargoPolicy"]["additionalInputs"][0],
                    "assets/logo.bin"
                );

                // A configuration/input change during model execution cannot
                // replace this admitted baseline or its approved policy.
                fs::write(provider_workspace.join("assets/logo.bin"), [8, 9, 10]).unwrap();
                let mut profile = access
                    .service
                    .list_validation_profiles(&access.project)
                    .unwrap()
                    .remove(0)
                    .0;
                profile.timeout_seconds = 42;
                let mut policy = access
                    .service
                    .get_cargo_validation_policy(&access.project, "cargo_check")
                    .unwrap()
                    .unwrap();
                policy.additional_inputs.clear();
                access
                    .service
                    .save_cargo_validation_profile(&access.project, profile, policy, true)
                    .unwrap();
                *provider_observed.lock().unwrap() = Some(FrozenObservation {
                    execution,
                    baseline_sha256,
                    profiles_json,
                });
            }
            completed_reply(facts)
        },
        move |harness| {
            let profile = detect_presets(harness.workspace()).remove(0);
            let policy = CargoValidationPolicyV1 {
                additional_inputs: vec!["assets/logo.bin".into()],
                toolchain: Some("hvc-admission-fixture-not-installed".into()),
                ..CargoValidationPolicyV1::default()
            };
            harness
                .service
                .save_cargo_validation_profile(&harness.project, profile, policy, true)
                .unwrap();
            *access.lock().unwrap() = Some(ProjectAccess {
                service: harness.service.clone(),
                project: harness.project.clone(),
                pulse: harness.pulse.clone(),
            });
        },
    );
    for stage in ["SPECIFICATION", "Contract"] {
        harness.wait_for_checkpoint(stage);
        harness.approve();
    }
    wait_until(&harness.service, &harness.pulse, |db| {
        db.query_row(
            "SELECT status='failed' FROM pulsations WHERE id=?1",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap()
    });
    let observation = observed.lock().unwrap();
    let observation = observation
        .as_ref()
        .expect("Code received the frozen context before dispatch");
    let (sha, frozen): (String, String) = harness.db().query_row(
        "SELECT baseline_sha256,profiles_json FROM stage_cargo_validation_contexts WHERE execution_id=?1",
        [&observation.execution], |r| Ok((r.get(0)?, r.get(1)?)),
    ).unwrap();
    assert_eq!(sha, observation.baseline_sha256);
    assert_eq!(frozen, observation.profiles_json);
    let baseline = load_baseline(
        &data
            .join("stage_candidates")
            .join(&observation.execution)
            .join("baseline"),
    )
    .unwrap();
    assert_eq!(
        fs::read(baseline.dir.join("assets/logo.bin")).unwrap(),
        ORIGINAL_ASSET
    );
    assert_eq!(
        fs::read(workspace.join("assets/logo.bin")).unwrap(),
        [8, 9, 10]
    );
    assert!(
        harness
            .service
            .get_cargo_validation_policy(&harness.project, "cargo_check")
            .unwrap()
            .unwrap()
            .additional_inputs
            .is_empty()
    );

    let receipt_id: String = harness.db().query_row(
        "SELECT id FROM host_validation_runs WHERE execution_id=?1 AND baseline_receipt_id IS NULL",
        [&observation.execution], |r| r.get(0),
    ).unwrap();
    let receipt = harness
        .service
        .get_validation_receipt(&receipt_id)
        .unwrap()
        .unwrap();
    assert_eq!(receipt.status, ReceiptStatus::Unavailable);
    assert!(
        receipt.started_at.is_none()
            && receipt.finished_at.is_none()
            && receipt.exit_code.is_none()
    );
    let details = harness
        .service
        .get_validation_evidence(&receipt_id)
        .unwrap()
        .unwrap();
    assert_eq!(details.phase, ValidationPhase::Preparation);
    assert_eq!(details.result, CheckResult::Unknown);
    assert!(!details.qualifies());
    assert!(!workspace.join("target").exists());
    assert_eq!(
        fs::read_to_string(workspace.join("src/auth.rs")).unwrap(),
        ORIGINAL_SOURCE
    );
    assert_eq!(
        harness
            .calls()
            .iter()
            .filter(|executor| executor.as_str() == "code_implementer")
            .count(),
        1
    );
}

#[test]
fn a_new_cargo_command_without_an_approved_policy_stops_before_the_code_model_is_called() {
    let harness = Harness::start_prepared(
        cargo_workspace(),
        vec![expiry_task()],
        settings(),
        |facts, _, _| completed_reply(facts),
        |harness| {
            // The generic/legacy API cannot supply the separate approval.
            let profile = detect_presets(harness.workspace()).remove(0);
            harness
                .service
                .upsert_validation_profile(&harness.project, profile, true)
                .unwrap();
        },
    );
    for stage in ["SPECIFICATION", "Contract"] {
        harness.wait_for_checkpoint(stage);
        harness.approve();
    }
    wait_until(&harness.service, &harness.pulse, |db| {
        db.query_row(
            "SELECT EXISTS(SELECT 1 FROM stage_executions WHERE pulsation_id=?1 AND stage='Code' AND status IN ('parked','failed'))",
            [&harness.pulse], |r| r.get(0),
        ).unwrap()
    });
    assert!(
        !harness
            .calls()
            .iter()
            .any(|executor| executor == "code_implementer")
    );
    let (contexts, receipts): (i64, i64) = harness.db().query_row(
        "SELECT (SELECT COUNT(*) FROM stage_cargo_validation_contexts c JOIN stage_executions e ON e.id=c.execution_id WHERE e.pulsation_id=?1),
                (SELECT COUNT(*) FROM host_validation_runs r JOIN stage_executions e ON e.id=r.execution_id WHERE e.pulsation_id=?1)",
        [&harness.pulse], |r| Ok((r.get(0)?, r.get(1)?)),
    ).unwrap();
    assert_eq!((contexts, receipts), (0, 0));
}

fn completed_cargo_harness(mutate_asset_during_test: bool) -> Harness {
    let env = cargo_workspace();
    let workspace = env.workspace_dir().to_path_buf();
    let harness = Harness::start_prepared(
        env,
        vec![expiry_task()],
        settings(),
        move |facts, _, _| {
            if matches!(facts.executor.as_str(), "test_author" | "test_repairer") {
                if mutate_asset_during_test {
                    // The host has already captured the Test baseline. The
                    // successful check still proves those old asset bytes,
                    // and cannot qualify the now-different live candidate.
                    fs::write(workspace.join("assets/logo.bin"), [6, 7, 8]).unwrap();
                }
                let mut options = ReplyOptions::default();
                options.contents.insert(
                    "tests/auth_test.rs".into(),
                    format!("use admission_fixture as auth;\n\n{AUTHORED_TEST}"),
                );
                let mut response = unit_response_with(facts, &options);
                // Bind the criterion to the actual compile check. Otherwise
                // an older stale Code receipt would have no affected
                // criterion and could not exercise verdict normalization.
                for coverage in response["payload"]["coverage"].as_array_mut().unwrap() {
                    coverage["checkRefs"] = serde_json::json!(["cargo_check"]);
                }
                provider_reply(response)
            } else {
                completed_reply(facts)
            }
        },
        |harness| {
            let profile = detect_presets(harness.workspace()).remove(0);
            harness
                .service
                .save_cargo_validation_profile(
                    &harness.project,
                    profile,
                    CargoValidationPolicyV1 {
                        additional_inputs: vec!["assets/logo.bin".into()],
                        ..CargoValidationPolicyV1::default()
                    },
                    true,
                )
                .unwrap();
        },
    );
    for stage in ["SPECIFICATION", "Contract", "Test"] {
        harness.wait_for_checkpoint(stage);
        harness.approve();
    }
    harness.wait_for_checkpoint("Integrator");
    harness
}

fn integrator_manifest(harness: &Harness) -> ReviewInputManifestV1 {
    let manifest: String = harness
        .db()
        .query_row(
            "SELECT r.manifest_json FROM integrator_review_inputs r
             JOIN stage_executions e ON e.id=r.stage_execution_id
             WHERE e.pulsation_id=?1 AND e.stage='Integrator' ORDER BY r.created_at DESC LIMIT 1",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap();
    serde_json::from_str(&manifest).unwrap()
}

fn integrator_verdicts(harness: &Harness) -> Vec<Verdict> {
    let payload: String = harness
        .db()
        .query_row(
            "SELECT content FROM artifacts WHERE pulsation_id=?1 AND kind='stage_payload_v1' AND upper(producer_stage)='INTEGRATOR' ORDER BY created_at DESC LIMIT 1",
            [&harness.pulse],
            |row| row.get(0),
        )
        .unwrap();
    let record: StagePayloadRecord = serde_json::from_str(&payload).unwrap();
    record
        .responses()
        .flat_map(|response| match response {
            UnitModelResponseV1::Completed {
                payload: CompletedPayloadV1::Integration(integration),
                ..
            } => integration
                .assessments
                .iter()
                .map(|assessment| assessment.verdict)
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .collect()
}

fn assert_criterion_coverage(harness: &Harness, expected: CoverageState) {
    let status = harness
        .service
        .checkpoint_risk_status(&harness.pulse)
        .unwrap();
    let matrix = status
        .matrix
        .expect("the accepted Integrator records coverage");
    let criterion = matrix
        .rows
        .iter()
        .find(|row| row.kind == CoverageRowKind::Criterion && row.subject.as_str() == "crit_1_1")
        .expect("the task criterion appears in the coverage matrix");
    assert_eq!(criterion.state, expected, "{criterion:?}");
}

#[test]
fn the_integrator_keeps_the_final_cargo_receipt_fresh_after_publishing_the_test_report() {
    let harness = completed_cargo_harness(false);
    let manifest = integrator_manifest(&harness);
    let check = manifest
        .checks
        .iter()
        .find(|check| check.stage == Stage::Test && check.check_ref.as_str() == "cargo_check")
        .expect("the Integrator receives the actual final Test compile receipt");
    assert_eq!(check.status, "passed", "{check:?}");
    assert_eq!(check.freshness, Freshness::Fresh, "{check:?}");
    assert_eq!(check.interpretation.as_deref(), Some("supported"));
    assert_eq!(check.result.as_deref(), Some("passed"));
    let earlier = manifest
        .checks
        .iter()
        .find(|check| check.stage == Stage::Code && check.check_ref.as_str() == "cargo_check")
        .unwrap();
    assert_eq!(earlier.freshness, Freshness::Stale, "{earlier:?}");
    assert!(!manifest.gaps.iter().any(|gap| {
        gap.code == GapCode::CheckStale
            && gap.limits_verdicts()
            && matches!(&gap.scope, GapScope::Checks(checks) if checks.iter().any(|check| check.as_str() == "cargo_check"))
    }), "a fresh final receipt supersedes the earlier Code evidence: {manifest:?}");
    assert_eq!(integrator_verdicts(&harness), [Verdict::Supported]);
    assert_criterion_coverage(&harness, CoverageState::Executed);
    let details = harness
        .service
        .get_validation_evidence(check.receipt_id.as_deref().unwrap())
        .unwrap()
        .unwrap();
    assert!(details.qualifies(), "{details:?}");
    let report: String = harness
        .db()
        .query_row(
            "SELECT relative_path FROM artifacts WHERE pulsation_id=?1 AND kind='validation_report' ORDER BY created_at DESC LIMIT 1",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap();
    assert!(harness.workspace().join(report).is_file());
    assert!(harness.workspace().join("tests/auth_test.rs").is_file());
    assert!(!harness.workspace().join("target").exists());
}

#[test]
fn a_successful_cargo_receipt_does_not_approve_an_asset_changed_outside_the_frozen_candidate() {
    let harness = completed_cargo_harness(true);
    let manifest = integrator_manifest(&harness);
    let check = manifest
        .checks
        .iter()
        .find(|check| check.stage == Stage::Test && check.check_ref.as_str() == "cargo_check")
        .unwrap();
    assert_eq!(check.status, "passed", "{check:?}");
    assert_eq!(check.freshness, Freshness::Stale, "{check:?}");
    assert_eq!(check.result.as_deref(), Some("passed"));
    assert!(manifest.gaps.iter().any(|gap| {
        gap.code == GapCode::CheckStale
            && matches!(&gap.scope, GapScope::Checks(checks) if checks.iter().any(|check| check.as_str() == "cargo_check"))
    }), "{manifest:?}");
    assert_eq!(integrator_verdicts(&harness), [Verdict::Unverified]);
    assert_criterion_coverage(&harness, CoverageState::Unverified);
    assert_eq!(
        fs::read(harness.workspace().join("assets/logo.bin")).unwrap(),
        [6, 7, 8]
    );
    let details = harness
        .service
        .get_validation_evidence(check.receipt_id.as_deref().unwrap())
        .unwrap()
        .unwrap();
    assert!(
        details.qualifies(),
        "the recorded execution was valid against its frozen inputs: {details:?}"
    );
}
