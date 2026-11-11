use super::*;
use crate::chat_service::{ChatExecutionTargetRequest, ChatMessage};
use crate::companion_drawings::semantics::{CanvasSemanticElementV1, CanvasSemanticKindV1};
use crate::companion_drawings::{CanvasElementKindV1, CanvasElementV1};
use crate::terminals::CliBackend;

fn message() -> ChatMessage {
    ChatMessage {
        id: "answer-a".into(),
        conversation_id: "conversation-a".into(),
        ordinal: 1,
        role: "assistant".into(),
        content: "A interface chama o servi\u{e7}o local.".into(),
        status: "completed".into(),
        execution_id: None,
        generation_id: None,
        last_sequence: 1,
        input_tokens: None,
        output_tokens: None,
        estimated_cost_usd: None,
        error_code: None,
        error_message: None,
        target_label: None,
        created_at: "2026-10-28T00:00:00Z".into(),
        updated_at: "2026-10-28T00:00:00Z".into(),
    }
}

fn session() -> CanvasAuthoringSession {
    let source =
        CanvasAuthoringSource::capture_final_message("project-a", &message(), "pt-BR", None)
            .unwrap();
    let target = CanvasCompanionTargetSnapshot::capture_from_host(
        ChatExecutionTargetRequest::Api {
            connection_id: "connection-a".into(),
        },
        "model-a",
    )
    .unwrap();
    CanvasAuthoringSession::from_host_acceptance(
        "session-a",
        source,
        target,
        CanvasProfileV1::Architecture,
        "Arquitetura proposta",
        100,
        200,
    )
    .unwrap()
}

fn element(id: &str) -> CanvasElementV1 {
    let mut shape = CanvasElementV1::new(
        CanvasElementKindV1::Rectangle { round: false },
        0.,
        0.,
        120.,
        80.,
    );
    shape.id = id.into();
    shape.label = "Servi\u{e7}o".into();
    shape
}

fn semantic(id: &str) -> CanvasSemanticElementV1 {
    CanvasSemanticElementV1 {
        element_id: id.into(),
        kind: CanvasSemanticKindV1::Component,
        participant_id: None,
        lane_id: None,
        source_id: None,
        target_id: None,
    }
}

fn batch(id: &str, revision: u64, commands: Vec<CanvasCommandV1>) -> CanvasAuthoringBatchV1 {
    CanvasAuthoringBatchV1 {
        schema_version: 1,
        operation_id: id.into(),
        expected_revision: revision,
        commands,
    }
}

fn add(id: &str) -> CanvasCommandV1 {
    CanvasCommandV1::UpsertElement {
        element: Box::new(element(id)),
        semantic: Some(semantic(id)),
    }
}

#[test]
fn canvas_authoring_upsert_round_trip_keeps_the_element_as_a_json_object() {
    let command = add("node-a");
    let encoded = serde_json::to_value(&command).unwrap();
    assert_eq!(encoded["kind"], "upsert_element");
    assert!(encoded["element"].is_object());
    assert_eq!(
        encoded["element"],
        serde_json::to_value(element("node-a")).unwrap()
    );
    let decoded: CanvasCommandV1 = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded, command);
    let request = batch("op-a", 0, vec![decoded]);
    assert_eq!(
        CanvasAuthoringBatchV1::parse_json(&serde_json::to_string(&request).unwrap()).unwrap(),
        request
    );
}

#[test]
fn canvas_authoring_captures_only_completed_assistant_sources_and_freezes_target() {
    for (role, status) in [
        ("user", "completed"),
        ("assistant", "streaming"),
        ("assistant", "cancelled"),
    ] {
        let mut input = message();
        input.role = role.into();
        input.status = status.into();
        assert!(
            CanvasAuthoringSource::capture_final_message("project-a", &input, "pt", None).is_err()
        );
    }
    let mut input = message();
    let source =
        CanvasAuthoringSource::capture_final_message("project-a", &input, "pt", None).unwrap();
    input.content = "Changed later".into();
    assert_eq!(
        source.explanation(),
        "A interface chama o servi\u{e7}o local."
    );
    assert_eq!(source.final_message_id(), "answer-a");
    assert_eq!(source.source_sha256().len(), 64);
    assert!(
        CanvasCompanionTargetSnapshot::capture_from_host(
            ChatExecutionTargetRequest::Cli {
                backend: CliBackend::Custom,
                model: None,
                model_profile: None
            },
            "default",
        )
        .is_err()
    );
    assert!(
        CanvasCompanionTargetSnapshot::capture_from_host(
            ChatExecutionTargetRequest::Cli {
                backend: CliBackend::Codex,
                model: Some("model-a".into()),
                model_profile: None
            },
            "model-b",
        )
        .is_err()
    );
    let api = CanvasCompanionTargetSnapshot::capture_from_host(
        ChatExecutionTargetRequest::Api {
            connection_id: "connection-a".into(),
        },
        "model-a",
    )
    .unwrap();
    assert_eq!(api.model_id(), "model-a");
    assert!(
        matches!(api.target(), ChatExecutionTargetRequest::Api { connection_id } if connection_id == "connection-a")
    );
}

#[test]
fn canvas_authoring_cli_snapshot_matches_companion_default_and_profile_contracts() {
    let default = ChatExecutionTargetRequest::Cli {
        backend: CliBackend::Codex,
        model: None,
        model_profile: None,
    };
    assert!(CanvasCompanionTargetSnapshot::capture_from_host(default.clone(), "model-a").is_err());
    assert_eq!(
        CanvasCompanionTargetSnapshot::capture_from_host(default, "default")
            .unwrap()
            .model_id(),
        "default"
    );
    assert!(
        CanvasCompanionTargetSnapshot::capture_from_host(
            ChatExecutionTargetRequest::Cli {
                backend: CliBackend::Gemini,
                model: Some("model-a".into()),
                model_profile: Some("high".into()),
            },
            "model-a"
        )
        .is_err()
    );
    for length in [100, 101] {
        let target = ChatExecutionTargetRequest::Cli {
            backend: CliBackend::Codex,
            model: Some("model-a".into()),
            model_profile: Some("x".repeat(length)),
        };
        assert_eq!(
            CanvasCompanionTargetSnapshot::capture_from_host(target, "model-a").is_ok(),
            length == 100
        );
    }
    for model in ["x".repeat(201), "invalid model".into()] {
        assert!(
            CanvasCompanionTargetSnapshot::capture_from_host(
                ChatExecutionTargetRequest::Cli {
                    backend: CliBackend::Codex,
                    model: Some(model.clone()),
                    model_profile: None,
                },
                &model
            )
            .is_err()
        );
    }
}

#[test]
fn canvas_authoring_source_bounds_redaction_optional_document_and_ttl_are_host_owned() {
    let mut input = message();
    input.content =
        "token: sk-proj-abcdefghijklmnopqrstuvwxyz1234567890ABCDEFGHIJKLMNOPQRSTUVWXYZ".into();
    let source = CanvasAuthoringSource::capture_final_message(
        "project-a",
        &input,
        "pt",
        Some(CanvasSourceDocumentV1 {
            document_id: "architecture-a".into(),
            revision: "revision-a".into(),
        }),
    )
    .unwrap();
    assert!(!source.explanation().contains("sk-proj-"));
    assert_eq!(source.document().unwrap().revision, "revision-a");
    input.content = "x".repeat(MAX_AUTHORING_SOURCE_BYTES + 1);
    assert!(CanvasAuthoringSource::capture_final_message("project-a", &input, "pt", None).is_err());
    let original = session();
    for expires in [100, 99, 100 + MAX_AUTHORING_SESSION_SECONDS + 1, u64::MAX] {
        assert!(
            CanvasAuthoringSession::from_host_acceptance(
                "session-a",
                original.source().clone(),
                original.target().clone(),
                CanvasProfileV1::Architecture,
                "Diagram",
                100,
                expires,
            )
            .is_err()
        );
    }
}

#[test]
fn canvas_authoring_wire_schema_cannot_issue_authority_or_widen_scope() {
    let valid = serde_json::to_value(batch("op-a", 0, vec![add("node-a")])).unwrap();
    for field in [
        "approved",
        "project_id",
        "session_id",
        "origin",
        "scope",
        "target",
        "profile",
    ] {
        let mut forged = valid.clone();
        forged[field] = serde_json::json!(true);
        assert!(
            CanvasAuthoringBatchV1::parse_json(&forged.to_string()).is_err(),
            "field {field}"
        );
    }
    let mut forged = valid.clone();
    forged["commands"][0]["approved"] = serde_json::json!(true);
    assert!(CanvasAuthoringBatchV1::parse_json(&forged.to_string()).is_err());
    assert!(
        CanvasAuthoringBatchV1::parse_json(&" ".repeat(MAX_AUTHORING_BATCH_BYTES + 1)).is_err()
    );
    let mut wrong_version = batch("op-a", 0, vec![add("node-a")]);
    wrong_version.schema_version = 99;
    assert!(
        CanvasAuthoringBatchV1::parse_json(&serde_json::to_string(&wrong_version).unwrap())
            .is_err()
    );
}

#[test]
fn canvas_authoring_fences_project_session_origin_and_lifecycle_before_mutation() {
    for field in ["project", "conversation", "session", "origin"] {
        let mut state = session();
        let before = state.inspect(state.context(), 100).unwrap();
        let mut context = state.context().clone();
        match field {
            "project" => context.project_id = "project-b".into(),
            "conversation" => context.conversation_id = "conversation-b".into(),
            "session" => context.session_id = "session-b".into(),
            _ => context.origin = "untrusted-tool-output".into(),
        }
        assert!(
            state
                .apply(&context, &batch("op-a", 0, vec![add("node-a")]), 101)
                .is_err()
        );
        assert_eq!(state.inspect(state.context(), 101).unwrap(), before);
    }
    for mode in ["expired", "cancelled", "closed"] {
        let mut state = session();
        let context = state.context().clone();
        let now = if mode == "expired" { 200 } else { 101 };
        if mode == "cancelled" {
            state.cancel();
        }
        if mode == "closed" {
            state.close();
        }
        assert!(
            state
                .apply(&context, &batch("op-a", 0, vec![add("node-a")]), now)
                .is_err()
        );
        assert!(
            state
                .finish_candidate(&context, "finish-a", 0, now)
                .is_err()
        );
        assert_eq!(state.revision(), 0);
    }
}

#[test]
fn canvas_authoring_batch_is_atomic_and_retries_return_original_receipt() {
    let mut state = session();
    let context = state.context().clone();
    let first = batch("op-a", 0, vec![add("node-a")]);
    let receipt = state.apply(&context, &first, 101).unwrap();
    assert_eq!(receipt.revision, 1);
    assert_eq!(state.apply(&context, &first, 101).unwrap(), receipt);
    assert_eq!(state.revision(), 1);
    state
        .apply(&context, &batch("op-b", 1, vec![add("node-b")]), 102)
        .unwrap();
    assert_eq!(state.apply(&context, &first, 103).unwrap(), receipt);
    assert_eq!(state.revision(), 2);
    let before = state.inspect(&context, 103).unwrap();
    assert!(
        state
            .apply(&context, &batch("op-a", 0, vec![add("other")]), 103)
            .is_err()
    );
    assert!(
        state
            .apply(&context, &batch("stale", 0, vec![add("node-c")]), 103)
            .is_err()
    );
    let mut invalid = element("invalid");
    invalid.width = f32::NAN;
    let broken = batch(
        "broken",
        2,
        vec![
            add("node-c"),
            CanvasCommandV1::UpsertElement {
                element: Box::new(invalid),
                semantic: Some(semantic("invalid")),
            },
        ],
    );
    assert!(state.apply(&context, &broken, 103).is_err());
    assert_eq!(state.inspect(&context, 103).unwrap(), before);
    assert!(
        state
            .apply(&context, &batch("empty", 2, vec![]), 103)
            .is_err()
    );
    assert!(
        state
            .apply(
                &context,
                &batch("large", 2, vec![add("x"); MAX_AUTHORING_COMMANDS + 1]),
                103
            )
            .is_err()
    );
}

#[test]
fn canvas_authoring_shared_reducer_preserves_metadata_and_matches_semantic_ids() {
    let state = session();
    let original = state.inspect(state.context(), 100).unwrap().scene;
    let authored = apply_canvas_commands(
        &original,
        &[
            add("node-a"),
            CanvasCommandV1::SetNarrative {
                narrative: "A interface usa o servi\u{e7}o.".into(),
            },
        ],
    )
    .unwrap();
    let metadata = authored.document.as_ref().unwrap();
    assert_eq!(metadata.language, "pt-BR");
    assert_eq!(metadata.profile, CanvasProfileV1::Architecture);
    assert_eq!(
        metadata.provenance,
        original.document.as_ref().unwrap().provenance
    );
    assert_eq!(metadata.narrative, "A interface usa o servi\u{e7}o.");
    let mut moved = authored.elements[0].clone();
    moved.x = 200.;
    let moved_scene = apply_canvas_commands(
        &authored,
        &[CanvasCommandV1::UpsertElement {
            element: Box::new(moved),
            semantic: None,
        }],
    )
    .unwrap();
    assert_eq!(
        moved_scene.document.as_ref().unwrap().elements,
        metadata.elements
    );
    assert!(
        apply_canvas_commands(
            &authored,
            &[CanvasCommandV1::UpsertElement {
                element: Box::new(element("node-b")),
                semantic: Some(semantic("wrong-id")),
            }]
        )
        .is_err()
    );
    assert!(
        apply_canvas_commands(
            &authored,
            &[CanvasCommandV1::UpsertElement {
                element: Box::new(element("node-b")),
                semantic: None,
            }]
        )
        .is_err()
    );
    assert_eq!(original.elements.len(), 0);
}

#[test]
fn canvas_authoring_legacy_reducer_does_not_infer_profile_or_grant() {
    let legacy = CanvasSceneV1::default();
    let updated = apply_canvas_commands(
        &legacy,
        &[CanvasCommandV1::UpsertElement {
            element: Box::new(element("a")),
            semantic: None,
        }],
    )
    .unwrap();
    assert!(updated.document.is_none());
    assert!(apply_canvas_commands(&legacy, &[add("a")]).is_err());
    assert!(
        apply_canvas_commands(
            &legacy,
            &[CanvasCommandV1::SetNarrative {
                narrative: "No profile".into()
            }]
        )
        .is_err()
    );
}

#[test]
fn canvas_authoring_finish_is_only_a_candidate_with_pinned_source_and_frozen_target() {
    let mut state = session();
    let context = state.context().clone();
    assert!(
        state
            .finish_candidate(&context, "finish-a", 0, 101)
            .is_err()
    );
    state
        .apply(&context, &batch("op-a", 0, vec![add("node-a")]), 101)
        .unwrap();
    assert!(
        state
            .finish_candidate(&context, "finish-a", 0, 101)
            .is_err()
    );
    let candidate = state
        .finish_candidate(&context, "finish-a", 1, 102)
        .unwrap();
    assert_eq!(candidate.title, "Arquitetura proposta");
    assert_eq!(candidate.source.final_message_id(), "answer-a");
    assert_eq!(candidate.target.model_id(), "model-a");
    assert_eq!(candidate.revision, 1);
    assert_eq!(
        state
            .finish_candidate(&context, "finish-a", 1, 103)
            .unwrap(),
        candidate
    );
    assert!(
        state
            .finish_candidate(&context, "different-finish", 1, 103)
            .is_err()
    );
    assert!(
        state
            .apply(&context, &batch("op-b", 1, vec![add("node-b")]), 103)
            .is_err()
    );
    assert_eq!(state.state(), CanvasAuthoringState::Finished);
}

#[test]
fn canvas_authoring_source_can_exceed_local_narrative_limit_without_truncation() {
    let mut input = message();
    input.content = "x".repeat(super::super::MAX_LABEL_CHARS + 1);
    let source =
        CanvasAuthoringSource::capture_final_message("project-a", &input, "en", None).unwrap();
    let baseline = session();
    let state = CanvasAuthoringSession::from_host_acceptance(
        "session-b",
        source,
        baseline.target().clone(),
        CanvasProfileV1::Architecture,
        "Diagram",
        100,
        200,
    )
    .unwrap();
    assert_eq!(state.source().explanation(), input.content);
    assert_eq!(
        state
            .inspect(state.context(), 100)
            .unwrap()
            .scene
            .document
            .unwrap()
            .narrative,
        ""
    );
}

#[test]
fn canvas_authoring_candidate_can_still_be_cancelled_before_durable_publication() {
    for mode in ["cancelled", "closed", "expired"] {
        let mut state = session();
        let context = state.context().clone();
        state
            .apply(&context, &batch("op-a", 0, vec![add("node-a")]), 101)
            .unwrap();
        state
            .finish_candidate(&context, "finish-a", 1, 102)
            .unwrap();
        if mode == "cancelled" {
            state.cancel();
        }
        if mode == "closed" {
            state.close();
        }
        assert!(
            state
                .finish_candidate(
                    &context,
                    "finish-a",
                    1,
                    if mode == "expired" { 200 } else { 103 }
                )
                .is_err()
        );
    }
}

#[test]
fn canvas_authoring_reducer_cascades_deleted_relationships_and_preserves_unrelated_nodes() {
    let state = session();
    let initial = state.inspect(state.context(), 100).unwrap().scene;
    let mut line = CanvasElementV1::new(
        CanvasElementKindV1::Line {
            points: [[0., 0.], [200., 0.]],
        },
        0.,
        0.,
        200.,
        0.,
    );
    line.id = "edge-a".into();
    line.label = "Calls".into();
    let mut relation = semantic("edge-a");
    relation.kind = CanvasSemanticKindV1::Relationship;
    relation.source_id = Some("node-a".into());
    relation.target_id = Some("node-b".into());
    let connected = apply_canvas_commands(
        &initial,
        &[
            add("node-a"),
            add("node-b"),
            CanvasCommandV1::UpsertElement {
                element: Box::new(line),
                semantic: Some(relation),
            },
        ],
    )
    .unwrap();
    let deleted = apply_canvas_commands(
        &connected,
        &[CanvasCommandV1::RemoveElement {
            element_id: "node-a".into(),
        }],
    )
    .unwrap();
    assert_eq!(deleted.elements.len(), 1);
    assert_eq!(deleted.elements[0].id, "node-b");
    assert_eq!(deleted.document.unwrap().elements.len(), 1);
    assert_eq!(connected.elements.len(), 3);
}

#[test]
fn canvas_authoring_receipts_are_bounded_and_failed_requests_do_not_consume_ids() {
    let mut state = session();
    let context = state.context().clone();
    for index in 0..MAX_AUTHORING_REQUESTS {
        state
            .apply(
                &context,
                &batch(&format!("op-{index}"), index as u64, vec![add("node-a")]),
                101,
            )
            .unwrap();
    }
    assert_eq!(state.revision(), MAX_AUTHORING_REQUESTS as u64);
    assert!(
        state
            .apply(
                &context,
                &batch("too-many", state.revision(), vec![add("node-b")]),
                101
            )
            .is_err()
    );
    assert_eq!(
        state
            .apply(&context, &batch("op-0", 0, vec![add("node-a")]), 101)
            .unwrap()
            .revision,
        1
    );
    let mut other = session();
    let other_context = other.context().clone();
    assert!(
        other
            .apply(
                &other_context,
                &batch("retry", 99, vec![add("node-a")]),
                101
            )
            .is_err()
    );
    assert_eq!(
        other
            .apply(&other_context, &batch("retry", 0, vec![add("node-a")]), 101)
            .unwrap()
            .revision,
        1
    );
}

#[test]
fn canvas_authoring_finish_preserves_completeness_notices_without_treating_them_as_invalid_connections()
 {
    let mut state = session();
    let context = state.context().clone();
    let mut unlabeled = element("node-a");
    unlabeled.label.clear();
    state
        .apply(
            &context,
            &batch(
                "op-a",
                0,
                vec![CanvasCommandV1::UpsertElement {
                    element: Box::new(unlabeled),
                    semantic: Some(semantic("node-a")),
                }],
            ),
            101,
        )
        .unwrap();
    let candidate = state
        .finish_candidate(&context, "finish-a", 1, 102)
        .unwrap();
    assert!(
        candidate
            .diagnostics
            .iter()
            .any(|notice| notice.code == "missing_label")
    );
    assert_eq!(candidate.scene.elements.len(), 1);
}
