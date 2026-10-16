//! ADR-024 M1, M3 and M4: the brief of every artifact checkpoint of a real v4
//! pulsation is read from the exact waiting checkpoint, labelled from the
//! frozen batch, refused once the checkpoint is stale, and compared with the
//! previous candidate after a revision; a stage failure lists its choices.
mod composition_support;

use composition_support::*;
use hvc_core::orchestration::PublicationState;
use hvc_core::orchestration::SchemaV1;
use hvc_core::orchestration::composition::settings::CompositionSettings;
use hvc_core::orchestration::composition::{
    CheckKind, CheckStatus, Handle, Uuid, ValidationCommandProfileV1, Verdict, VerificationMode,
};
use hvc_core::orchestration::review_brief::{
    BriefChange, ChangeKind, ChangeSubject, FailureDecisionKind, JudgementKind, ReviewBrief,
    StageVisual, failure_choices,
};
use hvc_core::persistence_service::{
    ApplyHitlDecisionRequest, GetPulsationRuntimeSnapshotRequest, HitlDecision, PulsationStage,
};
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
    assert_eq!(
        brief.objective.as_deref(),
        Some("Reject expired tokens in the validator.")
    );
    let StageVisual::Specification(visual) = &brief.visual else {
        panic!("not a Specification brief");
    };
    assert_eq!(visual.facts.tasks, 1);
    assert_eq!(visual.facts.criteria, 1);
    assert_eq!(visual.facts.by_reading, 1);
    assert_eq!(visual.facts.changed_interfaces, 1);

    // Task and criterion text come from the frozen batch, not the model.
    let task = &visual.tasks[0];
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
    assert!(!visual.files.is_empty());
    assert_eq!(visual.units.len(), 1);
    assert_eq!(visual.units[0].criteria, ["ac-1"]);

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

    // The next checkpoint briefs its own stage (M4).
    harness.approve();
    harness.wait_for_checkpoint("Contract");
    let contract = harness.service.get_hitl_checkpoint(&harness.pulse).unwrap();
    let contract = harness
        .service
        .read_checkpoint_brief(&contract)
        .unwrap()
        .expect("the Contract checkpoint has its own brief");
    assert_eq!(contract.stage, PulsationStage::Contract);
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

    // Approving the revised candidate moves on as before; the first Contract
    // candidate has nothing to compare.
    harness.approve();
    harness.wait_for_checkpoint("Contract");
    let contract = harness.service.get_hitl_checkpoint(&harness.pulse).unwrap();
    let contract = harness
        .service
        .read_checkpoint_brief(&contract)
        .unwrap()
        .expect("the Contract checkpoint has its own brief");
    assert_eq!(contract.revision, None);
}

const CHECK_FIXTURE: &str = env!("CARGO_BIN_EXE_check_fixture");

/// A required compile check that always passes, so later briefs carry a host
/// receipt.
fn enable_passing_check(harness: &Harness) {
    harness
        .service
        .upsert_validation_profile(
            &harness.project,
            ValidationCommandProfileV1 {
                schema_version: SchemaV1,
                profile_id: Uuid::generate(),
                profile_version: 1,
                check_ref: Handle::new("fixture_compile").unwrap(),
                kind: CheckKind::Compile,
                required: true,
                executable: CHECK_FIXTURE.to_owned(),
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
    harness
        .service
        .update_composition_settings(
            &harness.project,
            CompositionSettings {
                enabled: true,
                validation_enabled: true,
                ..CompositionSettings::default()
            },
        )
        .unwrap();
}

/// ADR-024 M4: Contract, Test and Integrator checkpoints of a real v4
/// pulsation brief their own payloads, with the approved specification's
/// objective, the host's check receipts and the files' publication state.
/// A stage that runs again, after a revision requested here or at a later
/// stage, is compared with its previous candidate.
#[test]
fn later_checkpoints_brief_their_own_payloads_with_receipts_and_publications() {
    const OBJECTIVE: &str = "Reject expired tokens in the validator.";
    let harness = Harness::start(|facts, _, _| completed_reply(facts));
    enable_passing_check(&harness);
    harness.wait_for_checkpoint("SPECIFICATION");
    harness.approve();

    harness.wait_for_checkpoint("Contract");
    let contract = current_brief(&harness);
    assert_eq!(contract.stage, PulsationStage::Contract);
    assert_eq!(contract.next_stage, Some(PulsationStage::Code));
    assert_eq!(contract.objective.as_deref(), Some(OBJECTIVE));
    assert_eq!(
        contract.counts,
        [
            "1 interface group · 1 changed",
            "1 work unit · 1 bound to an interface"
        ]
    );
    assert_eq!(
        judgement(&contract),
        [(JudgementKind::InterfaceChange, "validator")]
    );
    let StageVisual::Contract(visual) = &contract.visual else {
        panic!("not a Contract brief");
    };
    assert_eq!(
        visual.interfaces[0].signature,
        "pub fn validate_token(expires_at: u64, now: u64) -> bool"
    );
    assert_eq!(visual.interfaces[0].units, ["implement_expiry"]);
    assert_eq!(visual.units[0].interfaces, ["validator"]);
    harness.approve();

    // Test: the declared coverage, and the receipt of the check the host ran.
    harness.wait_for_checkpoint("Test");
    let test = current_brief(&harness);
    assert_eq!(test.next_stage, Some(PulsationStage::Integrator));
    assert_eq!(
        test.counts,
        [
            "1 criterion · 1 authored",
            "1 host check · 1 passed",
            "1 test file · 1 new"
        ]
    );
    assert!(test.judgement.is_empty(), "{:?}", test.judgement);
    let StageVisual::Test(visual) = &test.visual else {
        panic!("not a Test brief");
    };
    assert_eq!(visual.coverage[0].criterion_id, "ac-1");
    assert_eq!(visual.coverage[0].tests, ["tests/auth_test.rs"]);
    assert_eq!(visual.checks[0].check_ref, "fixture_compile");
    assert_eq!(visual.checks[0].status, CheckStatus::Passed);
    assert!(visual.checks[0].receipt.is_some());
    assert_eq!(test.revision, None);

    // A revision at Test runs Code and Test again; the new Test brief
    // compares with the first and quotes the feedback.
    request_revision(&harness, "Add a boundary test for expires_at == now.");
    let second = wait_for_next(&harness, "Test");
    let diff = current_brief(&harness)
        .revision
        .expect("Test ran again after its revision");
    assert_eq!(
        diff.feedback.as_deref(),
        Some("Add a boundary test for expires_at == now.")
    );
    assert_eq!(
        diff.changes,
        Some(Vec::new()),
        "the scripted author answered the same"
    );
    harness.approve();

    // Integrator: verdicts, no check for this stage, files already applied.
    harness.wait_for_checkpoint("Integrator");
    let integrator = current_brief(&harness);
    assert_eq!(integrator.next_stage, None);
    assert_eq!(integrator.counts[0], "1 criterion · 1 supported");
    assert_eq!(integrator.counts[1], "No host check for this stage");
    assert!(integrator.judgement.is_empty());
    let StageVisual::Integrator(visual) = &integrator.visual else {
        panic!("not an Integrator brief");
    };
    assert_eq!(visual.verdicts[0].verdict, Verdict::Supported);
    let files: Vec<(&str, PulsationStage, PublicationState)> = visual
        .publications
        .iter()
        .map(|file| (file.path.as_str(), file.stage, file.state))
        .collect();
    assert!(
        files.contains(&(
            "src/auth.rs",
            PulsationStage::Code,
            PublicationState::Applied
        )),
        "{files:?}"
    );
    assert!(
        files.contains(&(
            "tests/auth_test.rs",
            PulsationStage::Test,
            PublicationState::Applied
        )),
        "{files:?}"
    );

    // A revision requested at the Integrator targets Code. Test runs again
    // although its last review was an approval, and is compared with it.
    request_revision(&harness, "Name the boundary case in the report.");
    wait_for_next(&harness, &second);
    let diff = current_brief(&harness)
        .revision
        .expect("Test ran again after a later revision");
    assert_eq!(
        diff.feedback.as_deref(),
        Some("Name the boundary case in the report.")
    );
    assert_eq!(diff.changes, Some(Vec::new()));
}

fn current_brief(harness: &Harness) -> ReviewBrief {
    let checkpoint = harness.service.get_hitl_checkpoint(&harness.pulse).unwrap();
    harness
        .service
        .read_checkpoint_brief(&checkpoint)
        .unwrap()
        .expect("a v4 artifact checkpoint has a brief")
}

fn judgement(brief: &ReviewBrief) -> Vec<(JudgementKind, &str)> {
    brief
        .judgement
        .iter()
        .map(|item| (item.kind, item.label.as_str()))
        .collect()
}

fn request_revision(harness: &Harness, feedback: &str) {
    let checkpoint = harness.service.get_hitl_checkpoint(&harness.pulse).unwrap();
    harness
        .service
        .apply_hitl_decision(ApplyHitlDecisionRequest {
            pulsation_id: harness.pulse.clone(),
            checkpoint_id: checkpoint.checkpoint_id,
            expected_state_version: checkpoint.state_version,
            feedback: Some(feedback.into()),
            decision: HitlDecision::RequestRevision,
        })
        .unwrap();
}

/// Waits for a waiting Test checkpoint other than `not` (a checkpoint id or
/// a stage name for "any") and returns its id.
fn wait_for_next(harness: &Harness, not: &str) -> String {
    let mut found = None;
    wait_until(&harness.service, &harness.pulse, |db| {
        found = db
            .query_row(
                "SELECT id FROM checkpoints WHERE pulsation_id=?1 AND stage='Test' AND status='waiting' AND kind='artifact_review' AND id<>?2",
                [&harness.pulse, not],
                |r| r.get::<_, String>(0),
            )
            .ok();
        found.is_some()
    });
    found.unwrap()
}

/// ADR-024 M4: a real stage-failure checkpoint offers exactly the decisions
/// the host permits, each saying what it will do.
#[test]
fn a_contract_conflict_failure_lists_every_permitted_choice() {
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
    let snapshot = harness
        .service
        .get_pulsation_runtime_snapshot(GetPulsationRuntimeSnapshotRequest {
            project_id: harness.project.clone(),
        })
        .unwrap()
        .snapshot;
    let failure = snapshot
        .failure_checkpoints
        .iter()
        .find(|failure| failure.checkpoint_id == checkpoint)
        .expect("the snapshot lists the waiting failure");
    let choices: Vec<(FailureDecisionKind, Option<PulsationStage>)> = failure_choices(failure)
        .iter()
        .map(|choice| (choice.decision, choice.target))
        .collect();
    assert_eq!(
        choices,
        [
            (FailureDecisionKind::Revision, Some(PulsationStage::Code)),
            (
                FailureDecisionKind::Revision,
                Some(PulsationStage::Specification)
            ),
            (
                FailureDecisionKind::Revision,
                Some(PulsationStage::Contract)
            ),
            (FailureDecisionKind::Abort, None),
        ],
        "a diagnosis is never retried; the approved earlier stages are targets"
    );
}
