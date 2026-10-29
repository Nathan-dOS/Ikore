//! ADR-035 M1 over the real pipeline v4: approving the Specification issues
//! the mandate its brief previewed, every later stage publishes inside it,
//! every checkpoint decision leaves a receipt, and a transport changed after
//! the approval parks the next stage until it is restored.
#![allow(
    clippy::unwrap_used,
    reason = "Test-only integration helpers fail immediately when fixture setup is invalid"
)]
mod composition_support;

use composition_support::*;
use hvc_core::orchestration::mandate::{AcknowledgementKind, Enforcement, MandateCause};
use hvc_core::persistence_service::{
    ApplyHitlDecisionRequest, HitlDecision, ResolveStageFailureRequest, RiskAcknowledgement,
    StageFailureDecision,
};

fn wait_until_done(harness: &Harness) {
    wait_until(&harness.service, &harness.pulse, |db| {
        db.query_row(
            "SELECT status='done' FROM pulsations WHERE id=?1",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap()
    });
}

#[test]
fn a_composed_pulsation_runs_inside_the_mandate_its_specification_approval_issued() {
    let harness = Harness::start(|facts, _, _| completed_reply(facts));
    harness.wait_for_checkpoint("SPECIFICATION");
    let specification = harness.service.get_hitl_checkpoint(&harness.pulse).unwrap();
    let preview = harness
        .service
        .mandate_preview(&specification.checkpoint_id)
        .unwrap()
        .expect("a waiting Specification review has a preview");
    assert_eq!(preview.mandate.enforcement, Enforcement::EnforcedV4);
    for path in ["src/auth.rs", "tests/auth_test.rs"] {
        assert!(
            preview.mandate.grants_path(path),
            "`{path}` is granted: {:?}",
            preview.mandate.scope.grants
        );
    }
    assert!(
        preview
            .sensitive_paths
            .iter()
            .any(|path| path == "src/auth.rs"),
        "the fixture's source is sensitive: {:?}",
        preview.sensitive_paths
    );
    harness.approve();

    let revisions = harness.service.pulsation_mandates(&harness.pulse).unwrap();
    assert_eq!(revisions.len(), 1);
    assert_eq!(revisions[0].cause, MandateCause::Issued);
    assert_eq!(
        revisions[0].mandate, preview.mandate,
        "the approval issued what the brief previewed"
    );
    for stage in ["Contract", "Test", "Integrator"] {
        harness.wait_for_checkpoint(stage);
        harness.approve();
    }
    wait_until_done(&harness);

    let db = harness.db();
    let applied: Vec<String> = db
        .prepare(
            "SELECT relative_path FROM artifact_publications WHERE pulsation_id=?1 AND state='applied' AND job_class='main' ORDER BY relative_path",
        )
        .unwrap()
        .query_map([&harness.pulse], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(applied.len() >= 3, "{applied:?}");
    for path in &applied {
        assert!(
            revisions[0].mandate.grants_path(path),
            "`{path}` was published outside the mandate"
        );
    }
    assert!(
        harness
            .service
            .mandate_enforcement_events(&harness.pulse)
            .unwrap()
            .is_empty(),
        "nothing was refused"
    );

    let checkpoints: Vec<String> = db
        .prepare(
            "SELECT id FROM checkpoints WHERE pulsation_id=?1 AND purpose='artifact_review' ORDER BY created_at",
        )
        .unwrap()
        .query_map([&harness.pulse], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(checkpoints.len(), 4);
    let receipts = checkpoints
        .iter()
        .map(|id| harness.service.checkpoint_receipt(id).unwrap().unwrap())
        .collect::<Vec<_>>();
    for receipt in &receipts {
        assert_eq!(receipt.purpose, "artifact_review");
        assert_eq!(receipt.decision, "approved");
        assert_eq!(
            receipt.mandate_revision,
            Some(1),
            "{}: the mandate was in force",
            receipt.stage
        );
        assert!(
            !receipt.reviewed_artifacts.is_empty(),
            "{}: the decision covered its candidates",
            receipt.stage
        );
    }
    assert!(
        receipts.iter().any(|receipt| receipt
            .acknowledgements
            .iter()
            .any(|ack| ack.kind == AcknowledgementKind::Risk)),
        "the high-risk Integrator approval kept its acknowledgement digest"
    );
}

#[test]
fn an_approval_whose_mandate_cannot_be_derived_is_refused_and_changes_nothing() {
    let harness = Harness::start(|facts, _, _| completed_reply(facts));
    harness.wait_for_checkpoint("SPECIFICATION");
    let checkpoint = harness.service.get_hitl_checkpoint(&harness.pulse).unwrap();
    harness
        .db()
        .execute(
            "UPDATE artifacts SET content=NULL WHERE pulsation_id=?1 AND producer_stage='SPECIFICATION' AND acceptance='candidate' AND kind='stage_payload_v1'",
            [&harness.pulse],
        )
        .unwrap();
    let error = harness
        .service
        .apply_hitl_decision_with_acknowledgements(
            ApplyHitlDecisionRequest {
                pulsation_id: harness.pulse.clone(),
                checkpoint_id: checkpoint.checkpoint_id.clone(),
                expected_state_version: checkpoint.state_version,
                feedback: None,
                decision: HitlDecision::Approve,
            },
            None,
            Some(RiskAcknowledgement {
                reason: "Fixture approval.".into(),
            }),
        )
        .unwrap_err();
    assert!(
        error.to_string().contains("mandate_derivation_failed"),
        "{error}"
    );
    assert!(
        harness
            .service
            .pulsation_mandates(&harness.pulse)
            .unwrap()
            .is_empty()
    );
    assert!(
        harness
            .service
            .checkpoint_receipt(&checkpoint.checkpoint_id)
            .unwrap()
            .is_none(),
        "a refused approval leaves no receipt"
    );
    let still = harness.service.get_hitl_checkpoint(&harness.pulse).unwrap();
    assert_eq!(still.checkpoint_id, checkpoint.checkpoint_id);
    assert_eq!(
        still.state_version, checkpoint.state_version,
        "nothing was half approved"
    );
}

#[test]
fn the_budget_frozen_at_the_approval_ignores_a_later_settings_change() {
    let harness = Harness::start(|facts, _, _| completed_reply(facts));
    harness.wait_for_checkpoint("SPECIFICATION");
    harness.approve();
    let frozen = harness.service.pulsation_mandates(&harness.pulse).unwrap()[0]
        .mandate
        .budget
        .output_limit_for("Code")
        .unwrap();
    // The live cap now exceeds every floor; only the mandate keeps Code at
    // what the developer approved.
    harness
        .db()
        .execute(
            "INSERT INTO runtime_settings(key,value,updated_at) VALUES('runtime.max_output_tokens','40000','now') ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [],
        )
        .unwrap();
    assert!(frozen < 40_000, "{frozen}");
    for stage in ["Contract", "Test", "Integrator"] {
        harness.wait_for_checkpoint(stage);
        harness.approve();
    }
    wait_until_done(&harness);
    let code: u32 = harness
        .db()
        .query_row(
            "SELECT initial_output_token_limit FROM stage_budget_accounts WHERE pulsation_id=?1 AND stage='Code'",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        code, frozen,
        "Code used the frozen limit, not the live 40000"
    );
}

#[test]
fn a_transport_changed_after_the_approval_parks_the_stage_until_restored() {
    let harness = Harness::start(|facts, _, _| completed_reply(facts));
    harness.wait_for_checkpoint("SPECIFICATION");
    harness.approve();
    harness.wait_for_checkpoint("Contract");

    let db = harness.db();
    let original: (String, Option<String>, Option<String>) = db
        .query_row(
            "SELECT execution_mode,backend,model FROM cli_agent_configs WHERE project_id=?1 AND agent_role='backend_agent'",
            [&harness.project],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    db.execute(
        "UPDATE cli_agent_configs SET execution_mode='cli_process',backend='codex',model='gpt-5.6-luna' WHERE project_id=?1 AND agent_role='backend_agent'",
        [&harness.project],
    )
    .unwrap();
    harness.approve();

    let checkpoint = harness.wait_for_failure_checkpoint();
    let (class, code, allow_retry, diagnostic): (String, String, bool, String) = db
        .query_row(
            "SELECT failure_class,error_code,allow_retry,diagnostic FROM stage_failure_requests WHERE checkpoint_id=?1",
            [&checkpoint],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(class, "stale_input");
    assert_eq!(code, "mandate_capability_mismatch");
    assert!(
        allow_retry,
        "restoring the transport and retrying is offered"
    );
    assert!(
        diagnostic.contains("backend_agent") && diagnostic.contains("cli/codex"),
        "{diagnostic}"
    );
    let events = harness
        .service
        .mandate_enforcement_events(&harness.pulse)
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].rule, "capability_mismatch");
    assert!(
        harness
            .calls()
            .iter()
            .all(|call| call != "code_implementer"),
        "nothing was dispatched: {:?}",
        harness.calls()
    );

    db.execute(
        "UPDATE cli_agent_configs SET execution_mode=?2,backend=?3,model=?4 WHERE project_id=?1 AND agent_role='backend_agent'",
        rusqlite::params![harness.project, original.0, original.1, original.2],
    )
    .unwrap();
    harness
        .service
        .resolve_stage_failure(ResolveStageFailureRequest {
            checkpoint_id: checkpoint.clone(),
            expected_state_version: harness.state_version(),
            decision: StageFailureDecision::RetryStage,
            feedback: None,
        })
        .unwrap();
    let receipt = harness
        .service
        .checkpoint_receipt(&checkpoint)
        .unwrap()
        .unwrap();
    assert_eq!(receipt.purpose, "stage_failure");
    assert_eq!(receipt.decision, "retry_stage");
    for stage in ["Test", "Integrator"] {
        harness.wait_for_checkpoint(stage);
        harness.approve();
    }
    wait_until_done(&harness);
}
