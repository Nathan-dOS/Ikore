#![allow(
    clippy::unwrap_used,
    reason = "Test-only integration helpers fail immediately when fixture setup is invalid"
)]
//! ADR-022 clarification loop preventive HITL gate (P5/A5.2): a composed
//! batch that skipped the Forge dispatch gate (P1/A1.3) - greenfield
//! exemption, no confirmed path - parks at Intake instead of letting
//! Specification run, once Intake accepts a batch containing a `Foundation`
//! task. Resolving the checkpoint with a manual path hint queues
//! Specification and records `pathConfirmationSource: manualHitl` on the
//! otherwise-frozen manifest.
mod composition_support;

use composition_support::*;
use hvc_core::feature_forge::{AcceptanceCriterion, TaskDraft, TaskKind};
use hvc_core::orchestration::{ManualPathHint, ResolvePathConfirmationRequest};
use hvc_test_support::IsolatedTestEnvironment;

fn foundation_task_with_no_path() -> TaskDraft {
    TaskDraft {
        title: "Sketch the initial module layout".into(),
        kind: TaskKind::Foundation,
        description: "Lay out the crate's first modules and their responsibilities.".into(),
        criteria: vec![AcceptanceCriterion {
            id: "ac-1".into(),
            description: "The module layout is documented.".into(),
        }],
        dependencies: vec![],
        feature_id: None,
    }
}

fn manifest_json(harness: &Harness) -> serde_json::Value {
    let raw: String = harness
        .db()
        .query_row(
            "SELECT manifest_json FROM pulsation_batches WHERE pulsation_id=?1",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap();
    serde_json::from_str(&raw).unwrap()
}

/// Proof that the pulsation advanced normally past Intake -- Specification's
/// job exists -- without requiring it to succeed: this fixture's scripted
/// replies have no real source to cite once a task's text names no existing
/// file, an orthogonal, pre-existing gap unrelated to this gate.
fn wait_for_specification_job(harness: &Harness) {
    wait_until(&harness.service, &harness.pulse, |db| {
        db.query_row(
            "SELECT EXISTS(SELECT 1 FROM jobs WHERE pulsation_id=?1 AND kind='run_stage::SPECIFICATION')",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap()
    });
}

fn wait_for_path_confirmation_checkpoint(harness: &Harness) -> String {
    let mut id = None;
    wait_until(&harness.service, &harness.pulse, |db| {
        id = db
            .query_row(
                "SELECT c.id FROM checkpoints c JOIN pulsations p ON p.id=c.pulsation_id WHERE p.id=?1 AND p.status='checkpoint' AND c.status='waiting' AND c.kind='path_confirmation'",
                [&harness.pulse],
                |r| r.get::<_, String>(0),
            )
            .ok();
        id.is_some()
    });
    id.unwrap()
}

#[test]
fn a_foundation_task_with_no_confirmed_path_parks_before_specification_and_resolves_with_a_hint() {
    // An empty workspace admits the dispatch under the greenfield exception
    // (P1/A1.3) with `pathConfirmationSource: none` -- only this preventive
    // gate (P5/A5.2) stands between this batch and Specification's budget.
    let env = IsolatedTestEnvironment::new();
    let harness = Harness::start_in(env, vec![foundation_task_with_no_path()], |facts, _, _| {
        completed_reply(facts)
    });

    assert_eq!(manifest_json(&harness)["pathConfirmationSource"], "none");

    let checkpoint_id = wait_for_path_confirmation_checkpoint(&harness);
    let specification_queued: bool = harness
        .db()
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM jobs WHERE pulsation_id=?1 AND kind='run_stage::SPECIFICATION')",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        !specification_queued,
        "Specification must not be queued while parked on the preventive checkpoint"
    );
    let (stage, status): (String, String) = harness
        .db()
        .query_row(
            "SELECT stage,status FROM pulsations WHERE id=?1",
            [&harness.pulse],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(stage, "Intake");
    assert_eq!(status, "checkpoint");

    harness
        .service
        .resolve_path_confirmation_checkpoint(ResolvePathConfirmationRequest {
            pulsation_id: harness.pulse.clone(),
            checkpoint_id: checkpoint_id.clone(),
            expected_state_version: harness.state_version(),
            manual_path_hints: vec![ManualPathHint {
                task_id: harness.task_id().to_string(),
                path: "src/layout.rs".into(),
            }],
        })
        .unwrap();
    let receipt = harness
        .service
        .checkpoint_receipt(&checkpoint_id)
        .unwrap()
        .expect("the confirmation has a receipt");
    assert_eq!(receipt.purpose, "path_confirmation");
    assert_eq!(receipt.decision, "confirmed");
    assert_eq!(receipt.confirmed_paths, vec!["src/layout.rs".to_string()]);

    // Resolving the checkpoint amends the frozen manifest's provenance and
    // queues Specification -- verified directly rather than by waiting for
    // Specification's own checkpoint, since nothing in this task's text ever
    // names "src/layout.rs" (the hint only satisfies the gate, per A1.3; it
    // is not wired into the stage's own reference table until M3), so a
    // scripted Specification reply for this fixture has no real source to
    // cite regardless -- an orthogonal, pre-existing fixture limitation,
    // not something this milestone's gate is responsible for.
    assert_eq!(
        manifest_json(&harness)["pathConfirmationSource"],
        "manual_hitl"
    );
    wait_for_specification_job(&harness);
}

#[test]
fn a_batch_with_a_confirmed_path_never_parks_on_the_preventive_checkpoint() {
    // Same greenfield workspace and the same `Foundation` task, but this time
    // the description names a path -- the Forge gate (P1/A1.3) already
    // confirms `pathConfirmationSource: forgeGate` at dispatch, so the
    // preventive gate (P5/A5.2) has nothing to add and must not fire.
    let mut task = foundation_task_with_no_path();
    task.description = "Lay out the crate's first modules in src/layout.rs.".into();
    let env = IsolatedTestEnvironment::new();
    let harness = Harness::start_in(env, vec![task], |facts, _, _| completed_reply(facts));

    assert_eq!(
        manifest_json(&harness)["pathConfirmationSource"],
        "forge_gate"
    );
    wait_for_specification_job(&harness);
    let parked_on_path_confirmation: bool = harness
        .db()
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM checkpoints WHERE pulsation_id=?1 AND kind='path_confirmation')",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!parked_on_path_confirmation);
}

#[test]
fn a_bugfix_task_with_no_confirmed_path_never_parks_on_the_preventive_checkpoint() {
    // The preventive gate only fires for `Foundation` (host-known) or
    // Intake-classified `feature` tasks; an ordinary bugfix in the same
    // greenfield workspace advances straight through, unprotected and
    // unblocked, exactly as before this milestone.
    let task = TaskDraft {
        title: "Fix the off-by-one".into(),
        kind: TaskKind::Bug,
        description: "The counter overshoots by one somewhere in the request handler.".into(),
        criteria: vec![AcceptanceCriterion {
            id: "ac-1".into(),
            description: "The counter no longer overshoots.".into(),
        }],
        dependencies: vec![],
        feature_id: None,
    };
    let env = IsolatedTestEnvironment::new();
    let harness = Harness::start_in(env, vec![task], |facts, _, _| completed_reply(facts));

    assert_eq!(manifest_json(&harness)["pathConfirmationSource"], "none");
    wait_for_specification_job(&harness);
    let parked_on_path_confirmation: bool = harness
        .db()
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM checkpoints WHERE pulsation_id=?1 AND kind='path_confirmation')",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!parked_on_path_confirmation);
}
