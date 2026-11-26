//! Commit queue persistence, done-pulsation backfill (P5), manual skip (P7),
//! git operation history (`git_operations`), and commit receipts for reports.

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;

use super::{
    CommitReceiptV1, GitOperationKind, GitOperationRecordV1, GitOperationStatus,
    PulsationCommitQueueEntryV1, PulsationCommitQueueState, RuleViolation,
    SkipPulsationCommitRequest,
};
use crate::persistence_service::{PersistenceService, PersistenceServiceError};

fn now_utc() -> String {
    chrono::Utc::now().to_rfc3339()
}

pub(crate) fn is_git_tool_enabled_in_conn(
    db: &Connection,
    project_id: &str,
) -> Result<bool, PersistenceServiceError> {
    let enabled: Option<i64> = db
        .query_row(
            "SELECT enabled FROM project_git_rules WHERE project_id=?1 ORDER BY version DESC LIMIT 1",
            [project_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(enabled == Some(1))
}

/// Records `pulsation_id` as `pending` in `pulsation_commit_state` when the git tool
/// is enabled for `project_id` (Feature 08 §Banco de dados / C6 / G8).
pub(crate) fn enqueue_done_pulsation_in_tx(
    db: &Connection,
    project_id: &str,
    pulsation_id: &str,
    fallback_completed_at: &str,
) -> Result<(), PersistenceServiceError> {
    if !is_git_tool_enabled_in_conn(db, project_id)? {
        return Ok(());
    }
    let completed_at: String = db
        .query_row(
            "SELECT updated_at FROM pulsations WHERE id=?1",
            [pulsation_id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or_else(|| fallback_completed_at.to_owned());
    let now = now_utc();
    db.execute(
        "INSERT OR IGNORE INTO pulsation_commit_state(
           pulsation_id, project_id, completed_at, state, skip_reason, branch, head_commit, updated_at
         ) VALUES (?1, ?2, ?3, 'pending', NULL, NULL, NULL, ?4)",
        params![pulsation_id, project_id, completed_at, now],
    )?;
    Ok(())
}

/// Enqueues all finished (`status = 'done'`) pulsations for `project_id` in
/// completion order that do not yet have a `pulsation_commit_state` row (P5).
pub(crate) fn backfill_done_pulsations_in_tx(
    db: &Connection,
    project_id: &str,
) -> Result<usize, PersistenceServiceError> {
    if !is_git_tool_enabled_in_conn(db, project_id)? {
        return Ok(0);
    }
    let mut stmt = db.prepare(
        "SELECT id, updated_at
           FROM pulsations
          WHERE project_id=?1 AND status='done'
            AND id NOT IN (SELECT pulsation_id FROM pulsation_commit_state)
          ORDER BY updated_at ASC, rowid ASC",
    )?;
    let missing = stmt
        .query_map([project_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let now = now_utc();
    let count = missing.len();
    for (pulsation_id, completed_at) in missing {
        db.execute(
            "INSERT OR IGNORE INTO pulsation_commit_state(
               pulsation_id, project_id, completed_at, state, skip_reason, branch, head_commit, updated_at
             ) VALUES (?1, ?2, ?3, 'pending', NULL, NULL, NULL, ?4)",
            params![pulsation_id, project_id, completed_at, now],
        )?;
    }
    Ok(count)
}

pub(crate) fn begin_git_operation(
    db: &Connection,
    project_id: &str,
    kind: GitOperationKind,
    pulsation_id: Option<&str>,
    rules_version: i64,
    rules_sha256: &str,
) -> Result<String, PersistenceServiceError> {
    let op_id = Uuid::new_v4().to_string();
    let started_at = now_utc();
    db.execute(
        "INSERT INTO git_operations(
           id, project_id, kind, pulsation_id, rules_version, rules_sha256, status, started_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'running', ?7)",
        params![
            op_id,
            project_id,
            kind.as_str(),
            pulsation_id,
            rules_version,
            rules_sha256,
            started_at
        ],
    )?;
    Ok(op_id)
}

pub(crate) fn finish_git_operation(
    db: &Connection,
    op_id: &str,
    status: GitOperationStatus,
    receipt_json: Option<&str>,
    violations: &[RuleViolation],
) -> Result<(), PersistenceServiceError> {
    let finished_at = now_utc();
    let violations_json = if violations.is_empty() {
        None
    } else {
        Some(serde_json::to_string(violations).map_err(|error| {
            PersistenceServiceError::Runtime(format!("Failed to serialize violations: {error}"))
        })?)
    };
    db.execute(
        "UPDATE git_operations
            SET status=?2, receipt_json=?3, violations_json=?4, finished_at=?5
          WHERE id=?1",
        params![
            op_id,
            status.as_str(),
            receipt_json,
            violations_json,
            finished_at
        ],
    )?;
    Ok(())
}

pub(crate) fn latest_commit_receipt_for_pulsation(
    db: &Connection,
    pulsation_id: &str,
) -> Result<Option<CommitReceiptV1>, PersistenceServiceError> {
    let receipt_json: Option<String> = db
        .query_row(
            "SELECT receipt_json
               FROM git_operations
              WHERE pulsation_id=?1 AND kind='commit' AND status='succeeded' AND receipt_json IS NOT NULL
              ORDER BY finished_at DESC, rowid DESC
              LIMIT 1",
            [pulsation_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(json) = receipt_json else {
        return Ok(None);
    };
    let receipt: CommitReceiptV1 = serde_json::from_str(&json).map_err(|error| {
        PersistenceServiceError::Runtime(format!("Stored CommitReceiptV1 is invalid: {error}"))
    })?;
    Ok(Some(receipt))
}

impl PersistenceService {
    /// Lists the project's pulsation commit queue in completion order, automatically
    /// backfilling any finished `Done` pulsations when the git tool is enabled (P5).
    pub fn list_pulsation_commit_queue(
        &self,
        project_id: &str,
    ) -> Result<Vec<PulsationCommitQueueEntryV1>, PersistenceServiceError> {
        let project_id = project_id.trim().to_owned();
        if project_id.is_empty() {
            return Err(PersistenceServiceError::Validation(
                "Project id cannot be empty.".into(),
            ));
        }
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        backfill_done_pulsations_in_tx(&tx, &project_id)?;
        tx.commit()?;

        let mut stmt = db.prepare(
            "SELECT q.pulsation_id, p.feature_name, q.completed_at, q.state, q.skip_reason,
                    q.branch, q.head_commit, q.updated_at
               FROM pulsation_commit_state q
               JOIN pulsations p ON p.id=q.pulsation_id
              WHERE q.project_id=?1
              ORDER BY q.completed_at ASC, q.pulsation_id ASC",
        )?;
        let rows = stmt
            .query_map([&project_id], |row| {
                let state_str: String = row.get(3)?;
                let state = PulsationCommitQueueState::parse(&state_str).ok_or_else(|| {
                    rusqlite::Error::FromSqlConversionFailure(
                        3,
                        rusqlite::types::Type::Text,
                        format!("Unknown commit queue state: {state_str}").into(),
                    )
                })?;
                Ok(PulsationCommitQueueEntryV1 {
                    pulsation_id: row.get(0)?,
                    project_id: project_id.clone(),
                    feature_name: row.get(1)?,
                    completed_at: row.get(2)?,
                    state,
                    skip_reason: row.get(4)?,
                    branch: row.get(5)?,
                    head_commit: row.get(6)?,
                    updated_at: row.get(7)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Marks a pending pulsation in the commit queue as `skipped` with an explicit reason (P7).
    pub fn skip_pulsation_commit(
        &self,
        request: SkipPulsationCommitRequest,
    ) -> Result<PulsationCommitQueueEntryV1, PersistenceServiceError> {
        let project_id = request.project_id.trim().to_owned();
        let pulsation_id = request.pulsation_id.trim().to_owned();
        let reason = request.reason.trim().to_owned();
        if project_id.is_empty() || pulsation_id.is_empty() {
            return Err(PersistenceServiceError::Validation(
                "Project id and pulsation id are required.".into(),
            ));
        }
        if reason.is_empty() || reason.chars().count() > 500 {
            return Err(PersistenceServiceError::Validation(
                "Skip reason must be between 1 and 500 characters.".into(),
            ));
        }
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        backfill_done_pulsations_in_tx(&tx, &project_id)?;
        let current_state: Option<String> = tx
            .query_row(
                "SELECT state FROM pulsation_commit_state WHERE project_id=?1 AND pulsation_id=?2",
                params![project_id, pulsation_id],
                |row| row.get(0),
            )
            .optional()?;
        match current_state.as_deref() {
            None => {
                return Err(PersistenceServiceError::NotFound(
                    "Pulsation is not in the project commit queue.".into(),
                ));
            }
            Some("committed") => {
                return Err(PersistenceServiceError::Validation(
                    "Cannot skip a pulsation that is already committed.".into(),
                ));
            }
            Some(_) => {}
        }
        let now = now_utc();
        tx.execute(
            "UPDATE pulsation_commit_state
                SET state='skipped', skip_reason=?3, updated_at=?4
              WHERE project_id=?1 AND pulsation_id=?2",
            params![project_id, pulsation_id, reason, now],
        )?;
        tx.commit()?;

        let entry = db.query_row(
            "SELECT q.pulsation_id, p.feature_name, q.completed_at, q.state, q.skip_reason,
                    q.branch, q.head_commit, q.updated_at
               FROM pulsation_commit_state q
               JOIN pulsations p ON p.id=q.pulsation_id
              WHERE q.project_id=?1 AND q.pulsation_id=?2",
            params![project_id, pulsation_id],
            |row| {
                Ok(PulsationCommitQueueEntryV1 {
                    pulsation_id: row.get(0)?,
                    project_id: project_id.clone(),
                    feature_name: row.get(1)?,
                    completed_at: row.get(2)?,
                    state: PulsationCommitQueueState::Skipped,
                    skip_reason: row.get(4)?,
                    branch: row.get(5)?,
                    head_commit: row.get(6)?,
                    updated_at: row.get(7)?,
                })
            },
        )?;
        Ok(entry)
    }

    /// Lists recorded git operations for `project_id`, newest first.
    pub fn list_git_operations(
        &self,
        project_id: &str,
        limit: usize,
    ) -> Result<Vec<GitOperationRecordV1>, PersistenceServiceError> {
        let project_id = project_id.trim().to_owned();
        if project_id.is_empty() {
            return Err(PersistenceServiceError::Validation(
                "Project id cannot be empty.".into(),
            ));
        }
        let db = self.connection()?;
        let capped = (limit.clamp(1, 200)) as i64;
        let mut stmt = db.prepare(
            "SELECT id, project_id, kind, pulsation_id, rules_version, rules_sha256,
                    status, receipt_json, violations_json, started_at, finished_at
               FROM git_operations
              WHERE project_id=?1
              ORDER BY started_at DESC, rowid DESC
              LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![project_id, capped], |row| {
                let kind_str: String = row.get(2)?;
                let status_str: String = row.get(6)?;
                let violations_json: Option<String> = row.get(8)?;
                let kind = GitOperationKind::parse(&kind_str).ok_or_else(|| {
                    rusqlite::Error::FromSqlConversionFailure(
                        2,
                        rusqlite::types::Type::Text,
                        format!("Unknown git operation kind: {kind_str}").into(),
                    )
                })?;
                let status = GitOperationStatus::parse(&status_str).ok_or_else(|| {
                    rusqlite::Error::FromSqlConversionFailure(
                        6,
                        rusqlite::types::Type::Text,
                        format!("Unknown git operation status: {status_str}").into(),
                    )
                })?;
                let violations: Vec<RuleViolation> = violations_json
                    .as_deref()
                    .and_then(|json| serde_json::from_str(json).ok())
                    .unwrap_or_default();
                Ok(GitOperationRecordV1 {
                    id: row.get(0)?,
                    project_id: row.get(1)?,
                    kind,
                    pulsation_id: row.get(3)?,
                    rules_version: row.get(4)?,
                    rules_sha256: row.get(5)?,
                    status,
                    receipt_json: row.get(7)?,
                    violations,
                    started_at: row.get(9)?,
                    finished_at: row.get(10)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}
