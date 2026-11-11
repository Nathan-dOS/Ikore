//! Replayable one-way JSON/SVG mirrors under the registered project's managed directory.
use super::service::SyncReport;
use crate::feature_forge::service::{check_project, invalid};
use crate::persistence_service::{PersistenceService, PersistenceServiceError as Error};
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub(crate) fn hash(bytes: &[u8]) -> String {
    crate::lower_hex(&Sha256::digest(bytes))
}

struct Job {
    id: i64,
    drawing: String,
    revision: u64,
    operation: String,
    basename: String,
    json: Option<String>,
    svg: Option<String>,
    expected_json: Option<String>,
    expected_svg: Option<String>,
    desired_json: Option<String>,
    desired_svg: Option<String>,
}

enum SyncFailure {
    Conflict,
    Unavailable,
}

pub(crate) fn sync_project(
    persistence: &PersistenceService,
    project: &str,
) -> Result<SyncReport, Error> {
    let db = persistence.connection()?;
    check_project(&db, project)?;
    let registered: String = db.query_row(
        "SELECT workspace_path FROM projects WHERE id=?1",
        [project],
        |r| r.get(0),
    )?;
    // A pass is bounded to prevent a large queue monopolizing a blocking worker.
    let job_ids = db
        .prepare(
            "SELECT id FROM companion_drawing_sync_jobs WHERE project_id=?1 ORDER BY id LIMIT 1000",
        )?
        .query_map([project], |r| r.get::<_, i64>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut blocked = HashSet::new();
    let mut report = SyncReport::default();
    let started = Instant::now();
    for id in job_ids {
        if started.elapsed() >= Duration::from_secs(2) {
            break;
        }
        // Load one payload at a time, not up to 1000 complete SVG/JSON documents.
        let job = db.query_row(
            "SELECT id,drawing_id,revision,operation,basename,json_content,svg_content,
             expected_json_sha256,expected_svg_sha256,desired_json_sha256,desired_svg_sha256
             FROM companion_drawing_sync_jobs WHERE project_id=?1 AND id=?2",
            params![project, id],
            |r| {
                Ok(Job {
                    id: r.get(0)?,
                    drawing: r.get(1)?,
                    revision: r.get(2)?,
                    operation: r.get(3)?,
                    basename: r.get(4)?,
                    json: r.get(5)?,
                    svg: r.get(6)?,
                    expected_json: r.get(7)?,
                    expected_svg: r.get(8)?,
                    desired_json: r.get(9)?,
                    desired_svg: r.get(10)?,
                })
            },
        )?;
        if blocked.contains(&job.drawing) {
            continue;
        }
        // Project deletion shares the process guard; this recheck also stops archived projects.
        check_project(&db, project)?;
        db.execute(
            "UPDATE companion_drawing_sync_jobs SET started=1 WHERE id=?1 AND project_id=?2",
            params![job.id, project],
        )?;
        let result = mirror_job(Path::new(&registered), &job);
        match result {
            Ok(()) => {
                complete(&db, project, &job)?;
                report.completed += 1;
            }
            Err(failure) => {
                let (status, message) = match failure {
                    SyncFailure::Conflict => (
                        "conflict",
                        "A drawing file changed outside HVC; preserve it and export a copy",
                    ),
                    SyncFailure::Unavailable => (
                        "failed",
                        "Drawing files are pending; check the workspace and retry",
                    ),
                };
                db.execute("UPDATE companion_drawing_sync_jobs SET status=?1,error_message=?2 WHERE id=?3 AND project_id=?4",
                    params![status,message,job.id,project])?;
                blocked.insert(job.drawing);
            }
        }
    }
    report.pending = db.query_row(
        "SELECT count(*) FROM companion_drawing_sync_jobs WHERE project_id=?1",
        [project],
        |r| r.get(0),
    )?;
    report.conflicts = db.query_row("SELECT count(DISTINCT drawing_id) FROM companion_drawing_sync_jobs WHERE project_id=?1 AND status='conflict'", [project], |r| r.get(0))?;
    report.failed = db.query_row("SELECT count(DISTINCT drawing_id) FROM companion_drawing_sync_jobs WHERE project_id=?1 AND status='failed'", [project], |r| r.get(0))?;
    Ok(report)
}

fn complete(db: &Connection, project: &str, job: &Job) -> Result<(), Error> {
    // Persisting these together is essential: a crash never loses the replay hashes.
    db.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        if job.operation == "write" {
            db.execute(
                "UPDATE companion_drawings SET mirrored_revision=?1,json_sha256=?2,svg_sha256=?3
                WHERE project_id=?4 AND id=?5 AND mirrored_revision<=?1",
                params![
                    job.revision,
                    job.desired_json,
                    job.desired_svg,
                    project,
                    job.drawing
                ],
            )?;
        }
        db.execute(
            "DELETE FROM companion_drawing_sync_jobs WHERE id=?1 AND project_id=?2",
            params![job.id, project],
        )?;
        Ok::<_, Error>(())
    })();
    match result {
        Ok(()) => db.execute_batch("COMMIT").map_err(Error::from),
        Err(error) => {
            let _ = db.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn mirror_job(root: &Path, job: &Job) -> Result<(), SyncFailure> {
    let directory = checked_directory(root)?;
    if !valid_basename(&job.basename) {
        return Err(SyncFailure::Unavailable);
    }
    let json_path = directory.join(format!("{}.hvc-canvas.json", job.basename));
    let svg_path = directory.join(format!("{}.svg", job.basename));
    if job.operation == "delete" {
        remove_owned(&json_path, job.expected_json.as_deref())?;
        remove_owned(&svg_path, job.expected_svg.as_deref())?;
    } else {
        let json = job.json.as_deref().ok_or(SyncFailure::Unavailable)?;
        let svg = job.svg.as_deref().ok_or(SyncFailure::Unavailable)?;
        let json_hash = job
            .desired_json
            .as_deref()
            .ok_or(SyncFailure::Unavailable)?;
        let svg_hash = job.desired_svg.as_deref().ok_or(SyncFailure::Unavailable)?;
        if hash(json.as_bytes()) != json_hash || hash(svg.as_bytes()) != svg_hash {
            return Err(SyncFailure::Unavailable);
        }
        write_owned(
            &json_path,
            json.as_bytes(),
            job.expected_json.as_deref(),
            json_hash,
        )?;
        write_owned(
            &svg_path,
            svg.as_bytes(),
            job.expected_svg.as_deref(),
            svg_hash,
        )?;
    }
    Ok(())
}

fn valid_basename(value: &str) -> bool {
    value.len() >= 38
        && value.len() <= 100
        && value
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        && value
            .get(value.len() - 36..)
            .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
}

fn checked_directory(root: &Path) -> Result<PathBuf, SyncFailure> {
    if !root.is_absolute() {
        return Err(SyncFailure::Unavailable);
    }
    // Check the registered root and every ancestor: a root inside a junction is not safe.
    for ancestor in root.ancestors() {
        checked_metadata(ancestor, true)?;
    }
    let root = fs::canonicalize(root).map_err(|_| SyncFailure::Unavailable)?;
    let mut directory = root.clone();
    for component in [".hvc", "drawings"] {
        directory.push(component);
        match fs::symlink_metadata(&directory) {
            Ok(_) => checked_metadata(&directory, true)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&directory).map_err(|_| SyncFailure::Unavailable)?;
                checked_metadata(&directory, true)?;
            }
            Err(_) => return Err(SyncFailure::Unavailable),
        }
        let canonical = fs::canonicalize(&directory).map_err(|_| SyncFailure::Unavailable)?;
        if !canonical.starts_with(&root) {
            return Err(SyncFailure::Unavailable);
        }
    }
    Ok(directory)
}

fn checked_metadata(path: &Path, directory: bool) -> Result<(), SyncFailure> {
    let metadata = fs::symlink_metadata(path).map_err(|_| SyncFailure::Unavailable)?;
    if metadata.file_type().is_symlink()
        || directory && !metadata.is_dir()
        || !directory && !metadata.is_file()
    {
        return Err(SyncFailure::Unavailable);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(SyncFailure::Unavailable);
        }
    }
    Ok(())
}

fn current_hash(path: &Path) -> Result<Option<String>, SyncFailure> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(SyncFailure::Unavailable),
        Ok(metadata) => {
            checked_metadata(path, false)?;
            if metadata.len() > 16 * 1024 * 1024 {
                return Err(SyncFailure::Conflict);
            }
            Ok(Some(hash(
                &fs::read(path).map_err(|_| SyncFailure::Unavailable)?,
            )))
        }
    }
}

fn write_owned(
    path: &Path,
    content: &[u8],
    expected: Option<&str>,
    desired: &str,
) -> Result<(), SyncFailure> {
    let current = current_hash(path)?;
    if current.as_deref() == Some(desired) {
        return Ok(());
    }
    if current.is_some() && current.as_deref() != expected {
        return Err(SyncFailure::Conflict);
    }
    let parent = path.parent().ok_or(SyncFailure::Unavailable)?;
    checked_metadata(parent, true)?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|_| SyncFailure::Unavailable)?;
    file.write_all(content)
        .map_err(|_| SyncFailure::Unavailable)?;
    file.as_file()
        .sync_all()
        .map_err(|_| SyncFailure::Unavailable)?;
    checked_metadata(parent, true)?;
    if current_hash(path)? != current {
        return Err(SyncFailure::Conflict);
    }
    if current.is_some() {
        file.persist(path).map_err(|_| SyncFailure::Unavailable)?;
    } else {
        file.persist_noclobber(path)
            .map_err(|_| SyncFailure::Conflict)?;
    }
    // The checks reduce accidental overwrite; they are not an OS-level compare-and-swap.
    Ok(())
}

fn remove_owned(path: &Path, expected: Option<&str>) -> Result<(), SyncFailure> {
    let current = current_hash(path)?;
    if current.is_none() {
        return Ok(());
    }
    if current.as_deref() != expected {
        return Err(SyncFailure::Conflict);
    }
    checked_metadata(path.parent().ok_or(SyncFailure::Unavailable)?, true)?;
    if current_hash(path)? != current {
        return Err(SyncFailure::Conflict);
    }
    fs::remove_file(path).map_err(|_| SyncFailure::Unavailable)
}

/// Checks an existing registered root without creating it, useful to callers qualifying a workspace.
pub fn validate_drawing_workspace(root: &Path) -> Result<(), Error> {
    for ancestor in root.ancestors() {
        checked_metadata(ancestor, true)
            .map_err(|_| invalid("Drawing workspace is unavailable or linked"))?;
    }
    if !root.is_absolute() {
        return Err(invalid("Drawing workspace must be absolute"));
    }
    Ok(())
}
