//! ADR-024 M1: the Specification brief of a real v4 pulsation is read from the
//! exact waiting checkpoint, labelled from the frozen batch, and refused once
//! the checkpoint is stale.
mod composition_support;

use composition_support::*;
use hvc_core::orchestration::composition::VerificationMode;
use hvc_core::orchestration::review_brief::JudgementKind;

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
