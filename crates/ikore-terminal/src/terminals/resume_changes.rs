//! External changes on resume (ADR-037 M3): the per-file manifest a
//! resumable run keeps, and what a later resume of its session is told.
//!
//! A run started by the developer records the manifest of the snapshot it
//! ran on (path and original digest per file) and the sanitized content of
//! its small text files. Resuming its session compares that manifest with a
//! fresh snapshot: nothing changed resumes as before; changes within the
//! budget become a host notice placed before the developer's message, and
//! so pass through the same sanitization; more changes refuse the resume.

use super::{
    Arc, CliBackend, Connection, Digest, HashMap, MAX_SAFE_SNAPSHOT_BYTES, MAX_SAFE_SNAPSHOT_FILES,
    OptionalExtension, Path, SANITIZATION_POLICY_VERSION, Sha256, TerminalError,
    TerminalRepository, TerminalRun, TerminalService, Utc, lower_hex, params, required,
};
use crate::host::SnapshotPreparation;
use ikore_contracts::external_changes::{
    ExternalChangesV1, MAX_HUNK_FILE_BYTES, ManifestEntry, canonical_change_list, diff_manifests,
    within_budget,
};
use ikore_contracts::line_diff::{self, LineDiff};

/// Sanitized content one run keeps for hunks, at most (D9).
pub const MAX_RUN_BLOB_BYTES: usize = 1024 * 1024;
/// Hunk text per changed file, at most, before the notice budget applies.
const MAX_HUNK_CHARS: usize = 4_000;

/// The session fingerprint a run records (version 2): the sanitization
/// policy version, project, slot, backend and model, and no workspace hash,
/// so a changed workspace no longer breaks the session by itself.
pub(super) fn session_fingerprint_v2(run: &TerminalRun) -> String {
    let mut digest = Sha256::new();
    for part in [
        "session-fingerprint-v2",
        SANITIZATION_POLICY_VERSION,
        run.project_id.as_str(),
        run.slot_id.as_str(),
        run.backend.map(CliBackend::as_str).unwrap_or_default(),
        run.model.as_deref().unwrap_or_default(),
    ] {
        digest.update(part.as_bytes());
        digest.update([0]);
    }
    lower_hex(&digest.finalize())
}

/// The manifest of a prepared snapshot.
pub(super) fn manifest_of(snapshot: &SnapshotPreparation) -> Vec<ManifestEntry> {
    snapshot
        .files
        .iter()
        .map(|file| ManifestEntry {
            path: file.relative_path.to_string_lossy().into_owned(),
            original_sha256: lower_hex(&file.original_sha256),
        })
        .collect()
}

/// The digest that binds a resume's notice to the snapshot it starts on.
pub(super) fn changes_digest(changes: &ExternalChangesV1) -> String {
    lower_hex(&Sha256::digest(
        canonical_change_list(&changes.entries).as_bytes(),
    ))
}

/// Records the manifest of a resumable run's snapshot and the sanitized
/// content of its text files up to 32 KiB, at most 1 MiB per run. Blobs are
/// shared within the project.
pub(super) fn record_snapshot_manifest(
    db_path: &Path,
    run: &TerminalRun,
    snapshot: &SnapshotPreparation,
) -> Result<(), TerminalError> {
    let mut db = Connection::open(db_path)?;
    db.pragma_update(None, "foreign_keys", "ON")?;
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    let transaction = db.transaction()?;
    let now = Utc::now().to_rfc3339();
    let mut budget = MAX_RUN_BLOB_BYTES;
    for file in &snapshot.files {
        let projected = file.projected_content.as_bytes();
        let projected_sha256 = lower_hex(&Sha256::digest(projected));
        let blob =
            (projected.len() <= MAX_HUNK_FILE_BYTES && projected.len() <= budget).then(|| {
                budget -= projected.len();
                projected_sha256.clone()
            });
        if let Some(sha) = &blob {
            transaction.execute(
                "INSERT OR IGNORE INTO terminal_snapshot_blobs(project_id,sha256,content,byte_len,created_at)
                 VALUES(?1,?2,?3,?4,?5)",
                params![run.project_id, sha, file.projected_content, projected.len(), now],
            )?;
        }
        transaction.execute(
            "INSERT INTO terminal_run_snapshot_files(run_id,relative_path,original_sha256,projected_sha256,byte_len,blob_sha256)
             VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(run_id,relative_path) DO NOTHING",
            params![
                run.id,
                file.relative_path.to_string_lossy(),
                lower_hex(&file.original_sha256),
                projected_sha256,
                projected.len(),
                blob
            ],
        )?;
    }
    transaction.commit()?;
    Ok(())
}

impl TerminalRepository {
    /// The manifest a run recorded, with the blob of each file kept for hunks.
    pub(super) fn snapshot_manifest(
        &self,
        run_id: &str,
    ) -> Result<Vec<(ManifestEntry, Option<String>)>, TerminalError> {
        let db = self.connection()?;
        let mut statement = db.prepare(
            "SELECT relative_path,original_sha256,blob_sha256 FROM terminal_run_snapshot_files
             WHERE run_id=?1 ORDER BY relative_path",
        )?;
        let rows = statement.query_map([run_id], |row| {
            Ok((
                ManifestEntry {
                    path: row.get(0)?,
                    original_sha256: row.get(1)?,
                },
                row.get(2)?,
            ))
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    fn snapshot_blob(
        &self,
        project_id: &str,
        sha256: &str,
    ) -> Result<Option<String>, TerminalError> {
        Ok(self
            .connection()?
            .query_row(
                "SELECT content FROM terminal_snapshot_blobs WHERE project_id=?1 AND sha256=?2",
                params![project_id, sha256],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// 1 for runs recorded before V63, whose sessions resume only on an
    /// unchanged workspace; 2 for runs that carry a manifest.
    pub fn session_fingerprint_version(&self, run_id: &str) -> Result<u8, TerminalError> {
        Ok(self.connection()?.query_row(
            "SELECT COALESCE((SELECT session_fingerprint_version FROM terminal_run_resume_state
                              WHERE run_id=?1),1)",
            [required(run_id, "runId")?],
            |row| row.get(0),
        )?)
    }

    pub(super) fn set_session_fingerprint_version(
        &self,
        run_id: &str,
        version: u8,
    ) -> Result<(), TerminalError> {
        self.connection()?.execute(
            "INSERT INTO terminal_run_resume_state(run_id,session_fingerprint_version) VALUES(?1,?2)
             ON CONFLICT(run_id) DO UPDATE SET session_fingerprint_version=excluded.session_fingerprint_version",
            params![run_id, version],
        )?;
        Ok(())
    }

    pub(super) fn set_external_changes_digest(
        &self,
        run_id: &str,
        digest: &str,
    ) -> Result<(), TerminalError> {
        self.connection()?.execute(
            "INSERT INTO terminal_run_resume_state(run_id,session_fingerprint_version,external_changes_sha256)
             VALUES(?1,1,?2)
             ON CONFLICT(run_id) DO UPDATE SET external_changes_sha256=excluded.external_changes_sha256",
            params![run_id, digest],
        )?;
        Ok(())
    }

    pub(super) fn external_changes_digest(
        &self,
        run_id: &str,
    ) -> Result<Option<String>, TerminalError> {
        Ok(self
            .connection()?
            .query_row(
                "SELECT external_changes_sha256 FROM terminal_run_resume_state WHERE run_id=?1",
                [run_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }

    /// Removes the project's blobs no manifest points at any more.
    pub fn delete_orphan_snapshot_blobs(&self, project_id: &str) -> Result<usize, TerminalError> {
        Ok(delete_orphan_blobs(&self.connection()?, project_id)?)
    }
}

/// Removes the project's blobs no manifest points at any more, in the
/// caller's transaction: every deletion of runs calls it, so blobs go with
/// the last run that kept them.
pub(super) fn delete_orphan_blobs(db: &Connection, project_id: &str) -> rusqlite::Result<usize> {
    db.execute(
        "DELETE FROM terminal_snapshot_blobs WHERE project_id=?1 AND NOT EXISTS(
           SELECT 1 FROM terminal_run_snapshot_files f JOIN terminal_runs r ON r.id=f.run_id
           WHERE r.project_id=?1 AND f.blob_sha256=terminal_snapshot_blobs.sha256)",
        [project_id],
    )
}

/// What resuming a session would tell the agent, for the resume dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumePreview {
    /// Recorded before change notices: any change refuses the resume.
    Strict,
    Changes(ExternalChangesV1),
}

impl TerminalService {
    /// The changes since `source` ran, or `None` for a session without a
    /// manifest. Prepares a fresh snapshot with the source run's policy.
    async fn external_changes_since(
        self: &Arc<Self>,
        source: &TerminalRun,
        workspace: &Path,
    ) -> Result<Option<ExternalChangesV1>, TerminalError> {
        if self.repository.session_fingerprint_version(&source.id)? < 2 {
            return Ok(None);
        }
        let host = self.host.clone();
        let repository = self.repository.clone();
        let db_path = self.repository.db_path.clone();
        let source = source.clone();
        let workspace = workspace.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let snapshot = host.prepare_snapshot(
                &db_path,
                &workspace,
                &source,
                MAX_SAFE_SNAPSHOT_FILES,
                MAX_SAFE_SNAPSHOT_BYTES,
            )?;
            external_changes(&repository, &source, &snapshot).map(Some)
        })
        .await
        .map_err(|error| {
            TerminalError::Security(format!("the change scan was interrupted: {error}"))
        })?
    }

    /// What this resume tells the agent, and the digest that binds it to
    /// the snapshot the session starts on; refused past the budget.
    pub(super) async fn resume_notice(
        self: &Arc<Self>,
        source: &TerminalRun,
        workspace: &Path,
    ) -> Result<(Option<String>, Option<String>), TerminalError> {
        let Some(changes) = self.external_changes_since(source, workspace).await? else {
            return Ok((None, None));
        };
        if changes.decision == ikore_contracts::external_changes::ExternalChangesDecision::Refused {
            return Err(TerminalError::Validation(
                ikore_contracts::external_changes::refusal_message(&changes),
            ));
        }
        Ok((
            ikore_contracts::external_changes::render_notice(&changes),
            Some(changes_digest(&changes)),
        ))
    }

    /// What resuming `source_run_id` would tell the agent (ADR-037 M3).
    pub async fn preview_resume(
        self: &Arc<Self>,
        project_id: &str,
        source_run_id: &str,
    ) -> Result<ResumePreview, TerminalError> {
        let source = self.repository.get_run(source_run_id)?;
        if source.project_id != project_id {
            return Err(TerminalError::Validation(
                "the chosen session belongs to another project".into(),
            ));
        }
        let workspace = self.repository.project_workspace(project_id)?;
        Ok(
            match self.external_changes_since(&source, &workspace).await? {
                None => ResumePreview::Strict,
                Some(changes) => ResumePreview::Changes(changes),
            },
        )
    }
}

/// What changed between the manifest `parent` recorded and `snapshot`, with
/// hunks of the sanitized content for small text files, within budget.
pub(super) fn external_changes(
    repository: &TerminalRepository,
    parent: &TerminalRun,
    snapshot: &SnapshotPreparation,
) -> Result<ExternalChangesV1, TerminalError> {
    let recorded = repository.snapshot_manifest(&parent.id)?;
    let parent_manifest: Vec<ManifestEntry> =
        recorded.iter().map(|(entry, _)| entry.clone()).collect();
    let blobs: HashMap<&str, &str> = recorded
        .iter()
        .filter_map(|(entry, blob)| blob.as_deref().map(|blob| (entry.path.as_str(), blob)))
        .collect();
    let current: HashMap<String, &str> = snapshot
        .files
        .iter()
        .map(|file| {
            (
                file.relative_path.to_string_lossy().into_owned(),
                file.projected_content.as_str(),
            )
        })
        .collect();
    let mut entries = diff_manifests(&parent_manifest, &manifest_of(snapshot));
    for entry in &mut entries {
        if entry.change != ikore_contracts::external_changes::ChangeKind::Modified {
            continue;
        }
        let (Some(blob), Some(now)) = (blobs.get(entry.path.as_str()), current.get(&entry.path))
        else {
            continue;
        };
        if now.len() > MAX_HUNK_FILE_BYTES {
            continue;
        }
        let Some(before) = repository.snapshot_blob(&parent.project_id, blob)? else {
            continue;
        };
        if let LineDiff::Unified { text, .. } = line_diff::unified(&before, now, MAX_HUNK_CHARS) {
            let (added, removed) = text.lines().fold((0u32, 0u32), |(added, removed), line| {
                if line.starts_with('+') && !line.starts_with("+++") {
                    (added + 1, removed)
                } else if line.starts_with('-') && !line.starts_with("---") {
                    (added, removed + 1)
                } else {
                    (added, removed)
                }
            });
            entry.added_lines = Some(added);
            entry.removed_lines = Some(removed);
            entry.hunks = Some(text.trim_end().to_owned());
        }
    }
    Ok(within_budget(&parent.id, entries))
}

#[cfg(test)]
#[path = "resume_changes_tests.rs"]
mod tests;
