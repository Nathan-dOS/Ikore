//! Feature 08 M1: commit rules validation (including regex compilation),
//! canonical SHA-256 digest computation, and versioned SQLite persistence.

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};

use super::{
    CommitRulesV1, ProjectGitRulesRecord, RuleFieldError, SaveProjectGitRulesOutcome,
    SaveProjectGitRulesRequest, validate_commit_rules_structure,
};
use crate::persistence_service::{PersistenceService, PersistenceServiceError};

fn now_utc() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Validates every field of `CommitRulesV1`, including compiling a non-empty
/// regex in `message.pattern`.
pub fn validate_commit_rules(rules: &CommitRulesV1) -> Vec<RuleFieldError> {
    let mut errors = validate_commit_rules_structure(rules);
    let trimmed_pattern = rules.message.pattern.trim();
    if !trimmed_pattern.is_empty()
        && let Err(error) = regex::Regex::new(trimmed_pattern)
    {
        let detail = error.to_string().replace('`', "\"");
        errors.push(RuleFieldError::new(
            "message.pattern",
            "invalid_regex",
            format!("Message pattern is not a valid regular expression: {detail}"),
        ));
    }
    errors
}

/// Normalizes string lists/fields minimally without changing user semantics and
/// produces deterministic JSON plus its lowercase hex SHA-256 digest.
pub fn canonical_rules_json_and_sha256(
    rules: &CommitRulesV1,
) -> Result<(String, String), PersistenceServiceError> {
    let json = serde_json::to_string(rules).map_err(|error| {
        PersistenceServiceError::Validation(format!("Failed to serialize git rules: {error}"))
    })?;
    let mut hasher = Sha256::new();
    hasher.update(json.as_bytes());
    let digest: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok((json, digest))
}

fn ensure_active_project(
    conn: &Connection,
    project_id: &str,
) -> Result<(), PersistenceServiceError> {
    let trimmed = project_id.trim();
    if trimmed.is_empty() {
        return Err(PersistenceServiceError::Validation(
            "Project id cannot be empty.".into(),
        ));
    }
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1 AND is_archived=0)",
        [trimmed],
        |row| row.get(0),
    )?;
    if exists {
        Ok(())
    } else {
        Err(PersistenceServiceError::NotFound("project".into()))
    }
}

impl PersistenceService {
    /// Returns the latest saved git rules for `project_id`, or an initial
    /// version-0 disabled default record when none have been saved yet.
    pub fn get_project_git_rules(
        &self,
        project_id: &str,
    ) -> Result<ProjectGitRulesRecord, PersistenceServiceError> {
        let project_id = project_id.trim().to_owned();
        let db = self.connection()?;
        ensure_active_project(&db, &project_id)?;

        let row: Option<(i64, i64, String, String, String)> = db
            .query_row(
                "SELECT version, enabled, rules_json, rules_sha256, created_at
                   FROM project_git_rules
                  WHERE project_id=?1
                  ORDER BY version DESC
                  LIMIT 1",
                [&project_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()?;

        if let Some((version, enabled, rules_json, rules_sha256, created_at)) = row {
            let rules: CommitRulesV1 = serde_json::from_str(&rules_json).map_err(|error| {
                PersistenceServiceError::Validation(format!(
                    "Stored git rules JSON is invalid: {error}"
                ))
            })?;
            return Ok(ProjectGitRulesRecord {
                project_id,
                version,
                enabled: enabled != 0,
                rules,
                rules_sha256,
                created_at,
            });
        }

        let defaults = CommitRulesV1::default();
        let (_, rules_sha256) = canonical_rules_json_and_sha256(&defaults)?;
        Ok(ProjectGitRulesRecord {
            project_id,
            version: 0,
            enabled: false,
            rules: defaults,
            rules_sha256,
            created_at: String::new(),
        })
    }

    /// Lists all saved rule versions for `project_id`, newest first.
    pub fn list_project_git_rule_versions(
        &self,
        project_id: &str,
    ) -> Result<Vec<ProjectGitRulesRecord>, PersistenceServiceError> {
        let project_id = project_id.trim().to_owned();
        let db = self.connection()?;
        ensure_active_project(&db, &project_id)?;
        let mut stmt = db.prepare(
            "SELECT version, enabled, rules_json, rules_sha256, created_at
               FROM project_git_rules
              WHERE project_id=?1
              ORDER BY version DESC",
        )?;
        let records = stmt
            .query_map([&project_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(|(version, enabled, rules_json, rules_sha256, created_at)| {
                let rules: CommitRulesV1 = serde_json::from_str(&rules_json).map_err(|error| {
                    PersistenceServiceError::Validation(format!(
                        "Stored git rules JSON is invalid: {error}"
                    ))
                })?;
                Ok(ProjectGitRulesRecord {
                    project_id: project_id.clone(),
                    version,
                    enabled: enabled != 0,
                    rules,
                    rules_sha256,
                    created_at,
                })
            })
            .collect::<Result<Vec<_>, PersistenceServiceError>>()?;
        Ok(records)
    }

    /// Validates and persists a new version in `project_git_rules`.
    /// Invalid field values return `SaveProjectGitRulesOutcome::Invalid` without writing.
    pub fn save_project_git_rules(
        &self,
        request: SaveProjectGitRulesRequest,
    ) -> Result<SaveProjectGitRulesOutcome, PersistenceServiceError> {
        let project_id = request.project_id.trim().to_owned();
        let errors = validate_commit_rules(&request.rules);
        if !errors.is_empty() {
            return Ok(SaveProjectGitRulesOutcome::Invalid(errors));
        }

        let (rules_json, rules_sha256) = canonical_rules_json_and_sha256(&request.rules)?;
        let created_at = now_utc();
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_active_project(&tx, &project_id)?;
        let next_version: i64 = tx.query_row(
            "SELECT COALESCE(MAX(version), 0) + 1 FROM project_git_rules WHERE project_id=?1",
            [&project_id],
            |row| row.get(0),
        )?;
        let enabled_int: i64 = i64::from(request.enabled);
        tx.execute(
            "INSERT INTO project_git_rules(project_id, version, enabled, rules_json, rules_sha256, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                &project_id,
                next_version,
                enabled_int,
                &rules_json,
                &rules_sha256,
                &created_at
            ],
        )?;
        if request.enabled {
            crate::git_tool::queue::backfill_done_pulsations_in_tx(&tx, &project_id)?;
        }
        tx.commit()?;

        Ok(SaveProjectGitRulesOutcome::Saved(Box::new(
            ProjectGitRulesRecord {
                project_id,
                version: next_version,
                enabled: request.enabled,
                rules: request.rules,
                rules_sha256,
                created_at,
            },
        )))
    }

    /// Restores default `CommitRulesV1` rules as a new saved version while
    /// preserving the project's current `enabled` flag.
    pub fn restore_default_project_git_rules(
        &self,
        project_id: &str,
    ) -> Result<ProjectGitRulesRecord, PersistenceServiceError> {
        let current = self.get_project_git_rules(project_id)?;
        match self.save_project_git_rules(SaveProjectGitRulesRequest {
            project_id: current.project_id,
            enabled: current.enabled,
            rules: CommitRulesV1::default(),
        })? {
            SaveProjectGitRulesOutcome::Saved(record) => Ok(*record),
            SaveProjectGitRulesOutcome::Invalid(errors) => {
                Err(PersistenceServiceError::Validation(format!(
                    "Default commit rules failed validation unexpectedly: {:?}",
                    errors
                )))
            }
        }
    }
}
