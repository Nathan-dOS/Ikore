//! ADR-035 M2 over the real pipeline v4: the trust layers at every
//! checkpoint of a composed pulsation, a model other than the one requested,
//! a stage that spent its repair budget and a required check that failed.
#![allow(
    clippy::unwrap_used,
    reason = "Test-only integration helpers fail immediately when fixture setup is invalid"
)]
mod composition_support;

use composition_support::*;
use hvc_core::orchestration::SchemaV1;
use hvc_core::orchestration::composition::settings::CompositionSettings;
use hvc_core::orchestration::composition::{CheckKind, Handle, Uuid, ValidationCommandProfileV1};
use hvc_core::orchestration::layers::{LayerKind, LayerState, LayerV1, PulsationLayersSnapshot};
use hvc_core::persistence_service::GetPulsationRuntimeSnapshotRequest;
use hvc_test_support::ScriptedResponse;
use serde_json::json;

const CHECK_FIXTURE: &str = env!("CARGO_BIN_EXE_check_fixture");

fn layers(harness: &Harness) -> PulsationLayersSnapshot {
    harness.service.pulsation_layers(&harness.pulse).unwrap()
}

fn layer(snapshot: &PulsationLayersSnapshot, kind: LayerKind) -> &LayerV1 {
    snapshot.layer(kind).unwrap()
}

fn states(snapshot: &PulsationLayersSnapshot) -> [LayerState; 4] {
    LayerKind::ALL.map(|kind| layer(snapshot, kind).state)
}

#[test]
fn the_layers_follow_a_composed_pulsation_from_specification_to_the_end() {
    use LayerState::{Attention, Clear, Unknown};
    let harness = Harness::start(|facts, _, _| completed_reply(facts));
    harness.wait_for_checkpoint("SPECIFICATION");
    let before = layers(&harness);
    assert_eq!(
        states(&before),
        [Clear, Unknown, Clear, Unknown],
        "before the approval nothing is granted and no criterion is approved: {before:?}"
    );
    let detail = harness
        .service
        .get_pulsation_runtime_snapshot_for_detail(
            GetPulsationRuntimeSnapshotRequest {
                project_id: harness.project.clone(),
            },
            Some(&harness.pulse),
        )
        .unwrap()
        .snapshot
        .details;
    assert_eq!(
        detail[0].layers.as_ref(),
        Some(&before),
        "the detail reads the same layers"
    );
    harness.approve();

    harness.wait_for_checkpoint("Contract");
    let contract = layers(&harness);
    assert_eq!(states(&contract), [Clear, Attention, Clear, Clear]);
    assert!(
        layer(&contract, LayerKind::Identity)
            .headline
            .ends_with("mandate revision 1")
    );
    let scope = layer(&contract, LayerKind::Scope);
    assert!(
        scope.reasons[0].ends_with("sensitive path(s) in the write envelope."),
        "the fixture's `src/auth.rs` is sensitive: {scope:?}"
    );
    assert!(
        scope
            .items
            .iter()
            .any(|item| item.label == "src/auth.rs" && item.value == "granted · sensitive")
    );
    let intent = layer(&contract, LayerKind::Intent);
    assert_eq!(intent.headline, "1 criteria · 0 with a validated unit");
    harness.approve();

    harness.wait_for_checkpoint("Test");
    let test = layers(&harness);
    let intent = layer(&test, LayerKind::Intent);
    assert_eq!(intent.state, Attention);
    assert_eq!(intent.reasons, ["Risk is high."]);
    assert!(
        intent
            .items
            .iter()
            .any(|item| item.label == "Risk reason" && item.value.contains("src/auth.rs")),
        "{intent:?}"
    );
    harness.approve();

    harness.wait_for_checkpoint("Integrator");
    let integrator = layers(&harness);
    let intent = layer(&integrator, LayerKind::Intent);
    assert!(intent.headline.ends_with("1 supported"), "{intent:?}");
    assert!(
        intent
            .reasons
            .contains(&"1 item(s) are unverified or backed by the model only.".to_owned()),
        "no check is configured, so the verdict rests on the model: {intent:?}"
    );
    harness.approve();

    wait_until(&harness.service, &harness.pulse, |db| {
        db.query_row(
            "SELECT status='done' FROM pulsations WHERE id=?1",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap()
    });
    let done = layers(&harness);
    let applied: i64 = harness
        .db()
        .query_row(
            "SELECT COUNT(DISTINCT lower(relative_path)) FROM artifact_publications WHERE pulsation_id=?1 AND job_class='main' AND state='applied'",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        layer(&done, LayerKind::Scope)
            .headline
            .contains(&format!("· {applied} written ·")),
        "{:?}",
        layer(&done, LayerKind::Scope)
    );
    assert!(
        done.layers
            .iter()
            .all(|layer| layer.state != LayerState::Issue),
        "nothing grave happened: {done:?}"
    );
    assert_eq!(
        layer(&done, LayerKind::Identity).state,
        Clear,
        "every model ran as requested"
    );
}

#[test]
fn a_model_other_than_the_one_requested_puts_identity_in_attention() {
    let harness = Harness::start(|facts, _, _| {
        if facts.executor == "code_implementer" {
            ScriptedResponse::json(
                200,
                json!({"model":"gpt-4.1","choices":[{"message":{"content":unit_response(facts).to_string()},"finish_reason":"stop"}],"usage":{"prompt_tokens":120,"completion_tokens":60}}).to_string(),
            )
        } else {
            completed_reply(facts)
        }
    });
    harness.wait_for_checkpoint("SPECIFICATION");
    harness.approve();
    harness.wait_for_checkpoint("Contract");
    harness.approve();
    // The host refuses the answer and parks Code; the layer keeps the fact.
    harness.wait_for_failure_checkpoint();
    let snapshot = layers(&harness);
    let identity = layer(&snapshot, LayerKind::Identity);
    assert_eq!(identity.state, LayerState::Attention, "{identity:?}");
    assert_eq!(
        identity.reasons,
        ["`code_implementer` ran `gpt-4.1`, not the requested `gpt-4.1-mini`."]
    );
    assert_eq!(
        snapshot.worst_flagged().map(|layer| layer.kind),
        Some(LayerKind::Identity),
        "the flow bar names Identity first"
    );
}

#[test]
fn a_stage_that_spent_its_repair_budget_is_a_guardrails_issue() {
    // Both Code specialties answer with an empty payload: the implementer's
    // answer gets one repair from the extra pool, the repair is rejected too,
    // and the unit has nothing left.
    let harness = Harness::start(|facts, _, _| {
        if matches!(
            facts.executor.as_str(),
            "code_implementer" | "code_repairer"
        ) {
            provider_reply(json!({
                "schemaVersion": 1, "outcome": "completed",
                "executorKind": facts.executor, "payload": {}
            }))
        } else {
            completed_reply(facts)
        }
    });
    harness.wait_for_checkpoint("SPECIFICATION");
    harness.approve();
    harness.wait_for_checkpoint("Contract");
    harness.approve();
    harness.wait_for_failure_checkpoint();
    let snapshot = layers(&harness);
    let guardrails = layer(&snapshot, LayerKind::Guardrails);
    assert_eq!(guardrails.state, LayerState::Issue, "{guardrails:?}");
    assert!(
        guardrails
            .reasons
            .contains(&"The Code budget is exhausted with a unit still waiting.".to_owned()),
        "{guardrails:?}"
    );
    assert!(
        guardrails
            .reasons
            .contains(&"Less than 25% of the Code extra output tokens is left.".to_owned()),
        "the repair reserved the extra allowance: {guardrails:?}"
    );
    assert!(guardrails.headline.contains("0% extra budget left"));
}

#[test]
fn a_failed_required_check_is_a_guardrails_issue() {
    let env = hvc_test_support::IsolatedTestEnvironment::new();
    std::fs::create_dir_all(env.workspace_dir().join("src")).unwrap();
    std::fs::write(env.workspace_dir().join("src/auth.rs"), ORIGINAL_SOURCE).unwrap();
    let harness = Harness::start_in(env, vec![expiry_task()], |facts, _, _| {
        completed_reply(facts)
    });
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
                argv: vec!["fail-text".to_owned()],
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
    harness.wait_for_checkpoint("SPECIFICATION");
    harness.approve();
    harness.wait_for_checkpoint("Contract");
    harness.approve();
    harness.wait_for_failure_checkpoint();
    let snapshot = layers(&harness);
    let guardrails = layer(&snapshot, LayerKind::Guardrails);
    assert_eq!(guardrails.state, LayerState::Issue, "{guardrails:?}");
    assert!(
        guardrails
            .reasons
            .contains(&"Required check `fixture_compile` failed.".to_owned()),
        "{guardrails:?}"
    );
    assert!(
        guardrails
            .items
            .iter()
            .any(|item| item.label == "fixture_compile"
                && item.value == "required · failed"
                && item.state == LayerState::Issue)
    );
    assert_eq!(
        guardrails.headline,
        "0/1 required check(s) passed · 100% extra budget left"
    );
}
