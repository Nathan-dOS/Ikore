//! Pipeline v4 recovery paths (ADR-022): the bounded correction, the
//! transport retry inside the shared allowance, and parking with the
//! decision set the host permits. Every path is observed through the same
//! supervisor and SQLite state the product uses.
mod composition_support;

use composition_support::*;
use hvc_core::persistence_service::{
    PersistenceServiceError, PulsationStage, ResolveStageFailureRequest, StageFailureDecision,
};
use hvc_test_support::ScriptedResponse;
use serde_json::json;

#[test]
fn a_transport_fault_is_retried_once_from_the_extra_pool_and_the_stage_completes() {
    // The first Code call fails at the provider; the retry holds an extra
    // permit of the same account and the stage still accepts once.
    let harness = Harness::start(|facts, ordinal, _| {
        if facts.executor == "code_implementer" && ordinal == 4 {
            ScriptedResponse::json(500, r#"{"error":{"message":"fixture outage"}}"#)
        } else {
            completed_reply(facts)
        }
    });
    for stage in ["SPECIFICATION", "Contract", "Test"] {
        harness.wait_for_checkpoint(stage);
        harness.approve();
    }
    assert_eq!(
        harness.calls(),
        [
            "intake_classifier",
            "specification_author",
            "contract_author",
            "code_implementer",
            "code_implementer",
            "test_author",
        ]
    );
    let db = harness.db();
    let pools: Vec<(String, String, i64)> = db
        .prepare("SELECT r.pool,r.state,u.generation FROM stage_invocation_reservations r JOIN stage_work_units u ON u.id=r.unit_id JOIN stage_executions e ON e.id=r.execution_id WHERE e.pulsation_id=?1 AND e.stage='Code' ORDER BY r.created_at")
        .unwrap()
        .query_map([&harness.pulse], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        pools,
        vec![
            ("initial".into(), "settled".into(), 2),
            ("extra".into(), "settled".into(), 2)
        ],
        "the failed call consumed its initial permit; the retry took the extra one"
    );
    let (rejected, succeeded): (i64, i64) = db
        .query_row(
            "SELECT SUM(a.state='rejected'),SUM(a.state='succeeded') FROM stage_work_unit_attempts a JOIN stage_executions e ON e.id=a.execution_id WHERE e.pulsation_id=?1 AND e.stage='Code'",
            [&harness.pulse],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((rejected, succeeded), (1, 1));
    let accepted: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM stage_acceptances a JOIN stage_executions e ON e.id=a.stage_execution_id WHERE e.pulsation_id=?1 AND e.stage='Code'",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(accepted, 1, "one acceptance for the stage (SC-018)");
}

#[test]
fn a_malformed_reply_gets_one_correction_with_the_diagnostic_and_never_the_raw_output() {
    let harness = Harness::start(|facts, ordinal, request| {
        if facts.executor == "intake_classifier" && ordinal == 1 {
            // Cites a criterion handle as a task: rejected by the host, not by
            // the transport, so the raw bytes are kept for the developer.
            provider_reply(json!({
                "schemaVersion": 1, "outcome": "completed", "executorKind": "intake_classifier",
                "payload": {"tasks": [{"taskRef": "task_9", "classification": "bugfix", "risk": "low", "summary": "Wrong handle."}], "issues": [], "usedRefs": []}
            }))
        } else {
            if facts.executor == "intake_classifier" {
                let body: serde_json::Value = serde_json::from_str(&request.body).unwrap();
                let user = body["messages"][1]["content"].as_str().unwrap();
                assert!(
                    user.contains("## Correction"),
                    "the retry carries the correction section"
                );
                assert!(
                    user.contains("task_9"),
                    "the diagnostic names the offending handle"
                );
                assert!(
                    !user.contains("Wrong handle."),
                    "the raw rejected output is never re-sent"
                );
            }
            completed_reply(facts)
        }
    });
    harness.wait_for_checkpoint("SPECIFICATION");
    assert_eq!(
        harness.calls(),
        [
            "intake_classifier",
            "intake_classifier",
            "specification_author"
        ]
    );
    let db = harness.db();
    let (generation, state): (i64, String) = db
        .query_row(
            "SELECT u.generation,u.state FROM stage_work_units u JOIN stage_executions e ON e.id=u.execution_id WHERE e.pulsation_id=?1 AND e.stage='Intake'",
            [&harness.pulse],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((generation, state.as_str()), (2, "validated"));
    let rejected: Option<String> = db
        .query_row(
            "SELECT validation_error FROM agent_run_outputs WHERE pulsation_id=?1 AND validation_error IS NOT NULL",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        rejected.unwrap().contains("task_9"),
        "the rejected attempt keeps its diagnostic"
    );
}

#[test]
fn a_worker_diagnosed_contract_conflict_parks_with_gated_targets_and_supersedes_on_revision() {
    let harness = Harness::start(|facts, ordinal, _| {
        if facts.executor == "code_implementer" && ordinal == 4 {
            provider_reply(json!({
                "schemaVersion": 1, "outcome": "failed", "code": "contract_conflict",
                "message": "The approved signature cannot express the expiry input.", "relatedRefs": []
            }))
        } else {
            completed_reply(facts)
        }
    });
    for stage in ["SPECIFICATION", "Contract"] {
        harness.wait_for_checkpoint(stage);
        harness.approve();
    }
    let checkpoint = harness.wait_for_failure_checkpoint();
    // One call only: a diagnosis is never "repaired" by asking again.
    assert_eq!(harness.calls().len(), 4);
    let db = harness.db();
    struct Parked {
        class: String,
        allow_retry: bool,
        allow_revision: bool,
        primary: Option<String>,
        targets: Option<String>,
        execution: Option<String>,
        remaining: Option<String>,
    }
    let Parked { class, allow_retry, allow_revision, primary, targets, execution, remaining } = db
        .query_row(
            "SELECT failure_class,allow_retry,allow_revision,revision_stage,allowed_revision_targets_json,stage_execution_id,remaining_budget_json FROM stage_failure_requests WHERE checkpoint_id=?1",
            [&checkpoint],
            |r| {
                Ok(Parked {
                    class: r.get(0)?,
                    allow_retry: r.get(1)?,
                    allow_revision: r.get(2)?,
                    primary: r.get(3)?,
                    targets: r.get(4)?,
                    execution: r.get(5)?,
                    remaining: r.get(6)?,
                })
            },
        )
        .unwrap();
    assert_eq!(class, "contract_conflict");
    assert!(!allow_retry && allow_revision);
    assert_eq!(primary.as_deref(), Some("Code"));
    assert_eq!(
        targets.as_deref(),
        Some(r#"["Code","SPECIFICATION","Contract"]"#),
        "the approved earlier stages are exposed as targets only for a contract conflict"
    );
    let execution = execution.expect("checkpoint carries the execution");
    let remaining: serde_json::Value = serde_json::from_str(&remaining.unwrap()).unwrap();
    // The stage keeps both extra permits; the checkpoint reports what the
    // failed unit itself could still take (its per-unit allowance of one).
    assert_eq!(
        remaining["extraInvocations"],
        json!(1),
        "no extra permit was spent on a diagnosis"
    );
    let held_extra: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM stage_invocation_reservations WHERE execution_id=?1 AND pool='extra'",
            [&execution],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(held_extra, 0);
    let status: String = db
        .query_row(
            "SELECT status FROM stage_executions WHERE id=?1",
            [&execution],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(status, "parked");

    // A target outside the permitted set is refused by the host.
    let refused = harness.service.resolve_stage_failure_with_target(
        ResolveStageFailureRequest {
            checkpoint_id: checkpoint.clone(),
            expected_state_version: harness.state_version(),
            decision: StageFailureDecision::RequestRevision,
            feedback: Some("Widen the validator signature to take the clock.".into()),
        },
        Some(PulsationStage::Test),
    );
    assert!(
        matches!(refused, Err(PersistenceServiceError::Validation(_))),
        "{refused:?}"
    );

    // Revision to Specification supersedes the parked execution and re-runs
    // the pipeline from there; the earlier accepted Intake stays.
    harness
        .service
        .resolve_stage_failure_with_target(
            ResolveStageFailureRequest {
                checkpoint_id: checkpoint,
                expected_state_version: harness.state_version(),
                decision: StageFailureDecision::RequestRevision,
                feedback: Some("Widen the validator signature to take the clock.".into()),
            },
            Some(PulsationStage::Specification),
        )
        .unwrap();
    harness.wait_for_checkpoint("SPECIFICATION");
    assert_eq!(
        harness.calls(),
        [
            "intake_classifier",
            "specification_author",
            "contract_author",
            "code_implementer",
            "specification_author",
        ]
    );
    let statuses: Vec<(String, String)> = db
        .prepare(
            "SELECT stage,status FROM stage_executions WHERE pulsation_id=?1 ORDER BY created_at",
        )
        .unwrap()
        .query_map([&harness.pulse], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        statuses,
        vec![
            ("Intake".into(), "accepted".into()),
            ("SPECIFICATION".into(), "superseded".into()),
            ("Contract".into(), "superseded".into()),
            ("Code".into(), "superseded".into()),
            ("SPECIFICATION".into(), "accepted".into()),
        ]
    );
    let closed: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM stage_budget_accounts WHERE pulsation_id=?1 AND state='closed'",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(closed, 3, "human revision closes the superseded accounts");
}
