//! Semantic Canvas contracts through the real project-scoped service and native mirrors.
#![allow(
    clippy::unwrap_used,
    reason = "Isolated integration fixtures fail immediately"
)]

use ikore_core::companion_drawings::semantics::{
    CanvasDocumentV1, CanvasProfileV1, CanvasProvenanceV1, CanvasSemanticElementV1,
    CanvasSemanticKindV1,
};
use ikore_core::companion_drawings::{
    ArrowBindingV1, CanvasElementKindV1, CanvasElementV1, CanvasSceneV1, CompanionDrawingsService,
};
use ikore_core::persistence_service::{EnsureProjectForWorkspaceRequest, PersistenceService};
use ikore_test_support::CoreTestHarness;
use rusqlite::Connection;
use std::fs;

fn register(harness: &CoreTestHarness) -> String {
    harness
        .core()
        .persistence()
        .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
            workspace_path: harness.workspace_dir().to_string_lossy().into_owned(),
            preferred_name: Some("Semantic Canvas".into()),
        })
        .unwrap()
        .project
        .id
}

fn entry(id: &str, kind: CanvasSemanticKindV1) -> CanvasSemanticElementV1 {
    CanvasSemanticElementV1 {
        element_id: id.into(),
        kind,
        participant_id: None,
        lane_id: None,
        source_id: None,
        target_id: None,
    }
}

fn architecture() -> CanvasSceneV1 {
    let mut ui = CanvasElementV1::new(
        CanvasElementKindV1::Rectangle { round: true },
        20.,
        20.,
        120.,
        80.,
    );
    ui.id = "ui".into();
    ui.label = "Interface".into();
    let mut core = ui.clone();
    core.id = "core".into();
    core.x = 260.;
    core.label = "Servi\u{e7}o".into();
    let mut arrow = CanvasElementV1::new(
        CanvasElementKindV1::Arrow {
            points: [[0., 0.], [120., 0.]],
            bidirectional: false,
            start_binding: Some(ArrowBindingV1 {
                element_id: "ui".into(),
                anchor: [1., 0.],
                gap: 0.,
            }),
            end_binding: Some(ArrowBindingV1 {
                element_id: "core".into(),
                anchor: [-1., 0.],
                gap: 0.,
            }),
        },
        140.,
        60.,
        120.,
        0.,
    );
    arrow.id = "request".into();
    arrow.label = "Chamada".into();
    let mut relationship = entry("request", CanvasSemanticKindV1::Relationship);
    relationship.source_id = Some("ui".into());
    relationship.target_id = Some("core".into());
    CanvasSceneV1 {
        elements: vec![ui, core, arrow],
        document: Some(CanvasDocumentV1 {
            schema_version: 1,
            profile: CanvasProfileV1::Architecture,
            language: "pt-BR".into(),
            narrative: "A interface chama o servi\u{e7}o do projeto.".into(),
            provenance: Some(CanvasProvenanceV1 {
                source_id: "explanation-1".into(),
                source_revision: Some(1),
                source_sha256: "a".repeat(64),
            }),
            elements: vec![
                entry("ui", CanvasSemanticKindV1::Component),
                entry("core", CanvasSemanticKindV1::Component),
                relationship,
            ],
        }),
        ..Default::default()
    }
}

#[test]
fn canvas_semantic_document_survives_sqlite_mirror_reopen_and_project_fencing() {
    let harness = CoreTestHarness::new();
    let project = register(&harness);
    let service = harness.core().drawings();
    let initial = service.create(&project, "Architecture").unwrap();
    let saved = service
        .save(
            &project,
            &initial.id,
            initial.revision,
            &initial.title,
            &architecture(),
            &initial.viewport,
        )
        .unwrap();
    service.sync_project(&project).unwrap();
    let json_path = harness
        .workspace_dir()
        .join(".ikore/drawings")
        .join(format!("{}.ikore-canvas.json", saved.basename));
    let mirror: serde_json::Value = serde_json::from_slice(&fs::read(json_path).unwrap()).unwrap();
    assert_eq!(mirror["scene"]["document"]["profile"], "architecture");
    assert_eq!(mirror["scene"]["document"]["language"], "pt-BR");
    let reopened =
        CompanionDrawingsService::new(PersistenceService::initialize(harness.data_dir()).unwrap());
    assert_eq!(
        reopened.get(&project, &saved.id).unwrap().scene,
        saved.scene
    );
    assert!(reopened.get("another-project", &saved.id).is_err());
    assert_eq!(harness.embeddings().call_count(), 0);
}

#[test]
fn canvas_semantic_invalid_save_preserves_revision_and_pending_mirror_count() {
    let harness = CoreTestHarness::new();
    let project = register(&harness);
    let service = harness.core().drawings();
    let initial = service.create(&project, "Architecture").unwrap();
    service.sync_project(&project).unwrap();
    let mut invalid = architecture();
    invalid.document.as_mut().unwrap().elements[2].target_id = Some("missing".into());
    assert!(
        service
            .save(
                &project,
                &initial.id,
                initial.revision,
                &initial.title,
                &invalid,
                &initial.viewport,
            )
            .is_err()
    );
    let loaded = service.get(&project, &initial.id).unwrap();
    assert_eq!(loaded.revision, initial.revision);
    assert_eq!(loaded.scene, initial.scene);
    let db = Connection::open(harness.data_dir().join("ikore.db")).unwrap();
    let pending: i64 = db
        .query_row(
            "SELECT count(*) FROM companion_drawing_sync_jobs",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(pending, 0);
}

#[test]
fn canvas_semantic_duplicate_remaps_all_semantic_ids_and_keeps_source() {
    let harness = CoreTestHarness::new();
    let project = register(&harness);
    let service = harness.core().drawings();
    let initial = service.create(&project, "Architecture").unwrap();
    let saved = service
        .save(
            &project,
            &initial.id,
            initial.revision,
            &initial.title,
            &architecture(),
            &initial.viewport,
        )
        .unwrap();
    let copy = service.duplicate(&project, &saved.id).unwrap();
    let document = copy.scene.document.as_ref().unwrap();
    assert_eq!(
        document.provenance,
        saved.scene.document.unwrap().provenance
    );
    for semantic in &document.elements {
        assert!(
            copy.scene
                .elements
                .iter()
                .any(|e| e.id == semantic.element_id)
        );
        assert!(!["ui", "core", "request"].contains(&semantic.element_id.as_str()));
        for referenced in [&semantic.source_id, &semantic.target_id]
            .into_iter()
            .flatten()
        {
            assert!(
                document
                    .elements
                    .iter()
                    .any(|e| &e.element_id == referenced)
            );
        }
    }
    assert!(ikore_core::companion_drawings::normalize_scene(&copy.scene).is_ok());
}

#[test]
fn canvas_semantic_narrative_redaction_precedes_database_journal_and_export() {
    let harness = CoreTestHarness::new();
    let project = register(&harness);
    let service = harness.core().drawings();
    let initial = service.create(&project, "Architecture").unwrap();
    let secret = format!("{}{}", "sk-proj-", "syntheticfixture0123456789");
    let mut scene = architecture();
    scene.document.as_mut().unwrap().narrative = format!("Architecture {secret}");
    let saved = service
        .save(
            &project,
            &initial.id,
            initial.revision,
            &initial.title,
            &scene,
            &initial.viewport,
        )
        .unwrap();
    assert!(
        !saved
            .scene
            .document
            .as_ref()
            .unwrap()
            .narrative
            .contains(&secret)
    );
    let db = Connection::open(harness.data_dir().join("ikore.db")).unwrap();
    let stored: String = db
        .query_row(
            "SELECT scene_json FROM companion_drawings WHERE id=?1",
            [&saved.id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!stored.contains(&secret));
    let queued: String = db
        .query_row(
            "SELECT json_content FROM companion_drawing_sync_jobs WHERE drawing_id=?1",
            [&saved.id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!queued.contains(&secret));
    service.sync_project(&project).unwrap();
    let mirrored = fs::read_to_string(
        harness
            .workspace_dir()
            .join(".ikore/drawings")
            .join(format!("{}.ikore-canvas.json", saved.basename)),
    )
    .unwrap();
    assert!(!mirrored.contains(&secret));
}

#[test]
fn canvas_semantic_legacy_scene_stays_untyped_and_future_contract_is_rejected() {
    let scene = CanvasSceneV1::default();
    let legacy_json = serde_json::to_value(&scene).unwrap();
    assert!(legacy_json.get("document").is_none());
    let legacy: CanvasSceneV1 = serde_json::from_value(legacy_json).unwrap();
    assert!(legacy.document.is_none());
    let mut future = architecture();
    future.document.as_mut().unwrap().schema_version = 99;
    assert!(ikore_core::companion_drawings::normalize_scene(&future).is_err());
}

#[test]
fn canvas_semantic_process_duplicate_remaps_participant_lane_and_sequence_references() {
    let harness = CoreTestHarness::new();
    let project = register(&harness);
    let service = harness.core().drawings();
    let mut scene = architecture();
    let mut pool = CanvasElementV1::new(
        CanvasElementKindV1::Rectangle { round: false },
        0.,
        -100.,
        600.,
        300.,
    );
    pool.id = "pool".into();
    pool.label = "Participant".into();
    let mut lane = pool.clone();
    lane.id = "lane".into();
    lane.label = "Engineering".into();
    lane.y = 0.;
    lane.height = 180.;
    scene.elements.extend([pool, lane]);
    let document = scene.document.as_mut().unwrap();
    document.profile = CanvasProfileV1::Bpmn;
    for semantic in &mut document.elements {
        if semantic.kind == CanvasSemanticKindV1::Component {
            semantic.kind = CanvasSemanticKindV1::ServiceTask;
            semantic.participant_id = Some("pool".into());
            semantic.lane_id = Some("lane".into());
        } else {
            semantic.kind = CanvasSemanticKindV1::SequenceFlow;
        }
    }
    document
        .elements
        .push(entry("pool", CanvasSemanticKindV1::Participant));
    let mut lane_entry = entry("lane", CanvasSemanticKindV1::Lane);
    lane_entry.participant_id = Some("pool".into());
    document.elements.push(lane_entry);
    let original = service.create(&project, "Process draft").unwrap();
    let saved = service
        .save(
            &project,
            &original.id,
            original.revision,
            &original.title,
            &scene,
            &original.viewport,
        )
        .unwrap();
    service.sync_project(&project).unwrap();
    let reopened =
        CompanionDrawingsService::new(PersistenceService::initialize(harness.data_dir()).unwrap());
    assert_eq!(
        reopened.get(&project, &saved.id).unwrap().scene,
        saved.scene
    );
    let duplicate = reopened.duplicate(&project, &saved.id).unwrap();
    let document = duplicate.scene.document.as_ref().unwrap();
    let pool = document
        .elements
        .iter()
        .find(|e| e.kind == CanvasSemanticKindV1::Participant)
        .unwrap();
    let lane = document
        .elements
        .iter()
        .find(|e| e.kind == CanvasSemanticKindV1::Lane)
        .unwrap();
    assert_ne!(pool.element_id, "pool");
    assert_ne!(lane.element_id, "lane");
    assert_eq!(
        lane.participant_id.as_deref(),
        Some(pool.element_id.as_str())
    );
    for task in document
        .elements
        .iter()
        .filter(|e| e.kind == CanvasSemanticKindV1::ServiceTask)
    {
        assert_eq!(
            task.participant_id.as_deref(),
            Some(pool.element_id.as_str())
        );
        assert_eq!(task.lane_id.as_deref(), Some(lane.element_id.as_str()));
    }
    assert!(ikore_core::companion_drawings::normalize_scene(&duplicate.scene).is_ok());
}
