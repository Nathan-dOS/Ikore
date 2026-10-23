#![allow(
    clippy::unwrap_used,
    reason = "Test-only integration helpers fail immediately when fixture setup is invalid"
)]
//! Pipeline v4 recovery paths (ADR-022): the bounded correction, the
//! transport retry inside the shared allowance, and parking with the
//! decision set the host permits. Every path is observed through the same
//! supervisor and SQLite state the product uses.
mod composition_support;

use composition_support::*;
use hvc_core::orchestration::{ClarificationAnswerV1, SubmitSpecificationAnswersRequest};
use hvc_core::persistence_service::{
    PersistenceServiceError, PulsationStage, ResolveStageFailureRequest, StageFailureDecision,
};
use hvc_test_support::ScriptedResponse;
use serde_json::json;

/// Answers the open clarification checkpoint of the harness pulsation.
fn answer_clarification(harness: &Harness, text: &str) {
    wait_until(&harness.service, &harness.pulse, |db| {
        db.query_row(
            "SELECT EXISTS(SELECT 1 FROM checkpoints WHERE pulsation_id=?1 AND status='waiting' AND purpose='clarification')",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap()
    });
    let pending = harness
        .service
        .list_open_specification_checkpoints(&harness.project)
        .unwrap()
        .into_iter()
        .next()
        .expect("an open clarification");
    harness
        .service
        .submit_specification_answers(SubmitSpecificationAnswersRequest {
            pulsation_id: harness.pulse.clone(),
            question_set_id: pending.id,
            checkpoint_id: pending.checkpoint_id,
            expected_input_revision: pending.fence.input_revision.unwrap(),
            expected_state_version: pending.fence.state_version,
            answers: pending
                .questions
                .iter()
                .map(|question| ClarificationAnswerV1 {
                    question_id: question.id.clone(),
                    text_answer: text.into(),
                    planned_paths: Vec::new(),
                    modified_paths: Vec::new(),
                })
                .collect(),
        })
        .unwrap();
}

#[test]
fn clarification_rounds_open_a_fresh_allowance_and_keep_the_specification_repair() {
    // ToDoListWebView, 2026-09-22: every answered round re-ran Specification
    // from the extra pool, so the second round ended in `budget_exhausted`
    // with no call at all and a malformed draft after one round had no
    // correction left. Each answer now opens a fresh account.
    let harness = Harness::start(
        |facts, ordinal, _| match (facts.executor.as_str(), ordinal) {
            ("specification_author", 2 | 3) => provider_reply(json!({
                "schemaVersion": 1, "outcome": "needs_input", "missingRefs": [],
                "questions": [{"key": "token_clock", "taskRefs": [facts.tasks[0]],
                    "question": "Which clock source should validate_token use?",
                    "rationale": "The expiry check needs a time source."}]
            })),
            ("specification_author", 4) => provider_reply(json!({
                "schemaVersion": 1, "outcome": "completed",
                "executorKind": "specification_author", "payload": {}
            })),
            _ => completed_reply(facts),
        },
    );
    answer_clarification(&harness, "Use the caller-supplied `now` argument.");
    answer_clarification(&harness, "Seconds since the Unix epoch.");
    harness.wait_for_checkpoint("SPECIFICATION");
    assert_eq!(
        harness.calls(),
        [
            "intake_classifier",
            "specification_author",
            "specification_author",
            "specification_author",
            "specification_author",
        ],
        "two answered rounds, then one malformed draft and its correction"
    );
    let db = harness.db();
    let failures: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM stage_failure_requests WHERE pulsation_id=?1",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(failures, 0, "no budget_exhausted checkpoint");
    let accounts: Vec<(String, Option<String>)> = db
        .prepare("SELECT state,predecessor_account_id FROM stage_budget_accounts WHERE pulsation_id=?1 AND stage='SPECIFICATION' ORDER BY created_at")
        .unwrap()
        .query_map([&harness.pulse], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(accounts.len(), 3, "one account per answered round");
    assert_eq!(accounts[0].0, "closed");
    assert_eq!(accounts[1].0, "closed");
    assert_eq!(accounts[2].0, "open");
    assert!(
        accounts[1].1.is_some() && accounts[2].1.is_some(),
        "every successor links its spent predecessor"
    );
    let pools: Vec<String> = db
        .prepare("SELECT r.pool FROM stage_invocation_reservations r JOIN stage_budget_accounts a ON a.id=r.budget_account_id WHERE a.pulsation_id=?1 AND a.stage='SPECIFICATION' ORDER BY r.created_at")
        .unwrap()
        .query_map([&harness.pulse], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        pools,
        ["initial", "initial", "initial", "extra"],
        "answers ride initial permits; only the correction uses the extra pool"
    );
}

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

/// A reply cut off mid-document: the host reads it as a truncated answer.
fn truncated_reply() -> ScriptedResponse {
    ScriptedResponse::json(
        200,
        json!({"model":"gpt-4.1-mini","choices":[{"message":{"content":"{\"schemaVersion\": 1, \"outcome\": \"completed\", \"payload\": {\"assessments\": ["},"finish_reason":"stop"}],"usage":{"prompt_tokens":120,"completion_tokens":60}}).to_string(),
    )
}

/// The parked Integrator failure: (`allow_retry`, primary target, targets).
fn integrator_offer(harness: &Harness, checkpoint: &str) -> (bool, Option<String>, String) {
    harness
        .db()
        .query_row(
            "SELECT allow_retry,revision_stage,allowed_revision_targets_json FROM stage_failure_requests WHERE checkpoint_id=?1 AND stage='Integrator'",
            [checkpoint],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
}

/// Code and Test each ran once and stayed accepted.
fn assert_code_and_test_untouched(harness: &Harness) {
    let statuses: Vec<(String, String)> = harness
        .db()
        .prepare("SELECT stage,status FROM stage_executions WHERE pulsation_id=?1 AND stage IN ('Code','Test') ORDER BY created_at")
        .unwrap()
        .query_map([&harness.pulse], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        statuses,
        vec![
            ("Code".into(), "accepted".into()),
            ("Test".into(), "accepted".into())
        ]
    );
}

#[test]
fn a_rejected_integrator_answer_is_retried_with_its_diagnostic_without_touching_code() {
    let harness = Harness::start(|facts, ordinal, request| {
        if facts.executor != "integration_reviewer" {
            return completed_reply(facts);
        }
        if ordinal == 6 {
            return truncated_reply();
        }
        let body: serde_json::Value = serde_json::from_str(&request.body).unwrap();
        let user = body["messages"][1]["content"].as_str().unwrap();
        assert!(user.contains("## Correction"), "the Retry says why");
        assert!(
            user.contains("ended before it was closed"),
            "the carried diagnostic reaches the model"
        );
        completed_reply(facts)
    });
    for stage in ["SPECIFICATION", "Contract", "Test"] {
        harness.wait_for_checkpoint(stage);
        harness.approve();
    }
    let checkpoint = harness.wait_for_failure_checkpoint();
    let (allow_retry, primary, targets) = integrator_offer(&harness, &checkpoint);
    assert!(allow_retry, "the unit still holds its extra permit");
    assert_eq!(primary.as_deref(), Some("Integrator"));
    assert_eq!(targets, r#"["Integrator","Code"]"#);
    harness
        .service
        .resolve_stage_failure_with_target(
            ResolveStageFailureRequest {
                checkpoint_id: checkpoint,
                expected_state_version: harness.state_version(),
                decision: StageFailureDecision::RetryStage,
                feedback: None,
            },
            None,
        )
        .unwrap();
    harness.wait_for_checkpoint("Integrator");
    assert_eq!(
        harness.calls(),
        [
            "intake_classifier",
            "specification_author",
            "contract_author",
            "code_implementer",
            "test_author",
            "integration_reviewer",
            "integration_reviewer",
        ]
    );
    assert_code_and_test_untouched(&harness);
}

#[test]
fn an_integrator_revision_reruns_only_the_integrator() {
    let harness = Harness::start(|facts, ordinal, _| {
        if facts.executor == "integration_reviewer" && ordinal == 6 {
            truncated_reply()
        } else {
            completed_reply(facts)
        }
    });
    for stage in ["SPECIFICATION", "Contract", "Test"] {
        harness.wait_for_checkpoint(stage);
        harness.approve();
    }
    let checkpoint = harness.wait_for_failure_checkpoint();
    harness
        .service
        .resolve_stage_failure_with_target(
            ResolveStageFailureRequest {
                checkpoint_id: checkpoint,
                expected_state_version: harness.state_version(),
                decision: StageFailureDecision::RequestRevision,
                feedback: Some("Keep each explanation to one sentence.".into()),
            },
            None,
        )
        .unwrap();
    harness.wait_for_checkpoint("Integrator");
    assert_eq!(
        harness.calls().last().map(String::as_str),
        Some("integration_reviewer")
    );
    assert_eq!(harness.calls().len(), 7, "Code and Test are not re-run");
    assert_code_and_test_untouched(&harness);
}

#[test]
fn a_check_citation_without_configured_checks_is_cleared_not_charged() {
    // "Teste Haiku Only 1" (ToDoListWebView, 2026-09-24): with no host check
    // configured, the Integrator cited the test file as a check. The host
    // classified it as its own contract error, parked without Retry and
    // forced a Code revision. The citation is now cleared by the host with
    // an audited note, and the stage completes with its single call.
    let harness = Harness::start(|facts, _, _| {
        if facts.executor == "integration_reviewer" {
            let mut reply = unit_response(facts);
            let source = reply["payload"]["usedRefs"][0].clone();
            for assessment in reply["payload"]["assessments"].as_array_mut().unwrap() {
                assessment["checkRefs"] = json!([source]);
            }
            provider_reply(reply)
        } else {
            completed_reply(facts)
        }
    });
    for stage in ["SPECIFICATION", "Contract", "Test"] {
        harness.wait_for_checkpoint(stage);
        harness.approve();
    }
    harness.wait_for_checkpoint("Integrator");
    assert_eq!(
        harness.calls(),
        [
            "intake_classifier",
            "specification_author",
            "contract_author",
            "code_implementer",
            "test_author",
            "integration_reviewer",
        ],
        "no correction call and no revision of Code"
    );
    let db = harness.db();
    let failures: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM stage_failure_requests WHERE pulsation_id=?1",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(failures, 0);
    let (state, diagnostic): (String, Option<String>) = db
        .query_row(
            "SELECT a.state,a.diagnostic FROM stage_work_unit_attempts a JOIN stage_executions e ON e.id=a.execution_id WHERE e.pulsation_id=?1 AND e.stage='Integrator'",
            [&harness.pulse],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(state, "succeeded");
    let diagnostic = diagnostic.expect("the accepted attempt records the host's normalization");
    assert!(
        diagnostic.contains("assessment.checkRefs")
            && diagnostic.contains("no host check is configured"),
        "{diagnostic}"
    );
}
