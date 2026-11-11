//! Explicit, human-initiated permanent project deletion.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::Serialize;
use uuid::Uuid;
use walkdir::WalkDir;

use crate::persistence_service::{PersistenceService, PersistenceServiceError as Error, Project};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDeletionPreview {
    pub project: Project,
    pub pulsations: i64,
    pub conversations: i64,
    pub files: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDeletionOutcome {
    /// The project row and original repository path have both been removed.
    pub deleted: bool,
    /// A quarantined directory or historical backup needs another cleanup pass.
    pub cleanup_pending: bool,
}

#[derive(Debug)]
struct Journal {
    project_id: String,
    root: PathBuf,
    quarantine: PathBuf,
    phase: String,
}

impl PersistenceService {
    pub fn pending_project_deletion_count(&self) -> Result<i64, Error> {
        Ok(self.connection()?.query_row(
            "SELECT count(*) FROM project_deletion_journal",
            [],
            |r| r.get(0),
        )?)
    }

    pub fn preview_project_deletion(&self, id: &str) -> Result<ProjectDeletionPreview, Error> {
        let db = self.connection()?;
        let project = project(&db, id)?;
        let root = checked_root(&db, &project, self.database_path())?;
        let mut files = 0_u64;
        let mut bytes = 0_u64;
        for entry in WalkDir::new(&root).follow_links(false) {
            let entry = entry.map_err(|e| Error::Validation(e.to_string()))?;
            reject_reparse(entry.path())?;
            if entry.file_type().is_file() {
                files = files.saturating_add(1);
                bytes = bytes.saturating_add(
                    entry
                        .metadata()
                        .map_err(|e| Error::Validation(e.to_string()))?
                        .len(),
                );
            }
        }
        let pulsations = db.query_row(
            "SELECT count(*) FROM pulsations WHERE project_id=?1",
            [id],
            |r| r.get(0),
        )?;
        let conversations = db.query_row(
            "SELECT count(*) FROM chat_conversations WHERE project_id=?1",
            [id],
            |r| r.get(0),
        )?;
        Ok(ProjectDeletionPreview {
            project,
            pulsations,
            conversations,
            files,
            bytes,
        })
    }

    /// The caller must close/detach any active workspace watcher and reject dirty editor tabs.
    /// No user data is touched when confirmation, path safety or active-run checks fail.
    pub(crate) fn validate_project_deletion_request(
        &self,
        id: &str,
        confirmation: &str,
    ) -> Result<ProjectDeletionPreview, Error> {
        let preview = self.preview_project_deletion(id)?;
        if confirmation != preview.project.name {
            return Err(Error::Validation(
                "Type the exact project name to confirm permanent deletion.".into(),
            ));
        }
        check_deletion_idle(&self.connection()?, id)?;
        Ok(preview)
    }

    pub fn delete_project_permanently(
        &self,
        id: &str,
        confirmation: &str,
    ) -> Result<ProjectDeletionOutcome, Error> {
        let preview = self.validate_project_deletion_request(id, confirmation)?;
        let root = fs::canonicalize(&preview.project.workspace_path)?;
        let parent = root
            .parent()
            .ok_or_else(|| Error::Validation("Cannot delete a filesystem root.".into()))?;
        let quarantine = parent.join(format!(".hvc-delete-{}", Uuid::new_v4()));
        let mut db = self.connection()?;
        {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let latest = project(&tx, id)?;
            if latest.workspace_path != preview.project.workspace_path
                || latest.name != preview.project.name
            {
                return Err(Error::Validation(
                    "The project changed while deletion was prepared. Reload and try again.".into(),
                ));
            }
            check_deletion_idle(&tx, id)?;
            tx.execute("INSERT INTO project_deletion_journal(project_id,root_path,quarantine_path,was_archived,phase,started_at) VALUES(?1,?2,?3,?4,'prepared',strftime('%Y-%m-%dT%H:%M:%fZ','now'))",params![id,root.to_string_lossy(),quarantine.to_string_lossy(),i64::from(latest.is_archived)])?;
            tx.execute("UPDATE projects SET is_archived=1 WHERE id=?1", [id])?;
            tx.commit()?;
        }
        if let Err(error) = fs::rename(&root, &quarantine) {
            rollback_prepared(&db, id, &root, &quarantine)?;
            return Err(Error::Io(error));
        }
        match purge_project(&mut db, id, &preview.project, &root) {
            Ok(()) => {}
            Err(error) => {
                rollback_prepared(&db, id, &root, &quarantine)?;
                return Err(error);
            }
        }
        let cleanup_pending = finish_purged(
            &db,
            self.database_path()
                .parent()
                .ok_or_else(|| Error::Validation("Database has no parent directory.".into()))?,
            id,
        )
        .is_err();
        Ok(ProjectDeletionOutcome {
            deleted: true,
            cleanup_pending,
        })
    }
}

/// Called under the app data-directory lock before background workers start.
pub(crate) fn recover_project_deletions(db: &Connection, data_dir: &Path) -> Result<(), Error> {
    let mut statement = db.prepare(
        "SELECT project_id,root_path,quarantine_path,phase FROM project_deletion_journal",
    )?;
    let journals = statement
        .query_map([], |r| {
            Ok(Journal {
                project_id: r.get(0)?,
                root: PathBuf::from(r.get::<_, String>(1)?),
                quarantine: PathBuf::from(r.get::<_, String>(2)?),
                phase: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    for journal in journals {
        if journal.phase == "prepared" {
            rollback_prepared(db, &journal.project_id, &journal.root, &journal.quarantine)?;
        } else if let Err(error) = finish_purged(db, data_dir, &journal.project_id) {
            // The durable journal remains; another startup can retry cleanup.
            eprintln!("project deletion cleanup pending: {error}");
        }
    }
    Ok(())
}

fn project(db: &Connection, id: &str) -> Result<Project, Error> {
    db.query_row("SELECT id,name,workspace_path,is_archived,created_at,updated_at,last_opened_at FROM projects WHERE id=?1",[id],|r|Ok(Project {
        id:r.get(0)?,name:r.get(1)?,workspace_path:r.get(2)?,is_archived:r.get::<_,i64>(3)? != 0,
        created_at:r.get(4)?,updated_at:r.get(5)?,last_opened_at:r.get(6)?,
    })).optional()?.ok_or_else(||Error::NotFound("Project not found.".into()))
}

fn reject_reparse(path: &Path) -> Result<(), Error> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        return Err(Error::Validation(format!(
            "Deletion refuses a symbolic link: {}",
            path.display()
        )));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return Err(Error::Validation(format!(
                "Deletion refuses a reparse point: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

fn checked_root(
    db: &Connection,
    project: &Project,
    database_path: &Path,
) -> Result<PathBuf, Error> {
    let raw = Path::new(&project.workspace_path);
    if !raw.is_absolute() {
        return Err(Error::Validation("Project path must be absolute.".into()));
    }
    for ancestor in raw.ancestors() {
        if ancestor.exists() {
            reject_reparse(ancestor)?;
        }
    }
    let root = fs::canonicalize(raw)?;
    if !root.is_dir()
        || root.parent().is_none()
        || root.parent().is_some_and(|p| p.parent().is_none())
    {
        return Err(Error::Validation(
            "Deletion refuses a filesystem root.".into(),
        ));
    }
    if root.join(".git").is_file() {
        return Err(Error::Validation(
            "Linked Git worktrees must be removed from their owner before deleting this directory."
                .into(),
        ));
    }
    let data_dir = fs::canonicalize(
        database_path
            .parent()
            .ok_or_else(|| Error::Validation("Database path has no parent.".into()))?,
    )?;
    if overlaps(&root, &data_dir) {
        return Err(Error::Validation(
            "Deletion refuses the application data directory or its parent.".into(),
        ));
    }
    if let Ok(home) = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME"))
        && let Ok(home) = fs::canonicalize(home)
        && home.starts_with(&root)
    {
        return Err(Error::Validation(
            "Deletion refuses the home directory or its parent.".into(),
        ));
    }
    if let Ok(cwd) = std::env::current_dir()
        && cwd.starts_with(&root)
    {
        return Err(Error::Validation(
            "Deletion refuses the process working directory.".into(),
        ));
    }
    if let Ok(exe) = std::env::current_exe()
        && exe.starts_with(&root)
    {
        return Err(Error::Validation(
            "Deletion refuses the running executable directory.".into(),
        ));
    }
    let mut statement = db.prepare("SELECT workspace_path FROM projects WHERE id<>?1")?;
    for other in statement.query_map([&project.id], |r| r.get::<_, String>(0))? {
        let other = other?;
        let path = fs::canonicalize(&other).unwrap_or_else(|_| PathBuf::from(&other));
        if overlaps(&root, &path) {
            return Err(Error::Validation(
                "Deletion refuses overlapping registered project roots.".into(),
            ));
        }
    }
    for entry in WalkDir::new(&root).follow_links(false) {
        let entry = entry.map_err(|e| Error::Validation(e.to_string()))?;
        reject_reparse(entry.path())?;
    }
    Ok(root)
}

fn active_work(db: &Connection, id: &str) -> Result<bool, Error> {
    let active:bool=db.query_row("SELECT
        EXISTS(SELECT 1 FROM runtime_slots WHERE project_id=?1)
        OR EXISTS(SELECT 1 FROM jobs WHERE project_id=?1 AND status='running')
        OR EXISTS(SELECT 1 FROM companion_canvas_requests WHERE project_id=?1 AND state='running')
        OR EXISTS(SELECT 1 FROM terminal_runs WHERE project_id=?1 AND status IN ('queued','starting','running'))
        OR EXISTS(SELECT 1 FROM artifact_publications WHERE project_id=?1 AND state='applying')
        OR EXISTS(SELECT 1 FROM chat_messages m JOIN chat_conversations c ON c.id=m.conversation_id WHERE c.project_id=?1 AND m.status IN ('pending','streaming'))
        OR EXISTS(SELECT 1 FROM forge_turns t JOIN forge_features f ON f.id=t.feature_id WHERE f.project_id=?1 AND t.status='running')
        OR EXISTS(SELECT 1 FROM companion_note_turns t JOIN companion_notes n ON n.id=t.note_id WHERE n.project_id=?1 AND t.status='running')",[id],|r|r.get(0))?;
    Ok(active)
}

fn check_deletion_idle(db: &Connection, id: &str) -> Result<(), Error> {
    if active_work(db, id)? {
        return Err(Error::Validation("The project has an active Companion turn, agent, terminal or filesystem operation. Stop it before deletion.".into()));
    }
    let pending: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM project_deletion_journal)",
        [],
        |row| row.get(0),
    )?;
    if pending {
        return Err(Error::Validation(
            "Another project deletion still needs recovery.".into(),
        ));
    }
    Ok(())
}

fn rollback_prepared(
    db: &Connection,
    id: &str,
    root: &Path,
    quarantine: &Path,
) -> Result<(), Error> {
    valid_quarantine(root, quarantine)?;
    if quarantine.exists() {
        if root.exists() {
            return Err(Error::Validation("Both the original and quarantined project directories exist; manual recovery is required.".into()));
        }
        fs::rename(quarantine, root)?;
    } else if !root.exists() {
        return Err(Error::Validation("Neither the original nor quarantined project directory exists; manual recovery is required.".into()));
    }
    let was_archived:Option<i64>=db.query_row("SELECT was_archived FROM project_deletion_journal WHERE project_id=?1 AND phase='prepared'",[id],|r|r.get(0)).optional()?;
    if let Some(was_archived) = was_archived {
        db.execute(
            "UPDATE projects SET is_archived=?2 WHERE id=?1",
            params![id, was_archived],
        )?;
        db.execute(
            "DELETE FROM project_deletion_journal WHERE project_id=?1",
            [id],
        )?;
    }
    Ok(())
}

fn valid_quarantine(root: &Path, quarantine: &Path) -> Result<(), Error> {
    let suffix = quarantine
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_prefix(".hvc-delete-"));
    if root.parent() != quarantine.parent() || suffix.is_none_or(|id| Uuid::parse_str(id).is_err())
    {
        return Err(Error::Validation("Invalid project quarantine path.".into()));
    }
    Ok(())
}

fn purge_project(
    db: &mut Connection,
    id: &str,
    project: &Project,
    root: &Path,
) -> Result<(), Error> {
    db.pragma_update(None, "secure_delete", "ON")?;
    db.pragma_update(None, "foreign_keys", "OFF")?;
    let result = (|| {
        let tx = db.transaction_with_behavior(TransactionBehavior::Exclusive)?;
        tx.execute("INSERT INTO retained_usage_projects(project_id,project_name,deleted_at) VALUES(?1,?2,strftime('%Y-%m-%dT%H:%M:%fZ','now'))",params![id,project.name])?;
        tx.execute("INSERT INTO retained_project_usage SELECT * FROM token_usage_records WHERE project_id=?1",[id])?;
        tx.execute("DELETE FROM token_usage_records WHERE project_id=?1", [id])?;
        delete_project_graph(&tx, id)?;
        tx.execute(
            "INSERT INTO companion_agent_output_fts(companion_agent_output_fts) VALUES('optimize')",
            [],
        )?;
        tx.execute(
            "INSERT INTO companion_pulsation_digest_fts(companion_pulsation_digest_fts) VALUES('optimize')",
            [],
        )?;
        let canonical = root.to_string_lossy().to_string();
        for table in [
            "workspace_note_embeddings",
            "embedding_sync_state",
            "embedding_deltas",
        ] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE workspace_path IN (?1,?2)"),
                params![project.workspace_path, canonical],
            )?;
        }
        for key in [
            format!("companion.target.{id}"),
            format!("forge.navigation.{id}"),
            format!("brownfield.onboarding_consent.{id}"),
        ] {
            tx.execute("DELETE FROM runtime_settings WHERE key=?1", [key])?;
        }
        // Affinity hashes include project IDs, but have no project key. They are short lived;
        // clearing the cache also avoids leaving an untraceable reference to this project.
        tx.execute("DELETE FROM llm_route_affinity", [])?;
        let broken: i64 =
            tx.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })?;
        if broken != 0 {
            return Err(Error::Validation(format!(
                "Project deletion would leave {broken} broken foreign keys."
            )));
        }
        tx.execute(
            "UPDATE project_deletion_journal SET phase='purged' WHERE project_id=?1",
            [id],
        )?;
        tx.commit()?;
        Ok(())
    })();
    db.pragma_update(None, "foreign_keys", "ON")?;
    result
}

fn quoted(name: &str) -> Result<String, Error> {
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return Err(Error::Validation(
            "Unexpected SQL identifier in schema.".into(),
        ));
    }
    Ok(format!("\"{name}\""))
}

fn delete_project_graph(tx: &Transaction<'_>, id: &str) -> Result<(), Error> {
    tx.execute_batch("CREATE TEMP TABLE project_deletion_rows(table_name TEXT NOT NULL,row_id INTEGER NOT NULL,PRIMARY KEY(table_name,row_id));")?;
    tx.execute(
        "INSERT INTO project_deletion_rows SELECT 'projects',rowid FROM projects WHERE id=?1",
        [id],
    )?;
    let names=tx.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE 'companion_agent_output_fts%' AND name NOT LIKE 'companion_pulsation_digest_fts%' ORDER BY name")?
        .query_map([],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let mut edges = Vec::new();
    for child in &names {
        let pragma = format!("PRAGMA foreign_key_list({})", quoted(child)?);
        let mut groups: BTreeMap<i64, (String, Vec<(String, String)>)> = BTreeMap::new();
        for item in tx.prepare(&pragma)?.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })? {
            let (fk_id, parent, from, to) = item?;
            groups
                .entry(fk_id)
                .or_insert_with(|| (parent, Vec::new()))
                .1
                .push((
                    from,
                    to.ok_or_else(|| {
                        Error::Validation(
                            "Implicit foreign-key target is unsupported by deletion.".into(),
                        )
                    })?,
                ));
        }
        for (_, (parent, columns)) in groups {
            edges.push((child.clone(), parent, columns));
        }
    }
    loop {
        let mut added = 0;
        for (child, parent, columns) in &edges {
            let condition = columns
                .iter()
                .map(|(from, to)| Ok(format!("c.{}=p.{}", quoted(from)?, quoted(to)?)))
                .collect::<Result<Vec<_>, Error>>()?
                .join(" AND ");
            let sql = format!(
                "INSERT OR IGNORE INTO project_deletion_rows SELECT ?1,c.rowid FROM {} c JOIN {} p ON {condition} WHERE EXISTS(SELECT 1 FROM project_deletion_rows d WHERE d.table_name=?2 AND d.row_id=p.rowid)",
                quoted(child)?,
                quoted(parent)?
            );
            added += tx.execute(&sql, params![child, parent])?;
        }
        if added == 0 {
            break;
        }
    }
    for table in &names {
        let pragma = format!("PRAGMA table_info({})", quoted(table)?);
        let columns = tx
            .prepare(&pragma)?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if columns.iter().any(|column| column == "project_id") && table != "projects" {
            let sql = format!(
                "SELECT count(*) FROM {} WHERE rowid IN (SELECT row_id FROM project_deletion_rows WHERE table_name=?1) AND project_id IS NOT NULL AND project_id<>?2",
                quoted(table)?
            );
            let foreign: i64 = tx.query_row(&sql, params![table, id], |r| r.get(0))?;
            if foreign != 0 {
                return Err(Error::Validation(format!(
                    "Deletion would affect {foreign} rows owned by another project in {table}."
                )));
            }
        }
    }
    for table in names.iter().filter(|name| name.as_str() != "projects") {
        let sql = format!(
            "DELETE FROM {} WHERE rowid IN (SELECT row_id FROM project_deletion_rows WHERE table_name=?1)",
            quoted(table)?
        );
        tx.execute(&sql, [table])?;
    }
    tx.execute("DELETE FROM projects WHERE rowid IN (SELECT row_id FROM project_deletion_rows WHERE table_name='projects')",[])?;
    tx.execute_batch("DROP TABLE project_deletion_rows")?;
    Ok(())
}

fn finish_purged(db: &Connection, data_dir: &Path, id: &str) -> Result<(), Error> {
    let record:Option<(String,String)>=db.query_row("SELECT root_path,quarantine_path FROM project_deletion_journal WHERE project_id=?1 AND phase='purged'",[id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let Some((root, quarantine)) = record else {
        return Ok(());
    };
    let root = PathBuf::from(root);
    let quarantine = PathBuf::from(quarantine);
    valid_quarantine(&root, &quarantine)?;
    if quarantine.exists() {
        for entry in WalkDir::new(&quarantine).follow_links(false) {
            let entry = entry.map_err(|e| Error::Validation(e.to_string()))?;
            reject_reparse(entry.path())?;
        }
        fs::remove_dir_all(&quarantine)?;
    }
    // Pulsation reports quote the removed project's criteria and paths.
    crate::reports::store::sweep_orphan_reports(db, data_dir)?;
    db.pragma_update(None, "secure_delete", "ON")?;
    // Compact pages freed by the project graph and FTS before creating a survivor backup.
    db.execute_batch("VACUUM")?;
    let clean_backup = data_dir.join(format!(
        "{}.after-deletion.{}.db",
        crate::app_names::snapshot_prefix(),
        Uuid::new_v4()
    ));
    db.execute("VACUUM INTO ?1", [clean_backup.to_string_lossy().as_ref()])?;
    let backup_result = (|| -> Result<(), Error> {
        let copy = Connection::open(&clean_backup)?;
        copy.pragma_update(None, "secure_delete", "ON")?;
        copy.execute(
            "DELETE FROM project_deletion_journal WHERE project_id=?1",
            [id],
        )?;
        copy.execute_batch("VACUUM")?;
        let integrity: String = copy.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        if integrity != "ok" {
            return Err(Error::Validation(format!(
                "Clean backup failed integrity check: {integrity}"
            )));
        }
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&clean_backup)?
            .sync_all()?;
        Ok(())
    })();
    if let Err(error) = backup_result {
        let _ = fs::remove_file(&clean_backup);
        return Err(error);
    }
    // Older app-owned migration snapshots and earlier clean snapshots can contain
    // the removed repository's data. Retain only the new verified snapshot.
    for entry in fs::read_dir(data_dir)? {
        let entry = entry?;
        if entry.path() == clean_backup {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        // Snapshots written before the rename to Ikore carry the legacy prefix.
        let old = ["pre-v13", "pre-v24", "after-deletion"]
            .into_iter()
            .find_map(|kind| crate::app_names::snapshot_suffix(&name, kind));
        if let Some(suffix) = old.and_then(|s| s.strip_suffix(".db"))
            && Uuid::parse_str(suffix).is_ok()
            && entry.file_type()?.is_file()
        {
            fs::remove_file(entry.path())?;
        }
    }
    db.execute(
        "DELETE FROM project_deletion_journal WHERE project_id=?1",
        [id],
    )?;
    if let Err(error) = db.execute_batch("VACUUM") {
        // secure_delete zeroed the final journal row; compaction can retry later.
        eprintln!("post-deletion journal compaction deferred: {error}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::persistence_service::{
        CreatePulsationRequest, EnsureProjectForWorkspaceRequest, PersistenceService,
    };
    use rusqlite::params;
    use std::fs;
    use uuid::Uuid;

    #[test]
    fn deletes_one_repository_and_its_history_but_retains_attributed_usage() {
        let fixture = tempfile::tempdir().unwrap();
        let data = fixture.path().join("app-data");
        let removed = fixture.path().join("removed-repository");
        let survivor = fixture.path().join("survivor-repository");
        fs::create_dir_all(removed.join(".git")).unwrap();
        fs::create_dir_all(&survivor).unwrap();
        fs::write(removed.join(".git/config"), "[core]\n").unwrap();
        fs::write(removed.join("notes.md"), "private project content").unwrap();
        fs::write(survivor.join("keep.md"), "other project").unwrap();
        let service = PersistenceService::initialize(&data).unwrap();
        let deleted = service
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: removed.to_string_lossy().into_owned(),
                preferred_name: Some("Removed usage label".into()),
            })
            .unwrap()
            .project;
        let kept = service
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: survivor.to_string_lossy().into_owned(),
                preferred_name: Some("Survivor".into()),
            })
            .unwrap()
            .project;
        let pulse = service
            .create_pulsation(CreatePulsationRequest {
                project_id: deleted.id.clone(),
                feature_name: "Deleted pulse".into(),
            })
            .unwrap()
            .pulsation;
        let db = service.connection().unwrap();
        for (project, source) in [(&deleted, "deleted-source"), (&kept, "survivor-source")] {
            db.execute("INSERT INTO token_usage_records(source_kind,source_id,unit_key,project_id,transport,provider_id,requested_model,input_tokens,output_tokens,total_tokens,quality,started_at,updated_at) VALUES('terminal_run',?1,'response',?2,'cli','codex','test-model',10,5,15,'complete','2026-09-26T12:00:00Z','2026-09-26T12:00:00Z')", params![source,project.id]).unwrap();
        }
        db.execute("INSERT INTO chat_conversations(id,project_id,provider_id_snapshot,model_id_snapshot,title,created_at,updated_at) VALUES('deleted-chat',?1,'openai','test-model','Private chat','2026-09-26','2026-09-26')",[&deleted.id]).unwrap();
        db.execute("INSERT INTO chat_messages(id,conversation_id,ordinal,role,content,status,created_at,updated_at) VALUES('deleted-message','deleted-chat',1,'user','secret text','completed','2026-09-26','2026-09-26')",[]).unwrap();
        db.execute("INSERT INTO companion_agent_output_documents(project_id,stable_reference,source_kind,source_id,status,title,content,created_at,updated_at) VALUES(?1,'old-output','artifact','source','completed','Private title','private FTS text','2026-09-26','2026-09-26')",[&deleted.id]).unwrap();
        db.execute("INSERT INTO runtime_settings(key,value,updated_at) VALUES(?1,'private setting','2026-09-26')",[format!("companion.target.{}",deleted.id)]).unwrap();
        db.execute("INSERT INTO runtime_settings(key,value,updated_at) VALUES('runtime.provider','openai','2026-09-26') ON CONFLICT(key) DO UPDATE SET value=excluded.value",[]).unwrap();
        // Mandates and receipts are append-only, yet still go with the project.
        let digest = "0".repeat(64);
        db.execute("INSERT INTO execution_mandates(id,pulsation_id,revision,cause,enforcement,mandate_json,mandate_sha256,checkpoint_id,created_at) VALUES('deleted-mandate',?1,1,'issued','enforced_v4','{}',?2,'deleted-checkpoint','2026-10-26')",params![pulse.id,digest]).unwrap();
        db.execute("INSERT INTO checkpoint_approval_receipts(checkpoint_id,pulsation_id,purpose,decision,receipt_json,receipt_sha256,mandate_revision,created_at) VALUES('deleted-checkpoint',?1,'artifact_review','approved','{}',?2,1,'2026-10-26')",params![pulse.id,digest]).unwrap();
        db.execute("INSERT INTO mandate_enforcement_events(id,pulsation_id,stage,rule,relative_path,detail,mandate_revision,created_at,updated_at) VALUES('deleted-event',?1,'Code','scope_violation','src/private.rs','refused',1,'2026-10-26','2026-10-26')",[&pulse.id]).unwrap();
        // Attestations go with the project; keys are global and stay.
        let key_id = format!("hvc-ed25519:{}", "b".repeat(64));
        db.execute("INSERT INTO attestation_keys(key_id,algorithm,public_key,state,created_at) VALUES(?1,'ed25519','AAAA','active','2026-10-26')",[&key_id]).unwrap();
        db.execute("INSERT INTO attestation_seal_intents(pulsation_id,state,created_at,updated_at) VALUES(?1,'sealed','2026-10-26','2026-10-26')",[&pulse.id]).unwrap();
        db.execute("INSERT INTO pulsation_attestations(pulsation_id,schema_version,predicate_type,deriver_version,statement_json,statement_sha256,envelope_json,key_id,signature_state,outcome,subject_count,sealed_at,signed_at) VALUES(?1,1,'urn:hvc:pulsation-attestation:v1',1,'{}',?2,'{}',?3,'signed','failed',0,'2026-10-26','2026-10-26')",params![pulse.id,digest,key_id]).unwrap();
        db.execute("INSERT INTO attestation_verifications(pulsation_id,checked_at,signature,rederivation,workspace_json) VALUES(?1,'2026-10-26','valid','matches','[]')",[&pulse.id]).unwrap();
        let global_connections: i64 = db
            .query_row("SELECT count(*) FROM provider_connections", [], |r| {
                r.get(0)
            })
            .unwrap();
        drop(db);
        // Pulsation reports quote criteria and paths; only the kept
        // project's may survive.
        let kept_pulse = service
            .create_pulsation(CreatePulsationRequest {
                project_id: kept.id.clone(),
                feature_name: "Kept pulse".into(),
            })
            .unwrap()
            .pulsation;
        let deleted_report =
            crate::reports::store::write_report(&data, &pulse.id, "private report").unwrap();
        let kept_report =
            crate::reports::store::write_report(&data, &kept_pulse.id, "kept report").unwrap();
        let old_backup = data.join(format!("hard-vibe-code.pre-v13.{}.db", Uuid::new_v4()));
        fs::copy(service.database_path(), &old_backup).unwrap();

        let preview = service.preview_project_deletion(&deleted.id).unwrap();
        assert_eq!(preview.pulsations, 1);
        assert_eq!(preview.conversations, 1);
        service
            .delete_project_permanently(&deleted.id, &deleted.name)
            .unwrap();

        assert!(!removed.exists());
        assert!(!old_backup.exists());
        assert!(survivor.join("keep.md").exists());
        assert!(!deleted_report.parent().unwrap().exists());
        assert_eq!(fs::read_to_string(&kept_report).unwrap(), "kept report");
        let clean_backups = fs::read_dir(&data)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("ikore.after-deletion.")
            })
            .collect::<Vec<_>>();
        assert_eq!(clean_backups.len(), 1);
        let backup = rusqlite::Connection::open(&clean_backups[0]).unwrap();
        assert_eq!(
            backup
                .query_row(
                    "SELECT count(*) FROM projects WHERE id=?1",
                    [&deleted.id],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert_eq!(
            backup
                .query_row("SELECT count(*) FROM project_deletion_journal", [], |r| r
                    .get::<_, i64>(
                    0
                ))
                .unwrap(),
            0
        );
        let db = service.connection().unwrap();
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM projects WHERE id=?1",
                [&deleted.id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM pulsations WHERE id=?1",
                [&pulse.id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM chat_messages WHERE id='deleted-message'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(db.query_row("SELECT count(*) FROM companion_agent_output_fts WHERE companion_agent_output_fts MATCH 'private'",[],|r|r.get::<_,i64>(0)).unwrap(),0);
        for table in [
            "execution_mandates",
            "checkpoint_approval_receipts",
            "mandate_enforcement_events",
            "attestation_seal_intents",
            "pulsation_attestations",
            "attestation_verifications",
        ] {
            assert_eq!(
                db.query_row(
                    &format!("SELECT count(*) FROM {table} WHERE pulsation_id=?1"),
                    [&pulse.id],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
                0,
                "{table}"
            );
        }
        assert_eq!(
            db.query_row("SELECT count(*) FROM attestation_keys", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1,
            "attestation keys are global"
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM runtime_settings WHERE key=?1",
                [format!("companion.target.{}", deleted.id)],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            db.query_row(
                "SELECT value FROM runtime_settings WHERE key='runtime.provider'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "openai"
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM provider_connections", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            global_connections
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM token_usage_records WHERE project_id=?1",
                [&deleted.id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM retained_project_usage WHERE project_id=?1",
                [&deleted.id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert_eq!(
            db.query_row(
                "SELECT project_name FROM retained_usage_projects WHERE project_id=?1",
                [&deleted.id],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "Removed usage label"
        );
        let summary = crate::usage::query_usage(
            service.database_path(),
            &crate::usage::UsageQuery {
                project_id: Some(deleted.id.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(summary.totals.total_tokens, Some(15));
        assert!(
            summary
                .projects
                .iter()
                .any(|project| project.id == deleted.id && project.name == "Removed usage label")
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM projects WHERE id=?1",
                [&kept.id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
            0
        );
    }

    #[test]
    fn refuses_overlapping_repository_roots() {
        let fixture = tempfile::tempdir().unwrap();
        let parent = fixture.path().join("parent");
        let child = parent.join("child");
        fs::create_dir_all(&child).unwrap();
        let service = PersistenceService::initialize(&fixture.path().join("data")).unwrap();
        let first = service
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: parent.to_string_lossy().into_owned(),
                preferred_name: None,
            })
            .unwrap()
            .project;
        service
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: child.to_string_lossy().into_owned(),
                preferred_name: None,
            })
            .unwrap();
        assert!(
            service
                .delete_project_permanently(&first.id, &first.name)
                .is_err()
        );
        assert!(child.exists());
    }

    #[test]
    fn refuses_active_turns_and_wrong_confirmation_without_touching_disk() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("repo");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("keep.txt"), "content").unwrap();
        let service = PersistenceService::initialize(&fixture.path().join("data")).unwrap();
        let project = service
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: root.to_string_lossy().into_owned(),
                preferred_name: Some("Project".into()),
            })
            .unwrap()
            .project;
        assert!(
            service
                .delete_project_permanently(&project.id, "wrong")
                .is_err()
        );
        let db = service.connection().unwrap();
        db.execute("INSERT INTO chat_conversations(id,project_id,provider_id_snapshot,model_id_snapshot,title,created_at,updated_at) VALUES('chat',?1,'openai','model','title','2026-09-26','2026-09-26')",[&project.id]).unwrap();
        db.execute("INSERT INTO chat_messages(id,conversation_id,ordinal,role,content,status,created_at,updated_at) VALUES('message','chat',1,'assistant','','streaming','2026-09-26','2026-09-26')",[]).unwrap();
        assert!(
            service
                .delete_project_permanently(&project.id, &project.name)
                .is_err()
        );
        assert!(root.join("keep.txt").exists());
        assert_eq!(
            db.query_row("SELECT count(*) FROM project_deletion_journal", [], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
            0
        );
    }

    /// ADR-029: a running Scribe turn blocks deletion like any other agent
    /// work, and the notes and their turns go with the project.
    #[test]
    fn refuses_a_running_note_turn_and_purges_companion_notes() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("repo");
        fs::create_dir_all(&root).unwrap();
        let service = PersistenceService::initialize(&fixture.path().join("data")).unwrap();
        let project = service
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: root.to_string_lossy().into_owned(),
                preferred_name: Some("Project".into()),
            })
            .unwrap()
            .project;
        let db = service.connection().unwrap();
        db.execute("INSERT INTO companion_notes(id,project_id,capture_request_id,raw_input,created_at,updated_at) VALUES('note',?1,'capture','Private thought','2026-10-22','2026-10-22')",[&project.id]).unwrap();
        db.execute("INSERT INTO companion_note_turns(id,note_id,note_revision,ordinal,request_id,kind,status,created_at,updated_at) VALUES('turn','note',1,1,'capture','structure','running','2026-10-22','2026-10-22')",[]).unwrap();
        assert!(
            service
                .delete_project_permanently(&project.id, &project.name)
                .is_err()
        );
        assert!(root.exists());
        db.execute(
            "UPDATE companion_note_turns SET status='failed' WHERE id='turn'",
            [],
        )
        .unwrap();
        drop(db);
        service
            .delete_project_permanently(&project.id, &project.name)
            .unwrap();
        let db = service.connection().unwrap();
        for table in ["companion_notes", "companion_note_turns"] {
            assert_eq!(
                db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                0,
                "{table}"
            );
        }
    }

    #[test]
    fn startup_restores_a_renamed_folder_before_database_purge() {
        let fixture = tempfile::tempdir().unwrap();
        let data = fixture.path().join("data");
        let root = fixture.path().join("repo");
        let quarantined = fixture
            .path()
            .join(format!(".hvc-delete-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("keep.txt"), "still here").unwrap();
        let service = PersistenceService::initialize(&data).unwrap();
        let project = service
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: root.to_string_lossy().into_owned(),
                preferred_name: None,
            })
            .unwrap()
            .project;
        let db = service.connection().unwrap();
        db.execute("INSERT INTO project_deletion_journal(project_id,root_path,quarantine_path,was_archived,phase,started_at) VALUES(?1,?2,?3,0,'prepared','2026-09-26')",params![project.id,root.to_string_lossy(),quarantined.to_string_lossy()]).unwrap();
        db.execute(
            "UPDATE projects SET is_archived=1 WHERE id=?1",
            [&project.id],
        )
        .unwrap();
        fs::rename(&root, &quarantined).unwrap();
        assert!(
            service
                .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                    workspace_path: root.to_string_lossy().into_owned(),
                    preferred_name: None
                })
                .is_err()
        );
        drop(db);
        drop(service);
        let recovered = PersistenceService::initialize(&data).unwrap();
        assert!(root.join("keep.txt").exists());
        assert!(!quarantined.exists());
        let db = recovered.connection().unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM project_deletion_journal", [], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
            0
        );
        assert_eq!(
            db.query_row(
                "SELECT is_archived FROM projects WHERE id=?1",
                [&project.id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }

    #[test]
    fn startup_finishes_a_committed_deletion_and_removes_old_backups() {
        let fixture = tempfile::tempdir().unwrap();
        let data = fixture.path().join("data");
        let original = fixture.path().join("removed");
        let quarantine = fixture
            .path()
            .join(format!(".hvc-delete-{}", Uuid::new_v4()));
        fs::create_dir_all(&quarantine).unwrap();
        fs::write(quarantine.join("private.txt"), "private content").unwrap();
        let service = PersistenceService::initialize(&data).unwrap();
        let backup = data.join(format!("hard-vibe-code.pre-v24.{}.db", Uuid::new_v4()));
        fs::copy(service.database_path(), &backup).unwrap();
        let db = service.connection().unwrap();
        db.execute("INSERT INTO project_deletion_journal(project_id,root_path,quarantine_path,was_archived,phase,started_at) VALUES('removed',?1,?2,0,'purged','2026-09-26')",params![original.to_string_lossy(),quarantine.to_string_lossy()]).unwrap();
        drop(db);
        drop(service);
        let recovered = PersistenceService::initialize(&data).unwrap();
        assert!(!quarantine.exists());
        assert!(!backup.exists());
        assert_eq!(recovered.pending_project_deletion_count().unwrap(), 0);
        assert_eq!(
            fs::read_dir(&data)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("ikore.after-deletion."))
                .count(),
            1
        );
    }

    #[test]
    fn startup_removes_reports_whose_pulsation_no_longer_exists() {
        let fixture = tempfile::tempdir().unwrap();
        let data = fixture.path().join("data");
        let service = PersistenceService::initialize(&data).unwrap();
        let orphan = crate::reports::store::write_report(&data, "gone", "orphan").unwrap();
        drop(service);
        PersistenceService::initialize(&data).unwrap();
        assert!(!orphan.parent().unwrap().exists());
        assert!(data.join(crate::reports::store::REPORTS_DIR).exists());
    }

    #[test]
    fn refuses_a_linked_git_worktree_before_creating_a_journal() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("linked-worktree");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join(".git"),
            "gitdir: ../shared/.git/worktrees/example",
        )
        .unwrap();
        let service = PersistenceService::initialize(&fixture.path().join("data")).unwrap();
        let project = service
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: root.to_string_lossy().into_owned(),
                preferred_name: None,
            })
            .unwrap()
            .project;
        assert!(
            service
                .delete_project_permanently(&project.id, &project.name)
                .is_err()
        );
        assert!(root.join(".git").exists());
        assert_eq!(service.pending_project_deletion_count().unwrap(), 0);
    }
}
