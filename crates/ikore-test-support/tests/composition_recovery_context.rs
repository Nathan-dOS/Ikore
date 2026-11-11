#![allow(
    clippy::unwrap_used,
    reason = "Isolated integration fixture assertions"
)]
//! Guided recovery I1: actual host failures reach the human-selected revision
//! without making the operator copy diagnostics or promoting tool text to instructions.
mod composition_support;

use composition_support::*;
use ikore_core::orchestration::SchemaV1;
use ikore_core::orchestration::composition::{CheckKind, Handle, Uuid, ValidationCommandProfileV1};
use ikore_core::persistence_service::{ResolveStageFailureRequest, StageFailureDecision};
use serde_json::Value;
use std::fs;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

fn failed_test() -> Harness {
    let env = ikore_test_support::IsolatedTestEnvironment::new();
    fs::create_dir_all(env.workspace_dir().join("src")).unwrap();
    fs::write(env.workspace_dir().join("src/auth.rs"), ORIGINAL_SOURCE).unwrap();
    let harness = Harness::start_in(env, vec![expiry_task()], |facts, _, _| {
        completed_reply(facts)
    });
    let mut settings = harness
        .service
        .get_composition_settings(&harness.project)
        .unwrap();
    settings.validation_enabled = true;
    harness
        .service
        .update_composition_settings(&harness.project, settings)
        .unwrap();
    for name in ["fixture_tests", "fixture_other_tests"] {
        harness
            .service
            .upsert_validation_profile(
                &harness.project,
                ValidationCommandProfileV1 {
                    schema_version: SchemaV1,
                    profile_id: Uuid::generate(),
                    profile_version: 1,
                    check_ref: Handle::new(name).unwrap(),
                    kind: CheckKind::Test,
                    required: true,
                    executable: env!("CARGO_BIN_EXE_check_fixture").into(),
                    argv: vec!["fail-text".into()],
                    cwd_relative_path: ".".into(),
                    input_globs: vec!["**/*.rs".into()],
                    environment_names: vec![],
                    toolchain_identity: "fixture".into(),
                    timeout_seconds: 30,
                    max_log_bytes: 65_536,
                },
                true,
            )
            .unwrap();
    }
    for stage in ["SPECIFICATION", "Contract"] {
        harness.wait_for_checkpoint(stage);
        harness.approve();
    }
    harness.wait_for_failure_checkpoint();
    harness
}

#[test]
fn all_failed_checks_and_unpublished_tests_are_preserved_with_the_checkpoint() {
    let harness = failed_test();
    let checkpoint = harness.wait_for_failure_checkpoint();
    let detail = serde_json::to_value(
        harness
            .service
            .read_stage_failure_detail(&checkpoint)
            .unwrap(),
    )
    .unwrap();
    let context = &detail["validationContext"];
    assert!(
        context.is_object(),
        "a failed check needs inspectable recovery evidence: {detail}"
    );
    assert_eq!(context["schemaVersion"], 1);
    assert_eq!(context["checks"].as_array().unwrap().len(), 2);
    for check in context["checks"].as_array().unwrap() {
        assert_eq!(check["status"], "failed");
        assert_eq!(check["required"], true);
        assert!(
            check["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d.as_str().unwrap().contains("fixture failure"))
        );
    }
    assert!(!context["requirements"].as_array().unwrap().is_empty());
    let preserved = context["tests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|test| test["path"] == "tests/auth_test.rs")
        .unwrap();
    assert_eq!(preserved["content"], AUTHORED_TEST);
    assert_eq!(preserved["sha256"].as_str().unwrap().len(), 64);
    assert!(
        !harness.workspace().join("tests/auth_test.rs").exists(),
        "failed Test output stays unpublished"
    );
    assert_eq!(
        harness.calls().len(),
        5,
        "capturing evidence spends no model invocation"
    );
    let stored: String = harness
        .db()
        .query_row(
            "SELECT context_json FROM validation_failure_contexts WHERE checkpoint_id=?1",
            [&checkpoint],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(serde_json::from_str::<Value>(&stored).unwrap(), *context);
    harness
        .db()
        .execute(
            "UPDATE validation_failure_contexts SET context_json='{}' WHERE checkpoint_id=?1",
            [&checkpoint],
        )
        .expect_err("failure evidence is immutable");
}

#[test]
fn an_empty_guidance_revision_receives_failure_evidence_after_restart() {
    let harness = failed_test();
    let checkpoint = harness.wait_for_failure_checkpoint();
    let version = harness.state_version();
    let stopped = harness.stop();
    let original: String = rusqlite::Connection::open(stopped.service.database_path())
        .unwrap()
        .query_row(
            "SELECT context_json FROM validation_failure_contexts WHERE checkpoint_id=?1",
            [&checkpoint],
            |row| row.get(0),
        )
        .unwrap();
    stopped
        .service
        .resolve_stage_failure(ResolveStageFailureRequest {
            checkpoint_id: checkpoint.clone(),
            expected_state_version: version,
            decision: StageFailureDecision::RequestRevision,
            feedback: None,
        })
        .unwrap();
    let seen = Arc::new(AtomicBool::new(false));
    let observed = seen.clone();
    let harness = stopped.restart(move |facts, _, request| {
        if facts.executor == "code_implementer" {
            assert!(request.body.contains("Previous validation failure"));
            assert!(request.body.contains("fixture failure"));
            assert!(request.body.contains("fixture_other_tests"));
            assert!(request.body.contains("expired_token_is_rejected"));
            assert!(request.body.contains("provenance: tool_output"));
            assert!(request.body.contains("provenance: agent_output"));
            observed.store(true, Ordering::SeqCst);
        }
        if facts.executor == "test_author" {
            assert!(!request.body.contains("## Previous validation failure"), "an unrelated downstream job does not inherit the recovery job's authority or evidence");
        }
        completed_reply(facts)
    });
    harness.wait_for_failure_checkpoint();
    assert!(
        seen.load(Ordering::SeqCst),
        "the selected Code job receives the preserved context"
    );
    let old: String = harness
        .db()
        .query_row(
            "SELECT context_json FROM validation_failure_contexts WHERE checkpoint_id=?1",
            [&checkpoint],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        old, original,
        "the historical context is immutable across restart and revision"
    );
    let old: Value = serde_json::from_str(&old).unwrap();
    assert_eq!(
        old["tests"][0]["content"], AUTHORED_TEST,
        "superseding Test does not erase the original failure"
    );
    let state: String = harness
        .db()
        .query_row(
            "SELECT status FROM stage_executions WHERE id=?1",
            [old["stageExecutionId"].as_str().unwrap()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(state, "superseded");
}
