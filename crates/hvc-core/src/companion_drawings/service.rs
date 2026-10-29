//! Project-scoped documents and durable filesystem intents. All public methods are blocking.
use super::{
    CanvasElementKindV1, CanvasSceneV1, CanvasViewportV1, export_svg, normalize_scene,
    sanitize_title, scene_json, validate_viewport,
};
use crate::feature_forge::service::{check_project, invalid, now};
use crate::persistence_service::{PersistenceService, PersistenceServiceError as Error};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrawingsEvent {
    pub project_id: String,
    pub drawing_id: String,
    pub revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrawingSyncStatus {
    Pending,
    Synced,
    Conflict,
    Failed,
}

impl DrawingSyncStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Synced => "synced",
            Self::Conflict => "conflict",
            Self::Failed => "failed",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "Saved in app · files pending",
            Self::Synced => "Saved",
            Self::Conflict => "File conflict",
            Self::Failed => "Saved in app · files pending",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompanionDrawing {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub scene: CanvasSceneV1,
    pub viewport: CanvasViewportV1,
    pub revision: u64,
    pub basename: String,
    pub created_at: String,
    pub updated_at: String,
    pub sync_status: DrawingSyncStatus,
}

/// Sidebar metadata. Loading the library never needs to clone every drawing scene.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompanionDrawingSummary {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub revision: u64,
    pub basename: String,
    pub created_at: String,
    pub updated_at: String,
    pub sync_status: DrawingSyncStatus,
}

impl CompanionDrawing {
    pub fn summary(&self) -> CompanionDrawingSummary {
        CompanionDrawingSummary {
            id: self.id.clone(),
            project_id: self.project_id.clone(),
            title: self.title.clone(),
            revision: self.revision,
            basename: self.basename.clone(),
            created_at: self.created_at.clone(),
            updated_at: self.updated_at.clone(),
            sync_status: self.sync_status,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub completed: usize,
    pub pending: usize,
    pub conflicts: usize,
    pub failed: usize,
}

#[derive(Clone)]
pub struct CompanionDrawingsService {
    pub(crate) persistence: PersistenceService,
    events: Option<Arc<crate::AppEventBus>>,
}

type ProjectLocks = Mutex<HashMap<String, Arc<Mutex<()>>>>;
static PROJECT_LOCKS: OnceLock<ProjectLocks> = OnceLock::new();

impl CompanionDrawingsService {
    pub fn new(persistence: PersistenceService) -> Self {
        Self {
            persistence,
            events: None,
        }
    }

    pub fn with_events(mut self, events: Arc<crate::AppEventBus>) -> Self {
        self.events = Some(events);
        self
    }

    pub(crate) fn changed(&self, project: &str, id: &str, revision: u64) {
        if let Some(events) = &self.events {
            events.publish(crate::AppEvent::Drawings(DrawingsEvent {
                project_id: project.into(),
                drawing_id: id.into(),
                revision,
            }));
        }
    }

    /// Serializes canvas commits, mirror writes and project deletion in this process.
    /// The closure must not call another public drawing method for the same project.
    pub fn with_project_guard<T>(
        &self,
        project: &str,
        operation: impl FnOnce() -> Result<T, Error>,
    ) -> Result<T, Error> {
        let key = format!("{}\0{project}", self.persistence.database_path().display());
        let lock = PROJECT_LOCKS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .map_err(|_| invalid("Drawing synchronization is unavailable"))?
            .entry(key)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let _guard = lock
            .lock()
            .map_err(|_| invalid("Drawing synchronization is unavailable"))?;
        operation()
    }

    pub fn list(&self, project: &str) -> Result<Vec<CompanionDrawing>, Error> {
        self.with_project_guard(project, || {
            let db = self.persistence.connection()?;
            check_project(&db, project)?;
            let ids = db
                .prepare("SELECT id FROM companion_drawings WHERE project_id=?1 ORDER BY updated_at DESC,id")?
                .query_map([project], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            ids.iter().map(|id| drawing(&db, project, id)).collect()
        })
    }

    pub fn list_summaries(&self, project: &str) -> Result<Vec<CompanionDrawingSummary>, Error> {
        self.with_project_guard(project, || {
            let db = self.persistence.connection()?;
            check_project(&db, project)?;
            let rows = db.prepare(
                "SELECT id,project_id,title,revision,basename,created_at,updated_at,
                 CASE WHEN EXISTS(SELECT 1 FROM companion_drawing_sync_jobs j WHERE j.project_id=d.project_id AND j.drawing_id=d.id AND j.status='conflict') THEN 'conflict'
                  WHEN EXISTS(SELECT 1 FROM companion_drawing_sync_jobs j WHERE j.project_id=d.project_id AND j.drawing_id=d.id AND j.status='failed') THEN 'failed'
                  WHEN mirrored_revision=revision THEN 'synced' ELSE 'pending' END
                 FROM companion_drawings d WHERE project_id=?1 ORDER BY updated_at DESC,id",
            )?.query_map([project], |r| Ok(CompanionDrawingSummary {
                id:r.get(0)?,project_id:r.get(1)?,title:r.get(2)?,revision:r.get(3)?,basename:r.get(4)?,
                created_at:r.get(5)?,updated_at:r.get(6)?,sync_status:sync_status(&r.get::<_,String>(7)?),
            }))?.collect::<Result<Vec<_>,_>>()?;
            rows.into_iter().map(|mut row| {row.title = sanitize_title(&row.title)?;Ok(row)}).collect()
        })
    }

    pub fn get(&self, project: &str, id: &str) -> Result<CompanionDrawing, Error> {
        self.with_project_guard(project, || {
            let db = self.persistence.connection()?;
            check_project(&db, project)?;
            drawing(&db, project, id)
        })
    }

    pub fn create(&self, project: &str, title: &str) -> Result<CompanionDrawing, Error> {
        self.with_project_guard(project, || {
            let mut db = self.persistence.connection()?;
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            check_project(&tx, project)?;
            let result = insert_drawing(
                &tx,
                project,
                title,
                &CanvasSceneV1::default(),
                &CanvasViewportV1::default(),
            )?;
            tx.commit()?;
            self.changed(project, &result.id, result.revision);
            Ok(result)
        })
    }

    pub fn save(
        &self,
        project: &str,
        id: &str,
        expected_revision: u64,
        title: &str,
        scene: &CanvasSceneV1,
        viewport: &CanvasViewportV1,
    ) -> Result<CompanionDrawing, Error> {
        let title = sanitize_title(title)?;
        let scene = normalize_scene(scene)?;
        validate_viewport(viewport)?;
        let scene_text = scene_json(&scene)?;
        let viewport_text = json(viewport)?;
        self.with_project_guard(project, || {
            let mut db = self.persistence.connection()?;
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            check_project(&tx, project)?;
            let current = drawing(&tx, project, id)?;
            if current.revision != expected_revision {
                return Err(invalid("The drawing changed; reload it before saving"));
            }
            let revision = current.revision.checked_add(1).filter(|n| *n <= i64::MAX as u64)
                .ok_or_else(|| invalid("The drawing revision limit was reached"))?;
            tx.execute(
                "UPDATE companion_drawings SET title=?1,scene_json=?2,viewport_json=?3,revision=?4,updated_at=?5
                 WHERE id=?6 AND project_id=?7 AND revision=?8",
                params![title, scene_text, viewport_text, revision, now(), id, project, expected_revision],
            )?;
            queue_write(&tx, project, id)?;
            let result = drawing(&tx, project, id)?;
            tx.commit()?;
            self.changed(project, &result.id, result.revision);
            Ok(result)
        })
    }

    /// Camera preferences never change the content revision or regenerate exports.
    pub fn save_viewport(
        &self,
        project: &str,
        id: &str,
        viewport: &CanvasViewportV1,
    ) -> Result<(), Error> {
        validate_viewport(viewport)?;
        self.with_project_guard(project, || {
            let db = self.persistence.connection()?;
            check_project(&db, project)?;
            if db.execute(
                "UPDATE companion_drawings SET viewport_json=?1 WHERE id=?2 AND project_id=?3",
                params![json(viewport)?, id, project],
            )? == 0
            {
                return Err(Error::NotFound("Drawing".into()));
            }
            Ok(())
        })
    }

    pub fn duplicate(&self, project: &str, id: &str) -> Result<CompanionDrawing, Error> {
        self.with_project_guard(project, || {
            let mut db = self.persistence.connection()?;
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            check_project(&tx, project)?;
            let original = drawing(&tx, project, id)?;
            let mut scene = original.scene;
            let ids: HashMap<String, String> = scene
                .elements
                .iter()
                .map(|element| (element.id.clone(), Uuid::new_v4().to_string()))
                .collect();
            for element in &mut scene.elements {
                element.id = ids[&element.id].clone();
                element.container_id = element
                    .container_id
                    .as_ref()
                    .and_then(|id| ids.get(id))
                    .cloned();
                if let CanvasElementKindV1::Arrow {
                    start_binding,
                    end_binding,
                    ..
                } = &mut element.shape
                {
                    for binding in [start_binding, end_binding].into_iter().flatten() {
                        binding.element_id = ids[&binding.element_id].clone();
                    }
                }
            }
            if let Some(document) = &mut scene.document {
                for element in &mut document.elements {
                    element.element_id = ids
                        .get(&element.element_id)
                        .cloned()
                        .ok_or_else(|| invalid("Semantic drawing element does not exist"))?;
                    for id in [
                        &mut element.participant_id,
                        &mut element.lane_id,
                        &mut element.source_id,
                        &mut element.target_id,
                    ]
                    .into_iter()
                    .flatten()
                    {
                        *id = ids
                            .get(id)
                            .cloned()
                            .ok_or_else(|| invalid("Semantic drawing reference does not exist"))?;
                    }
                }
            }
            let title = format!(
                "{} copy",
                original.title.chars().take(115).collect::<String>()
            );
            let result = insert_drawing(&tx, project, &title, &scene, &original.viewport)?;
            tx.commit()?;
            self.changed(project, &result.id, result.revision);
            Ok(result)
        })
    }

    pub fn delete(&self, project: &str, id: &str, expected_revision: u64) -> Result<(), Error> {
        self.with_project_guard(project, || {
            let mut db = self.persistence.connection()?;
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            check_project(&tx, project)?;
            let current = drawing(&tx, project, id)?;
            if current.revision != expected_revision {
                return Err(invalid("The drawing changed; reload it before deleting"));
            }
            discard_unstarted_writes(&tx, project, id)?;
            let (expected_json, expected_svg) = expected_hashes(&tx, project, id)?;
            tx.execute(
                "INSERT INTO companion_drawing_sync_jobs
                 (project_id,drawing_id,revision,operation,basename,expected_json_sha256,expected_svg_sha256)
                 VALUES(?1,?2,?3,'delete',?4,?5,?6)",
                params![project, id, current.revision, current.basename, expected_json, expected_svg],
            )?;
            tx.execute("DELETE FROM companion_drawings WHERE id=?1 AND project_id=?2 AND revision=?3",
                params![id, project, expected_revision])?;
            tx.commit()?;
            self.changed(project, id, expected_revision);
            Ok(())
        })
    }

    /// Performs one bounded pass; failed jobs remain durable for an explicit retry.
    pub fn sync_project(&self, project: &str) -> Result<SyncReport, Error> {
        self.with_project_guard(project, || {
            super::workspace_sync::sync_project(&self.persistence, project)
        })
    }
}

#[derive(Serialize)]
struct CanvasFileV1<'a> {
    schema_version: u32,
    drawing_id: &'a str,
    title: &'a str,
    scene: &'a CanvasSceneV1,
}

fn json(value: &impl Serialize) -> Result<String, Error> {
    serde_json::to_string(value).map_err(|_| invalid("Drawing serialization failed"))
}

pub(crate) fn insert_drawing(
    db: &Connection,
    project: &str,
    title: &str,
    scene: &CanvasSceneV1,
    viewport: &CanvasViewportV1,
) -> Result<CompanionDrawing, Error> {
    let title = sanitize_title(title)?;
    let scene = normalize_scene(scene)?;
    validate_viewport(viewport)?;
    let id = Uuid::new_v4().to_string();
    let slug: String = title
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .take(40)
        .collect();
    let slug = slug.trim_matches('-');
    let basename = format!("{}-{id}", if slug.is_empty() { "drawing" } else { slug });
    let timestamp = now();
    db.execute(
        "INSERT INTO companion_drawings(id,project_id,title,scene_json,viewport_json,revision,basename,created_at,updated_at)
         VALUES(?1,?2,?3,?4,?5,1,?6,?7,?7)",
        params![id, project, title, scene_json(&scene)?, json(viewport)?, basename, timestamp],
    )?;
    queue_write(db, project, &id)?;
    drawing(db, project, &id)
}

fn queue_write(db: &Connection, project: &str, id: &str) -> Result<(), Error> {
    let current = drawing(db, project, id)?;
    // Keep an in-flight/partially written revision, but replace untouched pending snapshots.
    discard_unstarted_writes(db, project, id)?;
    let json_content = serde_json::to_string_pretty(&CanvasFileV1 {
        schema_version: 1,
        drawing_id: &current.id,
        title: &current.title,
        scene: &current.scene,
    })
    .map_err(|_| invalid("Drawing serialization failed"))?
        + "\n";
    let svg_content = export_svg(&current.scene)?;
    let (expected_json, expected_svg) = expected_hashes(db, project, id)?;
    db.execute(
        "INSERT INTO companion_drawing_sync_jobs
         (project_id,drawing_id,revision,operation,basename,json_content,svg_content,
          expected_json_sha256,expected_svg_sha256,desired_json_sha256,desired_svg_sha256)
         VALUES(?1,?2,?3,'write',?4,?5,?6,?7,?8,?9,?10)",
        params![
            project,
            id,
            current.revision,
            current.basename,
            json_content,
            svg_content,
            expected_json,
            expected_svg,
            super::workspace_sync::hash(json_content.as_bytes()),
            super::workspace_sync::hash(svg_content.as_bytes())
        ],
    )?;
    Ok(())
}

fn discard_unstarted_writes(db: &Connection, project: &str, id: &str) -> Result<(), Error> {
    db.execute("DELETE FROM companion_drawing_sync_jobs WHERE project_id=?1 AND drawing_id=?2 AND operation='write' AND started=0",
        params![project,id])?;
    Ok(())
}

fn expected_hashes(
    db: &Connection,
    project: &str,
    id: &str,
) -> Result<(Option<String>, Option<String>), Error> {
    let queued = db
        .query_row(
            "SELECT desired_json_sha256,desired_svg_sha256 FROM companion_drawing_sync_jobs
         WHERE project_id=?1 AND drawing_id=?2 ORDER BY id DESC LIMIT 1",
            params![project, id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some(hashes) = queued {
        return Ok(hashes);
    }
    Ok(db.query_row(
        "SELECT json_sha256,svg_sha256 FROM companion_drawings WHERE project_id=?1 AND id=?2",
        params![project, id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?)
}

fn drawing(db: &Connection, project: &str, id: &str) -> Result<CompanionDrawing, Error> {
    let row = db.query_row(
        "SELECT id,project_id,title,scene_json,viewport_json,revision,basename,created_at,updated_at,
         CASE WHEN EXISTS(SELECT 1 FROM companion_drawing_sync_jobs j WHERE j.project_id=d.project_id AND j.drawing_id=d.id AND j.status='conflict') THEN 'conflict'
          WHEN EXISTS(SELECT 1 FROM companion_drawing_sync_jobs j WHERE j.project_id=d.project_id AND j.drawing_id=d.id AND j.status='failed') THEN 'failed'
          WHEN mirrored_revision=revision THEN 'synced' ELSE 'pending' END
         FROM companion_drawings d WHERE project_id=?1 AND id=?2",
        params![project,id], |r| Ok((
            r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,
            r.get::<_,u64>(5)?,r.get::<_,String>(6)?,r.get::<_,String>(7)?,r.get::<_,String>(8)?,r.get::<_,String>(9)?,
        )),
    ).optional()?.ok_or_else(|| Error::NotFound("Drawing".into()))?;
    let scene: CanvasSceneV1 =
        serde_json::from_str(&row.3).map_err(|_| invalid("Stored drawing data is invalid"))?;
    let scene = normalize_scene(&scene)?;
    let viewport: CanvasViewportV1 =
        serde_json::from_str(&row.4).map_err(|_| invalid("Stored drawing viewport is invalid"))?;
    validate_viewport(&viewport)?;
    Ok(CompanionDrawing {
        id: row.0,
        project_id: row.1,
        title: sanitize_title(&row.2)?,
        scene,
        viewport,
        revision: row.5,
        basename: row.6,
        created_at: row.7,
        updated_at: row.8,
        sync_status: sync_status(&row.9),
    })
}

fn sync_status(value: &str) -> DrawingSyncStatus {
    match value {
        "synced" => DrawingSyncStatus::Synced,
        "conflict" => DrawingSyncStatus::Conflict,
        "failed" => DrawingSyncStatus::Failed,
        _ => DrawingSyncStatus::Pending,
    }
}
