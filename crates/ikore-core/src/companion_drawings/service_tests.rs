use super::*;
use crate::persistence_service::{EnsureProjectForWorkspaceRequest, PersistenceService};
use rusqlite::params;
use std::fs;
use std::path::PathBuf;

struct Fixture {
    data: tempfile::TempDir,
    workspace: tempfile::TempDir,
    root: PathBuf,
    persistence: PersistenceService,
    service: CompanionDrawingsService,
    project: String,
}

impl Fixture {
    fn new() -> Self {
        let data = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let root = workspace.path().join("project");
        fs::create_dir(&root).unwrap();
        let persistence = PersistenceService::initialize(data.path()).unwrap();
        let project = persistence
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: root.to_string_lossy().into(),
                preferred_name: Some("Canvas fixture".into()),
            })
            .unwrap()
            .project
            .id;
        let service = CompanionDrawingsService::new(persistence.clone());
        Self {
            data,
            workspace,
            root,
            persistence,
            service,
            project,
        }
    }

    fn paths(&self, drawing: &CompanionDrawing) -> (PathBuf, PathBuf) {
        let directory = self.root.join(".ikore/drawings");
        (
            directory.join(format!("{}.ikore-canvas.json", drawing.basename)),
            directory.join(format!("{}.svg", drawing.basename)),
        )
    }

    fn scene(&self) -> CanvasSceneV1 {
        let mut element = CanvasElementV1::new(
            CanvasElementKindV1::Rectangle { round: true },
            10.0,
            20.0,
            120.0,
            80.0,
        );
        element.label = "Service & <database>\nAção".into();
        CanvasSceneV1 {
            elements: vec![element],
            ..Default::default()
        }
    }
}

#[test]
fn companion_drawings_crud_is_scoped_and_reopens_after_migration() {
    let fixture = Fixture::new();
    let drawing = fixture
        .service
        .create(&fixture.project, "Architecture")
        .unwrap();
    assert_eq!(drawing.sync_status, DrawingSyncStatus::Pending);
    assert_eq!(drawing.revision, 1);
    assert_eq!(fixture.service.list(&fixture.project).unwrap().len(), 1);
    let other = fixture
        .persistence
        .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
            workspace_path: fixture.workspace.path().to_string_lossy().into(),
            preferred_name: None,
        })
        .unwrap()
        .project
        .id;
    assert!(fixture.service.list(&other).unwrap().is_empty());
    assert!(fixture.service.get(&other, &drawing.id).is_err());
    assert!(
        fixture
            .service
            .save(
                &other,
                &drawing.id,
                drawing.revision,
                "Cross-project",
                &drawing.scene,
                &drawing.viewport
            )
            .is_err()
    );
    let reopened =
        CompanionDrawingsService::new(PersistenceService::initialize(fixture.data.path()).unwrap());
    assert_eq!(
        reopened.get(&fixture.project, &drawing.id).unwrap(),
        drawing
    );
    assert!(fixture.service.create("missing-project", "No").is_err());
}

#[test]
fn companion_drawings_revision_conflict_preserves_current_scene_and_basename() {
    let fixture = Fixture::new();
    let original = fixture.service.create(&fixture.project, "Before").unwrap();
    let saved = fixture
        .service
        .save(
            &fixture.project,
            &original.id,
            1,
            "After",
            &fixture.scene(),
            &original.viewport,
        )
        .unwrap();
    assert_eq!(saved.revision, 2);
    assert_eq!(original.basename, saved.basename);
    assert!(
        fixture
            .service
            .save(
                &fixture.project,
                &original.id,
                1,
                "Stale",
                &original.scene,
                &original.viewport
            )
            .is_err()
    );
    assert!(
        fixture
            .service
            .delete(&fixture.project, &original.id, 1)
            .is_err()
    );
    assert_eq!(
        fixture.service.get(&fixture.project, &original.id).unwrap(),
        saved
    );
    // Untouched pending revisions coalesce instead of growing with every autosave.
    let count: i64 = fixture
        .persistence
        .connection()
        .unwrap()
        .query_row(
            "SELECT count(*) FROM companion_drawing_sync_jobs",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn companion_drawings_viewport_does_not_change_content_or_exports() {
    let fixture = Fixture::new();
    let original = fixture.service.create(&fixture.project, "Camera").unwrap();
    fixture.service.sync_project(&fixture.project).unwrap();
    let (json, svg) = fixture.paths(&original);
    let original_json = fs::read(&json).unwrap();
    let original_svg = fs::read(&svg).unwrap();
    let viewport = CanvasViewportV1 {
        pan_x: 50.0,
        pan_y: -100.0,
        zoom: 2.5,
    };
    fixture
        .service
        .save_viewport(&fixture.project, &original.id, &viewport)
        .unwrap();
    let saved = fixture.service.get(&fixture.project, &original.id).unwrap();
    assert_eq!(saved.revision, 1);
    assert_eq!(saved.viewport, viewport);
    assert_eq!(saved.sync_status, DrawingSyncStatus::Synced);
    assert_eq!(
        fixture
            .service
            .sync_project(&fixture.project)
            .unwrap()
            .completed,
        0
    );
    assert_eq!(fs::read(json).unwrap(), original_json);
    assert_eq!(fs::read(svg).unwrap(), original_svg);
    assert!(
        fixture
            .service
            .save_viewport(
                &fixture.project,
                &original.id,
                &CanvasViewportV1 {
                    zoom: f32::NAN,
                    ..Default::default()
                }
            )
            .is_err()
    );
}

#[test]
fn companion_drawings_secret_redaction_precedes_database_jobs_and_files() {
    let fixture = Fixture::new();
    let token = format!("{}{}", "sk-proj-", "syntheticfixture0123456789");
    let original = fixture
        .service
        .create(&fixture.project, &format!("API {token}"))
        .unwrap();
    let mut scene = fixture.scene();
    scene.elements[0].label = format!("Token: {token}");
    let saved = fixture
        .service
        .save(
            &fixture.project,
            &original.id,
            1,
            &format!("API {token}"),
            &scene,
            &original.viewport,
        )
        .unwrap();
    assert!(!saved.title.contains(&token));
    assert!(!saved.basename.contains(&token));
    assert!(!saved.scene.elements[0].label.contains(&token));
    let db = fixture.persistence.connection().unwrap();
    let payload: String = db
        .query_row(
            "SELECT title || scene_json FROM companion_drawings",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let queued: String = db
        .query_row(
            "SELECT json_content || svg_content FROM companion_drawing_sync_jobs",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!payload.contains(&token));
    assert!(!queued.contains(&token));
    fixture.service.sync_project(&fixture.project).unwrap();
    let (json, svg) = fixture.paths(&saved);
    assert!(!fs::read_to_string(json).unwrap().contains(&token));
    let svg = fs::read_to_string(svg).unwrap();
    assert!(!svg.contains(&token));
    assert!(svg.contains("REDACTED_SECRET"));
}

#[test]
fn companion_drawings_partial_pair_replays_before_new_revision() {
    let fixture = Fixture::new();
    let original = fixture.service.create(&fixture.project, "Partial").unwrap();
    let (json, svg) = fixture.paths(&original);
    fs::create_dir_all(json.parent().unwrap()).unwrap();
    let db = fixture.persistence.connection().unwrap();
    let pending_json: String = db
        .query_row(
            "SELECT json_content FROM companion_drawing_sync_jobs",
            [],
            |r| r.get(0),
        )
        .unwrap();
    fs::write(&json, pending_json).unwrap();
    db.execute("UPDATE companion_drawing_sync_jobs SET started=1", [])
        .unwrap();
    let saved = fixture
        .service
        .save(
            &fixture.project,
            &original.id,
            1,
            "Newer",
            &fixture.scene(),
            &original.viewport,
        )
        .unwrap();
    let count: i64 = db
        .query_row(
            "SELECT count(*) FROM companion_drawing_sync_jobs",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 2);
    let report = fixture.service.sync_project(&fixture.project).unwrap();
    assert_eq!(report.completed, 2);
    assert_eq!(report.pending, 0);
    assert!(svg.is_file());
    assert!(fs::read_to_string(json).unwrap().contains("Newer"));
    assert_eq!(
        fixture
            .service
            .get(&fixture.project, &saved.id)
            .unwrap()
            .sync_status,
        DrawingSyncStatus::Synced
    );
    assert_eq!(
        fixture
            .service
            .sync_project(&fixture.project)
            .unwrap()
            .completed,
        0
    );
}

#[test]
fn companion_drawings_svg_failure_keeps_json_and_recovers_on_retry() {
    let fixture = Fixture::new();
    let original = fixture.service.create(&fixture.project, "Retry").unwrap();
    let (json, svg) = fixture.paths(&original);
    fs::create_dir_all(&svg).unwrap();
    let report = fixture.service.sync_project(&fixture.project).unwrap();
    assert_eq!(report.failed, 1);
    assert!(json.is_file());
    fs::remove_dir(&svg).unwrap();
    let report = fixture.service.sync_project(&fixture.project).unwrap();
    assert_eq!(report.completed, 1);
    assert_eq!(report.pending, 0);
    assert!(svg.is_file());
}

#[test]
fn companion_drawings_external_changes_are_not_overwritten_or_deleted() {
    let fixture = Fixture::new();
    let original = fixture
        .service
        .create(&fixture.project, "Conflict")
        .unwrap();
    fixture.service.sync_project(&fixture.project).unwrap();
    let (json, _) = fixture.paths(&original);
    fs::write(&json, "External user content").unwrap();
    let saved = fixture
        .service
        .save(
            &fixture.project,
            &original.id,
            1,
            "Updated",
            &fixture.scene(),
            &original.viewport,
        )
        .unwrap();
    let report = fixture.service.sync_project(&fixture.project).unwrap();
    assert_eq!(report.conflicts, 1);
    assert_eq!(fs::read_to_string(&json).unwrap(), "External user content");
    assert_eq!(
        fixture
            .service
            .get(&fixture.project, &saved.id)
            .unwrap()
            .sync_status,
        DrawingSyncStatus::Conflict
    );
    fixture
        .service
        .delete(&fixture.project, &saved.id, saved.revision)
        .unwrap();
    assert_eq!(
        fixture
            .service
            .sync_project(&fixture.project)
            .unwrap()
            .conflicts,
        1
    );
    assert_eq!(fs::read_to_string(json).unwrap(), "External user content");
}

#[test]
fn companion_drawings_missing_workspace_is_not_recreated_and_can_retry() {
    let fixture = Fixture::new();
    let original = fixture.service.create(&fixture.project, "Offline").unwrap();
    let moved = fixture.workspace.path().join("temporarily-moved");
    fs::rename(&fixture.root, &moved).unwrap();
    let report = fixture.service.sync_project(&fixture.project).unwrap();
    assert_eq!(report.failed, 1);
    assert!(!fixture.root.exists());
    assert!(fixture.service.get(&fixture.project, &original.id).is_ok());
    fs::rename(moved, &fixture.root).unwrap();
    assert_eq!(
        fixture
            .service
            .sync_project(&fixture.project)
            .unwrap()
            .pending,
        0
    );
}

#[test]
fn companion_drawings_duplicate_remaps_bindings_and_containers() {
    let fixture = Fixture::new();
    let original = fixture
        .service
        .create(&fixture.project, "Original")
        .unwrap();
    let mut boundary = CanvasElementV1::new(
        CanvasElementKindV1::Rectangle { round: false },
        0.0,
        0.0,
        400.0,
        300.0,
    );
    boundary.stroke_style = CanvasStrokeStyle::Dashed;
    let target = CanvasElementV1::new(CanvasElementKindV1::Ellipse, 50.0, 80.0, 100.0, 80.0);
    let arrow = CanvasElementV1::new(
        CanvasElementKindV1::Arrow {
            points: [[0.0, 0.0], [120.0, 0.0]],
            bidirectional: false,
            start_binding: None,
            end_binding: Some(ArrowBindingV1 {
                element_id: target.id.clone(),
                anchor: [-1.0, 0.0],
                gap: 0.0,
            }),
        },
        -70.0,
        120.0,
        120.0,
        0.0,
    );
    let scene = CanvasSceneV1 {
        elements: vec![boundary, target, arrow],
        ..Default::default()
    };
    let saved = fixture
        .service
        .save(
            &fixture.project,
            &original.id,
            1,
            "Original",
            &scene,
            &original.viewport,
        )
        .unwrap();
    let duplicate = fixture
        .service
        .duplicate(&fixture.project, &saved.id)
        .unwrap();
    assert_ne!(duplicate.id, saved.id);
    let old_ids: std::collections::HashSet<_> =
        saved.scene.elements.iter().map(|e| e.id.as_str()).collect();
    let new_ids: std::collections::HashSet<_> = duplicate
        .scene
        .elements
        .iter()
        .map(|e| e.id.as_str())
        .collect();
    for element in &duplicate.scene.elements {
        assert!(!old_ids.contains(element.id.as_str()));
        if let Some(parent) = &element.container_id {
            assert!(new_ids.contains(parent.as_str()));
        }
        if let CanvasElementKindV1::Arrow {
            end_binding: Some(binding),
            ..
        } = &element.shape
        {
            assert!(new_ids.contains(binding.element_id.as_str()));
            assert!(!old_ids.contains(binding.element_id.as_str()));
        }
    }
}

#[test]
fn companion_drawings_delete_cleans_only_owned_outputs_and_project_cascades() {
    let fixture = Fixture::new();
    let original = fixture.service.create(&fixture.project, "Delete").unwrap();
    fixture.service.sync_project(&fixture.project).unwrap();
    let (json, svg) = fixture.paths(&original);
    fixture
        .service
        .delete(&fixture.project, &original.id, 1)
        .unwrap();
    assert!(fixture.service.get(&fixture.project, &original.id).is_err());
    assert_eq!(
        fixture
            .service
            .sync_project(&fixture.project)
            .unwrap()
            .pending,
        0
    );
    assert!(!json.exists());
    assert!(!svg.exists());
    let _pending = fixture.service.create(&fixture.project, "Cascade").unwrap();
    let db = fixture.persistence.connection().unwrap();
    // A project cascade also removes drawing deletion intents, which have their own project FK.
    db.execute("DELETE FROM projects WHERE id=?1", [&fixture.project])
        .unwrap();
    let remaining: i64 = db
        .query_row(
            "SELECT count(*) FROM companion_drawing_sync_jobs",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(remaining, 0);
    let remaining_drawings: i64 = db
        .query_row("SELECT count(*) FROM companion_drawings", [], |r| r.get(0))
        .unwrap();
    assert_eq!(remaining_drawings, 0);
}

#[test]
fn companion_drawings_rejects_stored_unknown_schema_and_dangling_bindings() {
    let fixture = Fixture::new();
    let drawing = fixture
        .service
        .create(&fixture.project, "Invalid stored scene")
        .unwrap();
    let db = fixture.persistence.connection().unwrap();
    let scene = CanvasSceneV1 {
        schema_version: 2,
        ..Default::default()
    };
    db.execute(
        "UPDATE companion_drawings SET scene_json=?1 WHERE id=?2",
        params![serde_json::to_string(&scene).unwrap(), drawing.id],
    )
    .unwrap();
    assert!(fixture.service.get(&fixture.project, &drawing.id).is_err());
    assert!(
        fixture
            .service
            .duplicate(&fixture.project, &drawing.id)
            .is_err()
    );
    let arrow = CanvasElementV1::new(
        CanvasElementKindV1::Arrow {
            points: [[0.0, 0.0], [120.0, 0.0]],
            bidirectional: false,
            start_binding: None,
            end_binding: Some(ArrowBindingV1 {
                element_id: "missing".into(),
                anchor: [-1.0, 0.0],
                gap: 0.0,
            }),
        },
        0.0,
        0.0,
        120.0,
        0.0,
    );
    let scene = CanvasSceneV1 {
        elements: vec![arrow],
        ..Default::default()
    };
    db.execute(
        "UPDATE companion_drawings SET scene_json=?1 WHERE id=?2",
        params![serde_json::to_string(&scene).unwrap(), drawing.id],
    )
    .unwrap();
    assert!(fixture.service.get(&fixture.project, &drawing.id).is_err());
    assert!(
        fixture
            .service
            .duplicate(&fixture.project, &drawing.id)
            .is_err()
    );
}

#[test]
fn companion_drawings_stored_viewport_is_validated_and_sidebar_avoids_scene_decode() {
    let fixture = Fixture::new();
    let drawing = fixture
        .service
        .create(&fixture.project, "Metadata")
        .unwrap();
    assert_eq!(
        fixture.service.list_summaries(&fixture.project).unwrap(),
        vec![drawing.summary()]
    );
    let db = fixture.persistence.connection().unwrap();
    let invalid_viewport = CanvasViewportV1 {
        zoom: 0.0,
        ..Default::default()
    };
    db.execute(
        "UPDATE companion_drawings SET viewport_json=?1 WHERE id=?2",
        params![
            serde_json::to_string(&invalid_viewport).unwrap(),
            drawing.id
        ],
    )
    .unwrap();
    assert!(fixture.service.get(&fixture.project, &drawing.id).is_err());
    // Metadata listing remains available to identify a corrupt document.
    assert_eq!(
        fixture
            .service
            .list_summaries(&fixture.project)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn companion_drawings_archived_project_refuses_read_write_and_sync() {
    let fixture = Fixture::new();
    let drawing = fixture
        .service
        .create(&fixture.project, "Archived")
        .unwrap();
    fixture
        .persistence
        .connection()
        .unwrap()
        .execute(
            "UPDATE projects SET is_archived=1 WHERE id=?1",
            [&fixture.project],
        )
        .unwrap();
    assert!(fixture.service.list(&fixture.project).is_err());
    assert!(fixture.service.get(&fixture.project, &drawing.id).is_err());
    assert!(
        fixture
            .service
            .save(
                &fixture.project,
                &drawing.id,
                1,
                "No",
                &drawing.scene,
                &drawing.viewport
            )
            .is_err()
    );
    assert!(fixture.service.sync_project(&fixture.project).is_err());
}

#[test]
fn companion_drawings_database_basename_cannot_escape_managed_directory() {
    let fixture = Fixture::new();
    let drawing = fixture
        .service
        .create(&fixture.project, "Traversal")
        .unwrap();
    fixture
        .persistence
        .connection()
        .unwrap()
        .execute(
            "UPDATE companion_drawing_sync_jobs SET basename=?1 WHERE drawing_id=?2",
            params!["../../outside", drawing.id],
        )
        .unwrap();
    assert_eq!(
        fixture
            .service
            .sync_project(&fixture.project)
            .unwrap()
            .failed,
        1
    );
    assert!(!fixture.workspace.path().join("outside.svg").exists());
}

#[cfg(unix)]
#[test]
fn companion_drawings_linked_managed_directory_is_rejected() {
    let fixture = Fixture::new();
    let external = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(external.path(), fixture.root.join(".ikore")).unwrap();
    fixture.service.create(&fixture.project, "Linked").unwrap();
    assert_eq!(
        fixture
            .service
            .sync_project(&fixture.project)
            .unwrap()
            .failed,
        1
    );
    assert_eq!(fs::read_dir(external.path()).unwrap().count(), 0);
}

#[test]
fn legacy_hvc_drawings_are_migrated_to_ikore_drawings_automatically() {
    let fixture = Fixture::new();
    let legacy_dir = fixture.root.join(".hvc").join("drawings");
    fs::create_dir_all(&legacy_dir).unwrap();
    let old_json =
        legacy_dir.join("architecture-00000000-0000-4000-8000-000000000001.hvc-canvas.json");
    let old_svg = legacy_dir.join("architecture-00000000-0000-4000-8000-000000000001.svg");
    fs::write(&old_json, b"{\"test\": 1}").unwrap();
    fs::write(&old_svg, b"<svg></svg>").unwrap();

    super::workspace_sync::migrate_project_drawings(&fixture.root);

    let target_dir = fixture.root.join(".ikore").join("drawings");
    let target_json =
        target_dir.join("architecture-00000000-0000-4000-8000-000000000001.ikore-canvas.json");
    let target_svg = target_dir.join("architecture-00000000-0000-4000-8000-000000000001.svg");

    assert!(target_json.exists());
    assert!(target_svg.exists());
    assert_eq!(fs::read(&target_json).unwrap(), b"{\"test\": 1}");
    assert!(!old_json.exists());
    assert!(!legacy_dir.exists());
}

#[test]
fn migration_does_not_overwrite_existing_ikore_drawings() {
    let fixture = Fixture::new();
    let legacy_dir = fixture.root.join(".hvc").join("drawings");
    let target_dir = fixture.root.join(".ikore").join("drawings");
    fs::create_dir_all(&legacy_dir).unwrap();
    fs::create_dir_all(&target_dir).unwrap();

    let old_json = legacy_dir.join("doc-00000000-0000-4000-8000-000000000001.hvc-canvas.json");
    let target_json = target_dir.join("doc-00000000-0000-4000-8000-000000000001.ikore-canvas.json");
    fs::write(&old_json, b"legacy content").unwrap();
    fs::write(&target_json, b"new content").unwrap();

    super::workspace_sync::migrate_project_drawings(&fixture.root);

    // Existing target must be preserved without overwrite
    assert_eq!(fs::read(&target_json).unwrap(), b"new content");
}
