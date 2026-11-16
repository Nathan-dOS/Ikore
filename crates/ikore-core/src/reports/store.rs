//! Where pulsation reports live on disk (ADR-027 §5).
//!
//! A report is written on demand as
//! `<data dir>/reports/<pulsation id>/pulsation-report.html`, rewritten on each
//! request, and removed with its pulsation: a directory whose pulsation no
//! longer exists is swept after a project deletion and at startup. It is never
//! a document of record; "Save as…" copies it elsewhere.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use uuid::Uuid;

use crate::persistence_service::PersistenceServiceError;

pub const REPORTS_DIR: &str = "reports";
pub const REPORT_FILE: &str = "pulsation-report.html";

/// A pulsation id usable as one directory name: no separator, no dot, no
/// drive prefix. Host ids are UUIDs.
fn is_report_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// The report path of a pulsation, `None` for an id that is not a plain name.
pub fn report_path(data_dir: &Path, pulsation_id: &str) -> Option<PathBuf> {
    is_report_id(pulsation_id).then(|| {
        data_dir
            .join(REPORTS_DIR)
            .join(pulsation_id)
            .join(REPORT_FILE)
    })
}

/// Replaces the report file in one rename, so a reader never sees half a
/// document.
pub(crate) fn write_report(
    data_dir: &Path,
    pulsation_id: &str,
    html: &str,
) -> Result<PathBuf, PersistenceServiceError> {
    let target = report_path(data_dir, pulsation_id).ok_or_else(|| {
        PersistenceServiceError::Validation("The pulsation id cannot name a report.".into())
    })?;
    let directory = target
        .parent()
        .expect("a report path always has its pulsation directory");
    fs::create_dir_all(directory)?;
    let staging = directory.join(format!(".{REPORT_FILE}.{}.tmp", Uuid::new_v4()));
    fs::write(&staging, html.as_bytes())?;
    if let Err(error) = fs::rename(&staging, &target) {
        let _ = fs::remove_file(&staging);
        return Err(error.into());
    }
    Ok(target)
}

/// Removes every report directory whose pulsation no longer exists and
/// returns how many. Anything that is not a plain directory named like a
/// pulsation id is left alone, and links are never followed.
pub(crate) fn sweep_orphan_reports(
    db: &Connection,
    data_dir: &Path,
) -> Result<usize, PersistenceServiceError> {
    let root = data_dir.join(REPORTS_DIR);
    match fs::symlink_metadata(&root) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return Ok(0),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    }
    let mut removed = 0;
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if !file_type.is_dir() || file_type.is_symlink() {
            continue;
        }
        let name = entry.file_name();
        let Some(id) = name.to_str().filter(|id| is_report_id(id)) else {
            continue;
        };
        let exists: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM pulsations WHERE id=?1)",
            [id],
            |row| row.get(0),
        )?;
        if !exists {
            fs::remove_dir_all(entry.path())?;
            removed += 1;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_ids_name_a_report() {
        let data = Path::new("data");
        assert_eq!(
            report_path(data, "5d0c2a6e-8b1f-4c3a-9e27-61f0b4a9d2c8"),
            Some(
                data.join("reports")
                    .join("5d0c2a6e-8b1f-4c3a-9e27-61f0b4a9d2c8")
                    .join("pulsation-report.html")
            )
        );
        for hostile in ["", "..", "a/b", "a\\b", "C:", "a.b", "../x"] {
            assert_eq!(report_path(data, hostile), None, "{hostile:?}");
        }
    }

    #[test]
    fn writing_replaces_the_report_in_place() {
        let data = tempfile::tempdir().unwrap();
        let first = write_report(data.path(), "pulse", "<p>one</p>").unwrap();
        let second = write_report(data.path(), "pulse", "<p>two</p>").unwrap();
        assert_eq!(first, second);
        assert_eq!(fs::read_to_string(&second).unwrap(), "<p>two</p>");
        let entries: Vec<_> = fs::read_dir(second.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(entries, [REPORT_FILE], "no staging file is left behind");
    }

    #[test]
    fn the_sweep_removes_only_reports_of_missing_pulsations() {
        let data = tempfile::tempdir().unwrap();
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE pulsations(id TEXT PRIMARY KEY); INSERT INTO pulsations VALUES('kept');",
        )
        .unwrap();
        assert_eq!(sweep_orphan_reports(&db, data.path()).unwrap(), 0);
        write_report(data.path(), "kept", "kept").unwrap();
        write_report(data.path(), "gone", "gone").unwrap();
        let root = data.path().join(REPORTS_DIR);
        fs::create_dir_all(root.join("not.an.id")).unwrap();
        fs::write(root.join("notes.txt"), "not a report").unwrap();

        assert_eq!(sweep_orphan_reports(&db, data.path()).unwrap(), 1);
        assert!(root.join("kept").join(REPORT_FILE).exists());
        assert!(!root.join("gone").exists());
        assert!(root.join("not.an.id").exists());
        assert!(root.join("notes.txt").exists());
    }
}
