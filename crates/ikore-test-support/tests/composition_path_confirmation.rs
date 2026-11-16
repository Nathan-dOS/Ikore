#![allow(
    clippy::unwrap_used,
    reason = "Test-only integration helpers fail immediately when fixture setup is invalid"
)]
//! Automatic host-owned destination planning reaches Specification with
//! frozen file references. Legacy batches lacking a destination plan retain
//! the preventive Intake checkpoint and preserve manual recovery paths.
mod composition_support;

use composition_support::*;
use ikore_core::feature_forge::{AcceptanceCriterion, TaskDraft, TaskKind};
use ikore_core::orchestration::{
    ContentSha256, ManualPathHint, ResolvePathConfirmationRequest,
    composition::settings::CompositionSettings,
};
use ikore_test_support::IsolatedTestEnvironment;
use serde_json::Value;

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

/// Wait for the actual frozen Specification plan, so a queued but unusable
/// stage cannot masquerade as successful destination handoff.
fn specification_plan(harness: &Harness) -> Value {
    wait_until(&harness.service, &harness.pulse, |db| {
        db.query_row(
            "SELECT EXISTS(SELECT 1 FROM stage_executions WHERE pulsation_id=?1 AND stage='SPECIFICATION')",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap()
    });
    let raw: String = harness.db().query_row(
        "SELECT plan_json FROM stage_executions WHERE pulsation_id=?1 AND stage='SPECIFICATION' ORDER BY created_at DESC LIMIT 1",
        [&harness.pulse],
        |row| row.get(0),
    ).unwrap();
    serde_json::from_str(&raw).unwrap()
}

fn assert_reference(plan: &Value, path: &str, kind: &str) {
    assert!(
        plan["references"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reference| { reference["relativePath"] == path && reference["kind"] == kind }),
        "missing {kind} reference for {path}: {plan}"
    );
}

fn assert_no_path_checkpoint(harness: &Harness) {
    let parked: bool = harness.db().query_row(
        "SELECT EXISTS(SELECT 1 FROM checkpoints WHERE pulsation_id=?1 AND kind='path_confirmation')",
        [&harness.pulse],
        |row| row.get(0),
    ).unwrap();
    assert!(
        !parked,
        "automatic destination planning must not ask for file names"
    );
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
fn a_pathless_foundation_task_freezes_automatic_destinations_before_specification() {
    let env = IsolatedTestEnvironment::new();
    let harness = Harness::start_in(env, vec![foundation_task_with_no_path()], |facts, _, _| {
        completed_reply(facts)
    });
    let manifest = manifest_json(&harness);
    assert_eq!(manifest["pathConfirmationSource"], "forge_gate");
    let destination = &manifest["taskDestinations"][0];
    assert_eq!(destination["taskId"], harness.task_id());
    assert_eq!(destination["origin"], "repository_convention");
    let paths = destination["paths"].as_array().unwrap();
    assert!(!paths.is_empty());
    let plan = specification_plan(&harness);
    for path in paths {
        assert_reference(&plan, path.as_str().unwrap(), "planned_destination");
    }
    assert_no_path_checkpoint(&harness);
}

#[test]
fn a_legacy_pathless_batch_keeps_its_checkpoint_and_freezes_the_recovery_hint() {
    let env = IsolatedTestEnvironment::new();
    let harness = Harness::start_prepared(
        env,
        vec![foundation_task_with_no_path()],
        CompositionSettings {
            enabled: true,
            ..Default::default()
        }
        .with_all_recipes_qualified(),
        |facts, _, _| completed_reply(facts),
        |harness| {
            // This is the manifest an existing pulsation stored before
            // automatic destination planning. Mutate before the worker starts
            // rather than racing Intake's acceptance.
            let mut manifest = manifest_json(harness);
            let object = manifest.as_object_mut().unwrap();
            object.remove("taskDestinations");
            object.remove("destinationNotices");
            object.insert(
                "pathConfirmationSource".into(),
                Value::String("none".into()),
            );
            let encoded = serde_json::to_string(&manifest).unwrap();
            harness.db().execute(
                "UPDATE pulsation_batches SET manifest_json=?2,manifest_sha256=?3 WHERE pulsation_id=?1",
                rusqlite::params![harness.pulse, encoded, ContentSha256::of(encoded.as_bytes()).as_str()],
            ).unwrap();
        },
    );

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

    let manifest = manifest_json(&harness);
    assert_eq!(manifest["pathConfirmationSource"], "manual_hitl");
    assert_eq!(manifest["taskDestinations"][0]["taskId"], harness.task_id());
    assert_eq!(manifest["taskDestinations"][0]["origin"], "manual");
    assert_eq!(
        manifest["taskDestinations"][0]["paths"],
        serde_json::json!(["src/layout.rs"])
    );
    assert_reference(
        &specification_plan(&harness),
        "src/layout.rs",
        "planned_destination",
    );
}

#[test]
fn a_batch_with_a_confirmed_path_never_parks_on_the_preventive_checkpoint() {
    let mut task = foundation_task_with_no_path();
    task.description = "Lay out the crate's first modules in src/layout.rs.".into();
    let env = IsolatedTestEnvironment::new();
    let harness = Harness::start_in(env, vec![task], |facts, _, _| completed_reply(facts));

    assert_eq!(
        manifest_json(&harness)["pathConfirmationSource"],
        "forge_gate"
    );
    assert_eq!(
        manifest_json(&harness)["taskDestinations"][0]["origin"],
        "task_text"
    );
    assert_reference(
        &specification_plan(&harness),
        "src/layout.rs",
        "planned_destination",
    );
    assert_no_path_checkpoint(&harness);
}

#[test]
fn a_pathless_bugfix_resolves_the_existing_handler_and_delivers_it_to_specification() {
    let task = TaskDraft {
        title: "Fix the request handler counter".into(),
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
    std::fs::create_dir_all(env.workspace_dir().join("src")).unwrap();
    std::fs::write(
        env.workspace_dir().join("src/handler.rs"),
        "pub fn count() -> usize { 1 }\n",
    )
    .unwrap();
    let harness = Harness::start_in(env, vec![task], |facts, _, _| completed_reply(facts));

    let manifest = manifest_json(&harness);
    assert_eq!(manifest["pathConfirmationSource"], "forge_gate");
    assert_eq!(
        manifest["taskDestinations"][0]["origin"],
        "repository_convention"
    );
    assert_eq!(
        manifest["taskDestinations"][0]["paths"],
        serde_json::json!(["src/handler.rs", "tests/handler.rs"])
    );
    let plan = specification_plan(&harness);
    assert_reference(&plan, "src/handler.rs", "existing_source");
    assert_reference(&plan, "tests/handler.rs", "planned_destination");
    assert_no_path_checkpoint(&harness);
}
