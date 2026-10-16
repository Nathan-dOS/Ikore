//! Recipe paths that spend no model call (ADR-022, M5; recipes §Contract,
//! §Test, §Integrator): a preserved contract and a not-applicable test stage
//! are synthesized by the host and reviewed like any other, the Integrator
//! gets deterministic pre-checks before its single review, and a typed
//! recovery command is applied idempotently with stale-version protection.
mod composition_support;

use composition_support::*;
use hvc_core::orchestration::SchemaV1;
use hvc_core::orchestration::composition::{Stage, StageRecoveryCommandV1, Uuid};
use hvc_core::persistence_service::PersistenceServiceError;
use hvc_test_support::ScriptedResponse;
use serde_json::json;

fn preserved(facts: &PromptFacts) -> ScriptedResponse {
    let options = ReplyOptions {
        interface_intent: "preserve",
        ..ReplyOptions::default()
    };
    provider_reply(unit_response_with(facts, &options))
}

#[test]
fn a_preserved_contract_is_rendered_by_the_host_without_a_call_and_still_reviewed() {
    let harness = Harness::start(|facts, _, _| {
        assert_ne!(
            facts.executor, "contract_author",
            "no contract call on the preserved path"
        );
        preserved(facts)
    });
    harness.wait_for_checkpoint("SPECIFICATION");
    harness.approve();
    harness.wait_for_checkpoint("Contract");
    assert_eq!(
        harness.calls(),
        ["intake_classifier", "specification_author"]
    );
    let db = harness.db();
    let document: String = db
        .query_row(
            "SELECT content FROM artifacts WHERE pulsation_id=?1 AND kind='contract' ORDER BY created_at DESC LIMIT 1",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        document
            .contains("Preserved: the approved specification declares no new or changed interface"),
        "{document}"
    );
    assert!(
        document.contains("| `implement_expiry` | code |"),
        "the planned units are still reviewed: {document}"
    );
    let (state, attempts, reservations): (String, i64, i64) = db
        .query_row(
            "SELECT u.state,(SELECT COUNT(*) FROM stage_work_unit_attempts a WHERE a.unit_id=u.id),(SELECT COUNT(*) FROM stage_invocation_reservations r WHERE r.unit_id=u.id) FROM stage_work_units u JOIN stage_executions e ON e.id=u.execution_id WHERE e.pulsation_id=?1 AND e.stage='Contract'",
            [&harness.pulse],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (state.as_str(), attempts, reservations),
        ("validated", 0, 0),
        "synthesized: no attempt, no permit"
    );
    // The pipeline continues normally after approval.
    harness.approve();
    harness.wait_for_checkpoint("Test");
    assert_eq!(harness.calls()[2..], ["code_implementer", "test_author"]);
}

#[test]
fn a_stage_whose_criteria_are_all_not_applicable_authors_no_test_and_the_report_says_so() {
    let harness = Harness::start(|facts, _, _| {
        assert_ne!(
            facts.executor, "test_author",
            "no test call when every criterion is not applicable"
        );
        if facts.executor == "specification_author" {
            let mut reply = unit_response(facts);
            let requirements = reply["payload"]["requirements"].as_array_mut().unwrap();
            for requirement in requirements {
                requirement["verificationMode"] = json!("not_applicable");
                requirement["notApplicableReason"] =
                    json!("Documentation-only change approved by the developer.");
            }
            return provider_reply(reply);
        }
        completed_reply(facts)
    });
    for stage in ["SPECIFICATION", "Contract"] {
        harness.wait_for_checkpoint(stage);
        harness.approve();
    }
    harness.wait_for_checkpoint("Test");
    assert_eq!(
        harness.calls(),
        [
            "intake_classifier",
            "specification_author",
            "contract_author",
            "code_implementer"
        ]
    );
    let db = harness.db();
    let report: String = db
        .query_row(
            "SELECT content FROM artifacts WHERE pulsation_id=?1 AND kind='validation_report' ORDER BY created_at DESC LIMIT 1",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap();
    assert!(report.contains("not applicable"), "{report}");
    assert!(
        report.contains("Documentation-only change approved by the developer."),
        "{report}"
    );
    assert!(
        !harness.workspace().join("tests").exists(),
        "nothing authored, nothing published"
    );
    let evidence: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM task_evidence WHERE pulsation_id=?1 AND stage='Test'",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(evidence, 1, "the task still gets its evidence row");
}

#[test]
fn the_integrator_receives_host_prechecks_and_a_recovery_command_is_idempotent_and_fenced() {
    let harness = Harness::start(|facts, _, request| {
        if facts.executor == "integration_reviewer" {
            assert!(
                request.body.contains("## Host pre-checks"),
                "pre-checks reach the reviewer"
            );
            assert!(
                request
                    .body
                    .contains("The Code check named fixture_compile has no receipt (not run)."),
                "{}",
                facts.user_prompt
            );
            // Pre-checks name earlier stages' checks without a citable handle
            // ("Teste Haiku Only 1").
            let prechecks = facts
                .user_prompt
                .split("## Host pre-checks")
                .nth(1)
                .and_then(|rest| rest.split("\n## ").next())
                .unwrap();
            assert!(!prechecks.contains("`fixture_compile`"), "{prechecks}");
            assert!(
                facts
                    .user_prompt
                    .contains("cite no check handle that is not listed under Host checks")
            );
            // Diagnoses a production defect so the stage parks and a typed
            // recovery command can be applied.
            return provider_reply(json!({
                "schemaVersion": 1, "outcome": "failed", "code": "implementation_mismatch",
                "message": "The final artifacts contradict the approved behavior.", "relatedRefs": []
            }));
        }
        completed_reply(facts)
    });
    // A configured but disabled check: the Code stage records `not_run`, which
    // the Integrator pre-check reports deterministically.
    harness
        .service
        .upsert_validation_profile(
            &harness.project,
            hvc_core::orchestration::composition::ValidationCommandProfileV1 {
                schema_version: SchemaV1,
                profile_id: Uuid::generate(),
                profile_version: 1,
                check_ref: hvc_core::orchestration::composition::Handle::new("fixture_compile")
                    .unwrap(),
                kind: hvc_core::orchestration::composition::CheckKind::Compile,
                required: true,
                executable: env!("CARGO_BIN_EXE_check_fixture").to_owned(),
                argv: vec!["pass".into()],
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
    for stage in ["SPECIFICATION", "Contract", "Test"] {
        harness.wait_for_checkpoint(stage);
        harness.approve();
    }
    let checkpoint = harness.wait_for_failure_checkpoint();
    let db = harness.db();
    let (execution, generation, version): (String, u32, u32) = db
        .query_row(
            "SELECT id,parent_generation,execution_version FROM stage_executions WHERE pulsation_id=?1 AND stage='Integrator' AND status='parked'",
            [&harness.pulse],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    let (class, targets): (String, Option<String>) = db
        .query_row(
            "SELECT failure_class,allowed_revision_targets_json FROM stage_failure_requests WHERE checkpoint_id=?1",
            [&checkpoint],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(class, "validation_failed");
    assert_eq!(
        targets.as_deref(),
        Some(r#"["Code"]"#),
        "a production defect found by the reviewer routes to Code"
    );
    drop(db);
    let state_version = harness.state_version() as u32;
    let stale = harness
        .service
        .apply_stage_recovery(StageRecoveryCommandV1::Revise {
            schema_version: SchemaV1,
            project_id: Uuid::new(harness.project.clone()).unwrap(),
            pulsation_id: Uuid::new(harness.pulse.clone()).unwrap(),
            stage_execution_id: Uuid::new(execution.clone()).unwrap(),
            request_id: Uuid::generate(),
            expected_parent_generation: generation,
            expected_parent_state_version: state_version,
            expected_execution_version: version + 1,
            target_stage: Stage::Code,
            reason: "Fix the validator before re-reviewing.".into(),
        });
    assert!(
        matches!(&stale, Err(PersistenceServiceError::Validation(message)) if message.contains("SC-021")),
        "{stale:?}"
    );
    let command = StageRecoveryCommandV1::Revise {
        schema_version: SchemaV1,
        project_id: Uuid::new(harness.project.clone()).unwrap(),
        pulsation_id: Uuid::new(harness.pulse.clone()).unwrap(),
        stage_execution_id: Uuid::new(execution.clone()).unwrap(),
        request_id: Uuid::generate(),
        expected_parent_generation: generation,
        expected_parent_state_version: state_version,
        expected_execution_version: version,
        target_stage: Stage::Code,
        reason: "Fix the validator before re-reviewing.".into(),
    };
    let first = harness
        .service
        .apply_stage_recovery(command.clone())
        .unwrap();
    assert_eq!(first.stage_execution_id.as_str(), execution);
    assert_eq!(
        first.status,
        hvc_core::orchestration::composition::ExecutionStatus::Superseded
    );
    // Replaying the same request returns the same result without a second decision.
    let again = harness.service.apply_stage_recovery(command).unwrap();
    assert_eq!(again, first);
    harness.wait_for_checkpoint("Test");
    let calls = harness.calls();
    assert_eq!(
        calls.last().map(String::as_str),
        Some("test_author"),
        "{calls:?}"
    );
    assert_eq!(
        calls.iter().filter(|c| *c == "code_implementer").count(),
        2,
        "Code ran again after the revision: {calls:?}"
    );
}
