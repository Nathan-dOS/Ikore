//! Baselines, blobs and signals of documentation reconciliation (ADR-034 M5).
//! Detection only computes hashes; nothing here reaches a model.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read as _;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

use super::links::{LinkCheck, NoteLink};
use super::{DriftOrigin, MAX_BLOB_BYTES, MAX_PROJECT_BLOB_BYTES};
use crate::feature_forge::service::{id, now};
use crate::persistence_service::PersistenceServiceError as Error;
use crate::repository_context::RepositoryContext;

/// One linked file as the host reads it now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LinkedFile {
    Present {
        sha256: String,
        /// The UTF-8 text when it fits the blob limit.
        text: Option<String>,
    },
    Absent,
    Excluded,
}

impl LinkedFile {
    pub(crate) fn sha256(&self) -> Option<&str> {
        match self {
            Self::Present { sha256, .. } => Some(sha256),
            _ => None,
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The lowercase hex SHA-256 every reconciliation hash uses.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    hex(Sha256::digest(bytes).as_slice())
}

/// The repository rules applied to one path, as [`LinkCheck`].
pub(crate) fn check_path(root: &Path, repository: &RepositoryContext, path: &str) -> LinkCheck {
    let Ok(relative) = crate::orchestration::RelativeSourcePath::new(path.to_owned()) else {
        return LinkCheck::Excluded;
    };
    if repository.validate_file_reference(&relative).is_ok() {
        return LinkCheck::Present;
    }
    match std::fs::symlink_metadata(root.join(path)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => LinkCheck::Absent,
        _ => LinkCheck::Excluded,
    }
}

/// Reads and hashes one linked file. Large files are hashed as a stream and
/// never kept.
pub(crate) fn read_linked(root: &Path, repository: &RepositoryContext, path: &str) -> LinkedFile {
    match check_path(root, repository, path) {
        LinkCheck::Absent => return LinkedFile::Absent,
        LinkCheck::Excluded => return LinkedFile::Excluded,
        LinkCheck::Present => {}
    }
    let Ok(mut file) = std::fs::File::open(root.join(path)) else {
        return LinkedFile::Excluded;
    };
    let mut hasher = Sha256::new();
    let mut kept = Vec::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut total = 0usize;
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                hasher.update(&buffer[..read]);
                if total + read <= MAX_BLOB_BYTES {
                    kept.extend_from_slice(&buffer[..read]);
                }
                total += read;
            }
            Err(_) => return LinkedFile::Excluded,
        }
    }
    let text = (total <= MAX_BLOB_BYTES)
        .then(|| String::from_utf8(kept).ok())
        .flatten();
    LinkedFile::Present {
        sha256: hex(hasher.finalize().as_slice()),
        text,
    }
}

/// The hash of a note's current bytes, if it can be read.
pub(crate) fn note_sha256(root: &Path, note: &str) -> Option<String> {
    crate::workspace_service::read_context_document(root, note)
        .ok()
        .map(|content| sha256_hex(content.as_bytes()))
}

/// Stores the base bytes of a file when they fit both limits; returns the
/// baseline state.
fn store_blob(
    db: &Connection,
    project: &str,
    sha256: &str,
    text: Option<&str>,
) -> Result<&'static str, Error> {
    let Some(text) = text else {
        return Ok("base_unstored");
    };
    let stored: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM doc_reconciliation_blobs WHERE project_id=?1 AND content_sha256=?2)",
        params![project, sha256],
        |r| r.get(0),
    )?;
    if stored {
        return Ok("stored");
    }
    let used: i64 = db.query_row(
        "SELECT COALESCE(SUM(byte_len),0) FROM doc_reconciliation_blobs WHERE project_id=?1",
        [project],
        |r| r.get(0),
    )?;
    if used as usize + text.len() > MAX_PROJECT_BLOB_BYTES {
        return Ok("base_unstored");
    }
    db.execute(
        "INSERT INTO doc_reconciliation_blobs(project_id,content_sha256,content,byte_len,created_at) VALUES(?1,?2,?3,?4,?5)",
        params![project, sha256, text, text.len(), now()],
    )?;
    Ok("stored")
}

/// Deletes the blobs no baseline references any more.
pub(crate) fn collect_blobs(db: &Connection, project: &str) -> Result<(), Error> {
    db.execute(
        "DELETE FROM doc_reconciliation_blobs WHERE project_id=?1 AND content_sha256 NOT IN (SELECT code_sha256 FROM doc_reconciliation_baselines WHERE project_id=?1 AND base_state='stored')",
        [project],
    )?;
    Ok(())
}

/// The stored base text of a baseline, if its bytes were kept.
pub(crate) fn blob(db: &Connection, project: &str, sha256: &str) -> Result<Option<String>, Error> {
    Ok(db
        .query_row(
            "SELECT content FROM doc_reconciliation_blobs WHERE project_id=?1 AND content_sha256=?2",
            params![project, sha256],
            |r| r.get(0),
        )
        .optional()?)
}

/// Moves one pair's baseline to the file and note as they are now.
pub(crate) fn advance_baseline(
    db: &Connection,
    project: &str,
    root: &Path,
    note: &str,
    code: &str,
    file: &LinkedFile,
    reason: Option<&str>,
) -> Result<(), Error> {
    let (sha, state) = match file {
        LinkedFile::Present { sha256, text } => (
            Some(sha256.as_str()),
            store_blob(db, project, sha256, text.as_deref())?,
        ),
        _ => (None, "absent"),
    };
    db.execute(
        "UPDATE doc_reconciliation_baselines SET code_sha256=?4,base_state=?5,note_sha256=?6,reason=COALESCE(?7,reason),updated_at=?8 WHERE project_id=?1 AND note_path=?2 AND code_path=?3",
        params![project, note, code, sha, state, note_sha256(root, note), reason, now()],
    )?;
    Ok(())
}

/// The files and notes a sync reads, read before its write transaction so the
/// lock the pipeline also needs is held only for the inserts.
#[derive(Debug, Default)]
pub(crate) struct Prefetched {
    pub files: BTreeMap<String, LinkedFile>,
    pub notes: BTreeMap<String, Option<String>>,
}

/// The note -> path pairs that already have a baseline.
pub(crate) fn existing_pairs(
    db: &Connection,
    project: &str,
) -> Result<BTreeSet<(String, String)>, Error> {
    Ok(db
        .prepare(
            "SELECT note_path,code_path FROM doc_reconciliation_baselines WHERE project_id=?1",
        )?
        .query_map([project], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<Result<BTreeSet<_>, _>>()?)
}

/// Reads what the first observation of the new links needs.
pub(crate) fn prefetch_new(
    root: &Path,
    repository: &RepositoryContext,
    links: &[NoteLink],
    existing: &BTreeSet<(String, String)>,
) -> Prefetched {
    let mut prefetched = Prefetched::default();
    for link in links {
        if !link.present || existing.contains(&(link.note_path.clone(), link.code_path.clone())) {
            continue;
        }
        prefetched
            .files
            .entry(link.code_path.clone())
            .or_insert_with(|| read_linked(root, repository, &link.code_path));
        prefetched
            .notes
            .entry(link.note_path.clone())
            .or_insert_with(|| note_sha256(root, &link.note_path));
    }
    prefetched
}

/// Brings the baselines in line with the links: a new link is observed for
/// the first time (no signal), a removed link loses its rows. A link to a
/// missing file is only kept when it was already tracked, so its removal can
/// be reported. Only `prefetched` files are used; nothing is read here.
pub(crate) fn sync_baselines(
    db: &Connection,
    project: &str,
    links: &[NoteLink],
    prefetched: &Prefetched,
) -> Result<(), Error> {
    let existing = existing_pairs(db, project)?;
    let wanted = links
        .iter()
        .map(|link| (link.note_path.clone(), link.code_path.clone()))
        .collect::<BTreeSet<_>>();
    for (note, code) in existing.difference(&wanted) {
        db.execute(
            "DELETE FROM doc_reconciliation_baselines WHERE project_id=?1 AND note_path=?2 AND code_path=?3",
            params![project, note, code],
        )?;
        db.execute(
            "DELETE FROM doc_drift_signals WHERE project_id=?1 AND note_path=?2 AND code_path=?3",
            params![project, note, code],
        )?;
    }
    for link in links {
        if existing.contains(&(link.note_path.clone(), link.code_path.clone())) {
            db.execute(
                "UPDATE doc_reconciliation_baselines SET link_kind=?4 WHERE project_id=?1 AND note_path=?2 AND code_path=?3 AND link_kind<>?4",
                params![project, link.note_path, link.code_path, link.kind.as_str()],
            )?;
            continue;
        }
        if !link.present {
            continue;
        }
        let Some(LinkedFile::Present { sha256, text }) = prefetched.files.get(&link.code_path)
        else {
            continue;
        };
        let state = store_blob(db, project, sha256, text.as_deref())?;
        db.execute(
            "INSERT INTO doc_reconciliation_baselines(project_id,note_path,code_path,link_kind,note_sha256,code_sha256,base_state,state,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,'tracking',?8)",
            params![
                project,
                link.note_path,
                link.code_path,
                link.kind.as_str(),
                prefetched.notes.get(&link.note_path).cloned().flatten(),
                sha256,
                state,
                now()
            ],
        )?;
    }
    collect_blobs(db, project)?;
    Ok(())
}

/// Whether any tracking baseline links `code`: a read, so the many workspace
/// events that touch no linked file never take the write lock.
pub(crate) fn is_tracked(db: &Connection, project: &str, code: &str) -> Result<bool, Error> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM doc_reconciliation_baselines WHERE project_id=?1 AND code_path=?2 AND state='tracking')",
        params![project, code],
        |r| r.get(0),
    )?)
}

/// Compares one file, already read, with every tracking baseline that links
/// it and opens, updates or settles the pair's active signal. Returns how
/// many pairs drift.
pub(crate) fn observe_file(
    db: &Connection,
    project: &str,
    code: &str,
    file: &LinkedFile,
    origin: DriftOrigin,
    pulsation: Option<&str>,
) -> Result<usize, Error> {
    if *file == LinkedFile::Excluded {
        return Ok(0);
    }
    let baselines = db
        .prepare("SELECT note_path,code_sha256 FROM doc_reconciliation_baselines WHERE project_id=?1 AND code_path=?2 AND state='tracking'")?
        .query_map(params![project, code], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let current = file.sha256().map(str::to_owned);
    let mut drifting = 0;
    for (note, base) in baselines {
        if base == current {
            // Back to the reconciled bytes: nothing to update any more.
            db.execute(
                "UPDATE doc_drift_signals SET state='resolved',current_sha256=?4,updated_at=?5 WHERE project_id=?1 AND note_path=?2 AND code_path=?3 AND state='open'",
                params![project, note, code, current, now()],
            )?;
            continue;
        }
        drifting += 1;
        record_signal(
            db,
            project,
            &note,
            code,
            base.as_deref(),
            current.as_deref(),
            origin,
            pulsation,
        )?;
    }
    Ok(drifting)
}

#[allow(clippy::too_many_arguments)]
fn record_signal(
    db: &Connection,
    project: &str,
    note: &str,
    code: &str,
    base: Option<&str>,
    current: Option<&str>,
    origin: DriftOrigin,
    pulsation: Option<&str>,
) -> Result<(), Error> {
    let at = now();
    let pulsation = (origin == DriftOrigin::Pulsation)
        .then_some(pulsation)
        .flatten();
    let updated = db.execute(
        "UPDATE doc_drift_signals SET current_sha256=?4,origin=?5,pulsation_id=?6,updated_at=?7 WHERE project_id=?1 AND note_path=?2 AND code_path=?3 AND state IN ('open','proposed')",
        params![project, note, code, current, origin.as_str(), pulsation, at],
    )?;
    if updated == 0 {
        db.execute(
            "INSERT INTO doc_drift_signals(id,project_id,note_path,code_path,base_sha256,current_sha256,origin,pulsation_id,state,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'open',?9,?9)",
            params![id(), project, note, code, base, current, origin.as_str(), pulsation, at],
        )?;
    }
    Ok(())
}

/// The tracked paths the scan at project open compares.
pub(crate) fn tracked_paths(db: &Connection, project: &str) -> Result<Vec<String>, Error> {
    Ok(db
        .prepare("SELECT DISTINCT code_path FROM doc_reconciliation_baselines WHERE project_id=?1 AND state='tracking' ORDER BY code_path")?
        .query_map([project], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?)
}

/// Records the scan at project open: the files read (at most the limit) out
/// of `total` tracked paths.
pub(crate) fn record_scan(
    db: &Connection,
    project: &str,
    files: &[(String, LinkedFile)],
    total: usize,
) -> Result<(), Error> {
    for (path, file) in files {
        observe_file(db, project, path, file, DriftOrigin::StartupScan, None)?;
    }
    let scanned = files.len();
    db.execute(
        "INSERT INTO doc_reconciliation_scans(project_id,state,scanned,total,scanned_at) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(project_id) DO UPDATE SET state=excluded.state,scanned=excluded.scanned,total=excluded.total,scanned_at=excluded.scanned_at",
        params![
            project,
            if scanned < total { "partial_scan" } else { "complete" },
            scanned,
            total,
            now()
        ],
    )?;
    Ok(())
}

/// Compares the publications of every finished pulsation not handled yet. A
/// pulsation that also published the linked note advances the baseline
/// without a signal; otherwise the pair's signal records the pulsation.
pub(crate) fn process_finished_pulsations(
    db: &Connection,
    project: &str,
    root: &Path,
    repository: &RepositoryContext,
) -> Result<(), Error> {
    let pending = db
        .prepare("SELECT p.id FROM pulsations p WHERE p.project_id=?1 AND p.status='done' AND NOT EXISTS(SELECT 1 FROM doc_reconciliation_pulsations d WHERE d.pulsation_id=p.id) ORDER BY p.created_at,p.id")?
        .query_map([project], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    for pulsation in pending {
        let published = db
            .prepare("SELECT DISTINCT relative_path FROM artifact_publications WHERE pulsation_id=?1 AND state='applied' AND job_class='main'")?
            .query_map([&pulsation], |r| r.get::<_, String>(0))?
            .collect::<Result<BTreeSet<_>, _>>()?;
        let mut by_code: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for code in &published {
            let notes = db
                .prepare("SELECT note_path FROM doc_reconciliation_baselines WHERE project_id=?1 AND code_path=?2 AND state='tracking'")?
                .query_map(params![project, code], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            if !notes.is_empty() {
                by_code.insert(code.clone(), notes);
            }
        }
        for (code, notes) in by_code {
            let file = read_linked(root, repository, &code);
            for note in notes {
                if published.contains(&note) {
                    advance_baseline(db, project, root, &note, &code, &file, None)?;
                    db.execute(
                        "UPDATE doc_drift_signals SET state='resolved',current_sha256=?4,updated_at=?5 WHERE project_id=?1 AND note_path=?2 AND code_path=?3 AND state='open'",
                        params![project, note, code, file.sha256(), now()],
                    )?;
                }
            }
            observe_file(
                db,
                project,
                &code,
                &file,
                DriftOrigin::Pulsation,
                Some(pulsation.as_str()),
            )?;
        }
        db.execute(
            "INSERT INTO doc_reconciliation_pulsations(pulsation_id,project_id,processed_at) VALUES(?1,?2,?3)",
            params![pulsation, project, now()],
        )?;
    }
    collect_blobs(db, project)?;
    Ok(())
}

/// A renamed note keeps its baselines, signals and proposals.
pub(crate) fn rename_note(
    db: &Connection,
    project: &str,
    from: &str,
    to: &str,
) -> Result<(), Error> {
    for table in [
        "doc_reconciliation_baselines",
        "doc_drift_signals",
        "doc_reconciliation_proposals",
    ] {
        db.execute(
            &format!("UPDATE {table} SET note_path=?3 WHERE project_id=?1 AND note_path=?2"),
            params![project, from, to],
        )?;
    }
    Ok(())
}
