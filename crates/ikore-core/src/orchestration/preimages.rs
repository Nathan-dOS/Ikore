//! Reticle P1 with PI-Desktop PD6: what each publication replaced, kept after
//! the pulsation ends, so a run's changes can be compared with what was there
//! and, later (Reticle P2), undone.
//!
//! A replaced text is stored once per project, by hash, up to the publication
//! limit; the project keeps at most 64 MiB of them. A text that does not fit
//! is recorded by hash with the reason, never silently dropped.

use rusqlite::{Connection, Transaction, params};

use super::serial::PublicationBase;
use crate::persistence_service::{PersistenceService, PersistenceServiceError};

#[cfg(test)]
#[path = "preimages_tests.rs"]
mod tests;

/// A preimage is kept up to the publication limit (Reticle decision D3).
pub(crate) const MAX_PREIMAGE_BYTES: usize = 1024 * 1024;

/// What one project keeps at most, the documentation reconciliation's budget (V54).
const PROJECT_BUDGET_BYTES: i64 = 64 * 1024 * 1024;

/// One applied publication and what it replaced.
pub(super) struct Replaced<'a> {
    pub(super) project_id: &'a str,
    pub(super) pulsation_id: &'a str,
    pub(super) artifact_id: &'a str,
    pub(super) relative_path: &'a str,
    /// The path as observed before the replacement.
    pub(super) capture: &'a PublicationBase,
}

/// Records what an applied publication replaced, in its transaction. A replay
/// keeps the first record: by then the replaced bytes are gone.
pub(super) fn record(
    tx: &Transaction<'_>,
    replaced: &Replaced<'_>,
    now: &str,
) -> Result<(), PersistenceServiceError> {
    record_within(tx, replaced, now, PROJECT_BUDGET_BYTES)
}

fn record_within(
    tx: &Transaction<'_>,
    replaced: &Replaced<'_>,
    now: &str,
    budget: i64,
) -> Result<(), PersistenceServiceError> {
    let capture = replaced.capture;
    let (state, reason) = match (&capture.content, &capture.sha256) {
        (Some(content), Some(sha256)) => {
            if store_within(tx, replaced.project_id, sha256, content, now, budget)? {
                ("present", None)
            } else {
                ("present_unstored", Some("over_budget"))
            }
        }
        _ => (capture.state, capture.reason),
    };
    tx.execute(
        "INSERT OR IGNORE INTO publication_preimages(artifact_id,project_id,pulsation_id,relative_path,state,content_sha256,byte_len,reason,captured_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![
            replaced.artifact_id,
            replaced.project_id,
            replaced.pulsation_id,
            replaced.relative_path,
            state,
            capture.sha256,
            capture.byte_len,
            reason,
            now
        ],
    )?;
    Ok(())
}

/// Keeps `content` once per project; `false` when it does not fit `budget`
/// even after the texts no publication refers to any more are dropped.
fn store_within(
    tx: &Transaction<'_>,
    project_id: &str,
    sha256: &str,
    content: &str,
    now: &str,
    budget: i64,
) -> Result<bool, PersistenceServiceError> {
    let stored: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM publication_preimage_blobs WHERE project_id=?1 AND content_sha256=?2)",
        params![project_id, sha256],
        |row| row.get(0),
    )?;
    if stored {
        return Ok(true);
    }
    let len = i64::try_from(content.len()).unwrap_or(i64::MAX);
    if content.len() > MAX_PREIMAGE_BYTES {
        return Ok(false);
    }
    if used(tx, project_id)? + len > budget {
        tx.execute(
            "DELETE FROM publication_preimage_blobs WHERE project_id=?1 AND NOT EXISTS(SELECT 1 FROM publication_preimages p WHERE p.project_id=publication_preimage_blobs.project_id AND p.content_sha256=publication_preimage_blobs.content_sha256 AND p.state='present')",
            [project_id],
        )?;
        if used(tx, project_id)? + len > budget {
            return Ok(false);
        }
    }
    tx.execute(
        "INSERT INTO publication_preimage_blobs(project_id,content_sha256,content,byte_len,created_at) VALUES(?1,?2,?3,?4,?5)",
        params![project_id, sha256, content, len, now],
    )?;
    Ok(true)
}

fn used(tx: &Transaction<'_>, project_id: &str) -> Result<i64, PersistenceServiceError> {
    Ok(tx.query_row(
        "SELECT COALESCE(SUM(byte_len),0) FROM publication_preimage_blobs WHERE project_id=?1",
        [project_id],
        |row| row.get(0),
    )?)
}

/// What a published file replaced, as a run's changes show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublishedBefore {
    /// The path did not exist.
    Absent,
    /// The replaced text.
    Text(String),
    /// The path held a text the host did not keep: `too_large`, `replayed`,
    /// `unreadable`, `over_budget`, `purged` or `not_recorded` (published
    /// before preimages existed).
    Unavailable(String),
    /// The publication is not applied (pending, conflict or cancelled).
    NotApplied,
}

/// One file a job published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedChange {
    pub artifact_id: String,
    pub relative_path: String,
    /// `pending`, `applying`, `applied`, `conflict` or `cancelled`.
    pub publication_state: String,
    pub before: PublishedBefore,
    /// The published text.
    pub after: String,
}

impl PersistenceService {
    /// The files `job_id` published in `project_id`, by path (PD6).
    pub fn job_changes(
        &self,
        project_id: &str,
        job_id: &str,
    ) -> Result<Vec<PublishedChange>, PersistenceServiceError> {
        changes(
            &self.connection()?,
            "a.producer_job_id=?2",
            project_id,
            job_id,
        )
    }

    /// One published file of `project_id` (PD6).
    pub fn published_change(
        &self,
        project_id: &str,
        artifact_id: &str,
    ) -> Result<Option<PublishedChange>, PersistenceServiceError> {
        Ok(
            changes(&self.connection()?, "a.id=?2", project_id, artifact_id)?
                .into_iter()
                .next(),
        )
    }
}

fn changes(
    db: &Connection,
    filter: &str,
    project_id: &str,
    key: &str,
) -> Result<Vec<PublishedChange>, PersistenceServiceError> {
    let sql = format!(
        "SELECT a.id,o.relative_path,o.state,COALESCE(a.content,''),i.state,i.reason,b.content
         FROM artifact_publications o
         JOIN artifacts a ON a.id=o.artifact_id
         LEFT JOIN publication_preimages i ON i.artifact_id=o.artifact_id
         LEFT JOIN publication_preimage_blobs b
           ON b.project_id=i.project_id AND b.content_sha256=i.content_sha256 AND i.state='present'
         WHERE o.project_id=?1 AND {filter}
         ORDER BY o.relative_path,a.id"
    );
    let mut statement = db.prepare(&sql)?;
    let rows = statement
        .query_map(params![project_id, key], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        .map(
            |(artifact_id, relative_path, publication_state, after, state, reason, text)| {
                let before = match (state.as_deref(), reason, text) {
                    (Some("absent"), _, _) => PublishedBefore::Absent,
                    (Some("present"), _, Some(text)) => PublishedBefore::Text(text),
                    (Some("present"), _, None) => PublishedBefore::Unavailable("purged".into()),
                    (Some(_), Some(reason), _) => PublishedBefore::Unavailable(reason),
                    (Some(_), None, _) => PublishedBefore::Unavailable("unreadable".into()),
                    (None, _, _) if publication_state == "applied" => {
                        PublishedBefore::Unavailable("not_recorded".into())
                    }
                    (None, _, _) => PublishedBefore::NotApplied,
                };
                PublishedChange {
                    artifact_id,
                    relative_path,
                    publication_state,
                    before,
                    after,
                }
            },
        )
        .collect())
}
