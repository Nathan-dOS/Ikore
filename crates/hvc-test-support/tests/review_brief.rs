//! ADR-024 M1 and M3: the Specification brief of a real v4 pulsation is read
//! from the exact waiting checkpoint, labelled from the frozen batch, refused
//! once the checkpoint is stale, and compared with the previous candidate
//! after Request revision.
mod composition_support;

use composition_support::*;
use hvc_core::orchestration::composition::VerificationMode;
use hvc_core::orchestration::review_brief::{
    BriefChange, ChangeKind, ChangeSubject, JudgementKind,
};
use hvc_core::persistence_service::{ApplyHitlDecisionRequest, HitlDecision};
use serde_json::{Value, json};

#[test]
fn specification_checkpoint_brief_comes_from_the_typed_payload() {
    let harness = Harness::start(|facts, _, _| completed_reply(facts));
    harness.wait_for_checkpoint("SPECIFICATION");
    let checkpoint = harness.service.get_hitl_checkpoint(&harness.pulse).unwrap();

    let brief = harness
        .service
        .read_checkpoint_brief(&checkpoint)
        .unwrap()
        .expect("a v4 Specification checkpoint has a brief");
    assert_eq!(brief.objective, "Reject expired tokens in the validator.");
    assert_eq!(brief.facts.tasks, 1);
    assert_eq!(brief.facts.criteria, 1);
    assert_eq!(brief.facts.by_reading, 1);
    assert_eq!(brief.facts.changed_interfaces, 1);

    // Task and criterion text come from the frozen batch, not the model.
    let task = &brief.tasks[0];
    assert_eq!(task.title, "Reject expired tokens");
    assert_eq!(task.criteria[0].criterion_id, "ac-1");
    assert_eq!(
        task.criteria[0].description,
        "An expired token is rejected by validate_token"
    );
    assert_eq!(
        task.criteria[0].verification,
        VerificationMode::DocumentCheck
    );

    let judgement: Vec<(JudgementKind, &str)> = brief
        .judgement
        .iter()
        .map(|item| (item.kind, item.label.as_str()))
        .collect();
    assert_eq!(judgement[0], (JudgementKind::Decision, "D1"));
    assert_eq!(judgement[1], (JudgementKind::ReadingOnlyProof, "ac-1"));
    assert_eq!(judgement[2].0, JudgementKind::InterfaceChange);
    assert_eq!(judgement[3], (JudgementKind::Exclusions, "1"));
    assert_eq!(
        brief.judgement[0].text,
        "Keep the public signature of validate_token."
    );
    assert!(!brief.files.is_empty());
    assert_eq!(brief.units.len(), 1);
    assert_eq!(brief.units[0].criteria, ["ac-1"]);

    // The documents of the same checkpoint are unaffected by the brief read.
    assert!(
        !harness
            .service
            .read_hitl_review(&checkpoint)
            .unwrap()
            .is_empty()
    );

    // A stale version is refused exactly like the document read.
    let mut stale = checkpoint.clone();
    stale.state_version += 1;
    assert!(harness.service.read_checkpoint_brief(&stale).is_err());
    assert!(harness.service.read_hitl_review(&stale).is_err());

    // M1 only briefs Specification; the Contract checkpoint reads as `None`.
    harness.approve();
    harness.wait_for_checkpoint("Contract");
    let contract = harness.service.get_hitl_checkpoint(&harness.pulse).unwrap();
    assert!(
        harness
            .service
            .read_checkpoint_brief(&contract)
            .unwrap()
            .is_none()
    );
}

/// ADR-024 M3: Request revision only marks the earlier candidate
/// `superseded`. Its typed payload stays readable behind the resolved
/// checkpoint, so the next Specification brief compares the two and quotes
/// the feedback the run received. A previous payload that cannot be read
/// says so instead of inferring anything.
#[test]
fn a_revised_specification_brief_shows_what_changed_and_the_feedback() {
    const FEEDBACK: &str = "Also reject a token that expires exactly now.";
    let harness = Harness::start(|facts, _, _| {
        let mut reply = unit_response(facts);
        if facts.executor == "specification_author" && facts.user_prompt.contains(FEEDBACK) {
            reply["payload"]["decisions"] =
                json!(["Treat expires_at == now as expired; keep the signature."]);
        }
        provider_reply(reply)
    });
    harness.wait_for_checkpoint("SPECIFICATION");
    let first = harness.service.get_hitl_checkpoint(&harness.pulse).unwrap();
    let brief = harness
        .service
        .read_checkpoint_brief(&first)
        .unwrap()
        .unwrap();
    assert_eq!(
        brief.revision, None,
        "a first candidate has nothing to compare"
    );

    harness
        .service
        .apply_hitl_decision(ApplyHitlDecisionRequest {
            pulsation_id: harness.pulse.clone(),
            checkpoint_id: first.checkpoint_id.clone(),
            expected_state_version: first.state_version,
            feedback: Some(FEEDBACK.into()),
            decision: HitlDecision::RequestRevision,
        })
        .unwrap();
    wait_until(&harness.service, &harness.pulse, |db| {
        db.query_row(
            "SELECT EXISTS(SELECT 1 FROM checkpoints WHERE pulsation_id=?1 AND id<>?2 AND status='waiting' AND kind='artifact_review')",
            [&harness.pulse, &first.checkpoint_id],
            |r| r.get(0),
        )
        .unwrap()
    });
    let second = harness.service.get_hitl_checkpoint(&harness.pulse).unwrap();

    // Retention: the first candidate's payload is still there, superseded.
    let (status, acceptance, content): (String, String, Option<String>) = harness
        .db()
        .query_row(
            "SELECT c.status,a.acceptance,a.content FROM artifacts a
             JOIN checkpoints c ON c.pulsation_id=a.pulsation_id
               AND c.producer_job_id=a.producer_job_id AND c.generation=a.generation
               AND c.input_revision=a.input_revision AND c.stage=a.producer_stage
             WHERE c.id=?1 AND a.kind='stage_payload_v1'",
            [&first.checkpoint_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(status, "revision_requested");
    assert_eq!(acceptance, "superseded");
    let earlier: Value = serde_json::from_str(content.as_deref().unwrap()).unwrap();
    assert_eq!(
        earlier["units"][0]["response"]["payload"]["decisions"][0],
        "Keep the public signature of validate_token."
    );

    let revised = harness
        .service
        .read_checkpoint_brief(&second)
        .unwrap()
        .unwrap();
    let diff = revised
        .revision
        .expect("the stage ran again after revision");
    assert_eq!(diff.feedback.as_deref(), Some(FEEDBACK));
    let decision = |kind, before: Option<&str>, after: Option<&str>| BriefChange {
        kind,
        subject: ChangeSubject::Decision,
        label: "D1".into(),
        field: None,
        before: before.map(Into::into),
        after: after.map(Into::into),
    };
    assert_eq!(
        diff.changes.expect("the previous payload is readable"),
        [
            decision(
                ChangeKind::Removed,
                Some("Keep the public signature of validate_token."),
                None
            ),
            decision(
                ChangeKind::Added,
                None,
                Some("Treat expires_at == now as expired; keep the signature.")
            ),
        ]
    );

    // An unreadable previous payload is reported, never reconstructed.
    harness
        .db()
        .execute(
            "UPDATE artifacts SET content=NULL WHERE kind='stage_payload_v1' AND acceptance='superseded'",
            [],
        )
        .unwrap();
    let diff = harness
        .service
        .read_checkpoint_brief(&second)
        .unwrap()
        .unwrap()
        .revision
        .unwrap();
    assert_eq!(diff.changes, None);
    assert_eq!(diff.feedback.as_deref(), Some(FEEDBACK));

    // Approving the revised candidate moves on as before; Contract has no brief.
    harness.approve();
    harness.wait_for_checkpoint("Contract");
    let contract = harness.service.get_hitl_checkpoint(&harness.pulse).unwrap();
    assert!(
        harness
            .service
            .read_checkpoint_brief(&contract)
            .unwrap()
            .is_none()
    );
}
