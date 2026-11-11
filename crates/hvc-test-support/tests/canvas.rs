//! Native canvas services through the real `AppCore`, SQLite and temporary workspaces.
//! No provider requests, terminal discovery, global environment changes or user data.
#![allow(
    clippy::unwrap_used,
    reason = "Integration fixtures fail immediately when isolated setup is invalid"
)]

use hvc_core::AppEvent;
use hvc_core::companion_drawings::geometry::connector_world_points;
use hvc_core::companion_drawings::{
    ArrowBindingV1, CanvasElementKindV1, CanvasElementV1, CanvasSceneV1, CompanionDrawingsService,
    DrawingSyncStatus,
};
use hvc_core::persistence_service::{
    EnsureProjectForWorkspaceRequest, PersistenceService, Project,
};
use hvc_core::workspace_service::OpenWorkspaceRequest;
use hvc_test_support::CoreTestHarness;
use rusqlite::Connection;
use std::fs;
use std::path::Path;

fn register(core: &hvc_core::AppCore, root: &Path, name: &str) -> Project {
    core.persistence()
        .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
            workspace_path: root.to_string_lossy().into_owned(),
            preferred_name: Some(name.into()),
        })
        .unwrap()
        .project
}

fn architecture() -> CanvasSceneV1 {
    let mut ui = CanvasElementV1::new(
        CanvasElementKindV1::Rectangle { round: true },
        20.0,
        20.0,
        120.0,
        80.0,
    );
    ui.label = "UI".into();
    ui.stroke_rendering = hvc_core::companion_drawings::CanvasStrokeRendering::Sketch;
    ui.label_font_size = Some(24.);
    let mut core = CanvasElementV1::new(
        CanvasElementKindV1::Rectangle { round: true },
        260.0,
        20.0,
        120.0,
        80.0,
    );
    core.label = "Core & ação".into();
    let mut db = CanvasElementV1::new(CanvasElementKindV1::Ellipse, 500.0, 20.0, 120.0, 80.0);
    db.label = "SQLite".into();
    let mut arrow = CanvasElementV1::new(
        CanvasElementKindV1::Arrow {
            points: [[0.0, 0.0], [120.0, 0.0]],
            bidirectional: false,
            start_binding: Some(ArrowBindingV1 {
                element_id: ui.id.clone(),
                anchor: [1.0, 0.0],
                gap: 0.0,
            }),
            end_binding: Some(ArrowBindingV1 {
                element_id: core.id.clone(),
                anchor: [-1.0, 0.0],
                gap: 0.0,
            }),
        },
        140.0,
        60.0,
        120.0,
        0.0,
    );
    arrow.stroke_rendering = hvc_core::companion_drawings::CanvasStrokeRendering::Sketch;
    arrow.connector_curve = hvc_core::companion_drawings::CanvasConnectorCurve::CurveLeft;
    CanvasSceneV1 {
        elements: vec![ui, core, db, arrow],
        ..Default::default()
    }
}

#[test]
fn canvas_app_core_round_trip_bindings_export_and_project_isolation() {
    let harness = CoreTestHarness::new();
    let core = harness.core();
    let project_a = register(&core, harness.workspace_dir(), "Canvas A");
    let workspace_b = tempfile::tempdir().unwrap();
    let project_b = register(&core, workspace_b.path(), "Canvas B");
    let service = core.drawings();
    let mut events = core.subscribe();
    let drawing = service.create(&project_a.id, "Architecture").unwrap();
    let event = events.try_recv().unwrap();
    assert!(
        matches!(event,AppEvent::Drawings(event) if event.project_id==project_a.id && event.drawing_id==drawing.id && event.revision==1)
    );
    let saved = service
        .save(
            &project_a.id,
            &drawing.id,
            drawing.revision,
            &drawing.title,
            &architecture(),
            &drawing.viewport,
        )
        .unwrap();
    assert_eq!(saved.revision, 2);
    assert_eq!(service.sync_project(&project_a.id).unwrap().pending, 0);
    assert!(service.list_summaries(&project_b.id).unwrap().is_empty());
    assert!(service.get(&project_b.id, &drawing.id).is_err());
    let directory = harness.workspace_dir().join(".hvc/drawings");
    let json_path = directory.join(format!("{}.hvc-canvas.json", saved.basename));
    let svg_path = directory.join(format!("{}.svg", saved.basename));
    let exported: serde_json::Value =
        serde_json::from_slice(&fs::read(&json_path).unwrap()).unwrap();
    assert_eq!(exported["drawing_id"], drawing.id);
    let svg = fs::read_to_string(&svg_path).unwrap();
    assert!(svg.contains("font-size=\"24\""));
    assert!(
        svg.contains(" Q "),
        "the project mirror includes the curved connector"
    );
    assert!(
        fs::read_to_string(&svg_path)
            .unwrap()
            .contains("Core &amp; ação")
    );
    let reopened =
        CompanionDrawingsService::new(PersistenceService::initialize(harness.data_dir()).unwrap());
    let loaded = reopened.get(&project_a.id, &drawing.id).unwrap();
    assert_eq!(loaded.scene, saved.scene);
    assert!(
        loaded
            .scene
            .elements
            .iter()
            .any(|element| element.label_font_size == Some(24.))
    );
    assert!(
        loaded
            .scene
            .elements
            .iter()
            .any(|element| element.connector_curve
                == hvc_core::companion_drawings::CanvasConnectorCurve::CurveLeft)
    );
    assert_eq!(loaded.sync_status, DrawingSyncStatus::Synced);
    let before = loaded
        .scene
        .elements
        .iter()
        .find_map(connector_world_points)
        .unwrap();
    let mut edited = loaded.scene.clone();
    edited
        .elements
        .iter_mut()
        .find(|element| element.label == "Core & ação")
        .unwrap()
        .x += 90.0;
    let moved = service
        .save(
            &project_a.id,
            &loaded.id,
            loaded.revision,
            &loaded.title,
            &edited,
            &loaded.viewport,
        )
        .unwrap();
    let after = moved
        .scene
        .elements
        .iter()
        .find_map(connector_world_points)
        .unwrap();
    assert_eq!(after[0], before[0]);
    assert_eq!(after[1][0], before[1][0] + 90.0);
    let duplicate = service.duplicate(&project_a.id, &moved.id).unwrap();
    assert_ne!(duplicate.id, moved.id);
    assert!(
        duplicate.scene.elements.iter().all(|element| !moved
            .scene
            .elements
            .iter()
            .any(|old| old.id == element.id))
    );
    service
        .delete(&project_a.id, &moved.id, moved.revision)
        .unwrap();
    assert_eq!(service.sync_project(&project_a.id).unwrap().pending, 0);
    assert!(!json_path.exists());
    assert!(!svg_path.exists());
    assert!(service.get(&project_a.id, &duplicate.id).is_ok());
    assert_eq!(harness.embeddings().call_count(), 0);
}

#[test]
fn canvas_project_deletion_purges_pending_mirrors_and_cannot_recreate_workspace() {
    let harness = CoreTestHarness::new();
    let core = harness.core();
    let project = register(&core, harness.workspace_dir(), "Delete canvas project");
    let survivor_workspace = tempfile::tempdir().unwrap();
    let survivor = register(&core, survivor_workspace.path(), "Survivor");
    let service = core.drawings();
    let kept = service.create(&survivor.id, "Keep").unwrap();
    service.sync_project(&survivor.id).unwrap();
    let drawing = service.create(&project.id, "Pending diagram").unwrap();
    service.sync_project(&project.id).unwrap();
    service
        .save(
            &project.id,
            &drawing.id,
            drawing.revision,
            &drawing.title,
            &architecture(),
            &drawing.viewport,
        )
        .unwrap();
    core.open_workspace(OpenWorkspaceRequest {
        path: harness.workspace_dir().to_string_lossy().into_owned(),
    })
    .unwrap();
    assert!(
        core.delete_project_permanently(&project.id, "wrong confirmation")
            .is_err()
    );
    assert!(harness.workspace_dir().is_dir());
    let outcome = core
        .delete_project_permanently(&project.id, &project.name)
        .unwrap();
    assert!(outcome.deleted);
    assert!(!harness.workspace_dir().exists());
    // A stale worker/service instance cannot resurrect a project after the guarded purge.
    assert!(service.sync_project(&project.id).is_err());
    assert!(service.create(&project.id, "Stale callback").is_err());
    assert!(!harness.workspace_dir().exists());
    let db = Connection::open(harness.data_dir().join("ikore.db")).unwrap();
    for table in ["companion_drawings", "companion_drawing_sync_jobs"] {
        let count: i64 = db
            .query_row(
                &format!("SELECT count(*) FROM {table} WHERE project_id=?1"),
                [&project.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "{table}");
    }
    assert!(service.get(&survivor.id, &kept.id).is_ok());
    assert!(survivor_workspace.path().is_dir());
    assert_eq!(harness.embeddings().call_count(), 0);
}
