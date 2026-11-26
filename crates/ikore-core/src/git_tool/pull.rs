//! Host-managed git fast-forward pull executor (Feature 08 M3).
//!
//! Refuses to pull while any pulsation is active or while another automation
//! step is running (`ignore_slot_owner_id` allows the calling `git.pull` step
//! to authorize itself cleanly), enforces `git pull --ff-only`, returns the
//! affected file list when local edits would be overwritten, and updates
//! `project_file_baselines` plus an audit external-changes notice after a fast-forward.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::Path;
use std::time::Instant;

use ikore_contracts::external_changes::{
    ChangeKind, ExternalChangeV1, render_notice, within_budget,
};
use ikore_contracts::git_tool::{
    GitOperationKind, GitOperationStatus, PullExecutionOutcomeV1, PullReceiptV1, RuleViolation,
    violation_codes,
};
use rusqlite::{TransactionBehavior, params};
use sha2::{Digest, Sha256};

use crate::events::{AppEvent, AppEventBus};
use crate::git_tool::GitOperationEvent;
use crate::persistence_service::{PersistenceService, PersistenceServiceError};
use crate::secret_redactor::SecretRedactor;

use super::commit::GitProjectLocks;
use super::plan::inspect_repo_state;
use super::process::{
    HostBinary, REMOTE_TIMEOUT_SECS, ResolvedHostBinary, STATUS_TIMEOUT_SECS, os_args_owned,
    resolve_host_binary, run_host_command, validate_executable_path,
};
use super::push::is_auth_failure;
use super::queue::{begin_git_operation, finish_git_operation};

fn emit_git_event(
    events: Option<&AppEventBus>,
    project_id: &str,
    operation_id: &str,
    status: GitOperationStatus,
) {
    if let Some(bus) = events {
        bus.publish(AppEvent::GitOperation(GitOperationEvent {
            project_id: project_id.to_owned(),
            operation_id: operation_id.to_owned(),
            kind: GitOperationKind::Pull,
            status,
        }));
    }
}

async fn run_git_rw(
    git: &ResolvedHostBinary,
    cwd: &Path,
    args: &[&str],
    timeout_secs: u64,
    redactor: &SecretRedactor,
) -> Result<super::process::CommandOutput, PersistenceServiceError> {
    let owned: Vec<OsString> = args.iter().map(OsString::from).collect();
    let refs = os_args_owned(&owned);
    run_host_command(git, cwd, &refs, false, timeout_secs, redactor)
        .await
        .map_err(|error| PersistenceServiceError::Runtime(error.to_string()))
}

fn blocked_pull(
    persistence: &PersistenceService,
    events: Option<&AppEventBus>,
    op_id: &str,
    project_id: &str,
    violations: Vec<RuleViolation>,
) -> Result<PullExecutionOutcomeV1, PersistenceServiceError> {
    {
        let db = persistence.connection()?;
        finish_git_operation(&db, op_id, GitOperationStatus::Blocked, None, &violations)?;
    }
    emit_git_event(events, project_id, op_id, GitOperationStatus::Blocked);
    Ok(PullExecutionOutcomeV1 {
        operation_id: op_id.to_owned(),
        status: GitOperationStatus::Blocked,
        receipt: None,
        violations,
        changed_paths: Vec::new(),
    })
}

fn extract_overwritten_paths(stderr: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut capture = false;
    for raw in stderr.lines() {
        let trimmed = raw.trim();
        let lower = trimmed.to_ascii_lowercase();
        if lower.contains("would be overwritten by merge")
            || lower.contains("untracked working tree files would be overwritten")
        {
            capture = true;
            continue;
        }
        if !capture {
            continue;
        }
        if trimmed.is_empty()
            || lower.starts_with("please commit")
            || lower.starts_with("please move")
            || lower.starts_with("aborting")
        {
            capture = false;
            continue;
        }
        if raw.starts_with('\t') || raw.starts_with("        ") {
            paths.push(trimmed.replace('\\', "/"));
        }
    }
    paths
}

fn is_non_fast_forward(lower_text: &str) -> bool {
    lower_text.contains("not possible to fast-forward")
        || lower_text.contains("need to specify how to reconcile divergent branches")
        || lower_text.contains("diverging")
        || lower_text.contains("fatal: refusing to merge")
}

/// Fast-forward pulls the current repository branch when no pulsation or other
/// automation run is active, and notifies Ikore baselines of changed files (Feature 08 M3).
pub async fn execute_pull_for_project(
    persistence: &PersistenceService,
    locks: &GitProjectLocks,
    events: Option<&AppEventBus>,
    project_id: &str,
    ignore_slot_owner_id: Option<&str>,
    git_override: Option<&Path>,
) -> Result<PullExecutionOutcomeV1, PersistenceServiceError> {
    let project_id = project_id.trim().to_owned();
    let project_lock = locks.lock_for(&project_id);
    let _guard = project_lock.lock().await;

    let started = Instant::now();
    let rules_record = persistence.get_project_git_rules(&project_id)?;
    let op_id = {
        let db = persistence.connection()?;
        begin_git_operation(
            &db,
            &project_id,
            GitOperationKind::Pull,
            None,
            rules_record.version,
            &rules_record.rules_sha256,
        )?
    };
    emit_git_event(events, &project_id, &op_id, GitOperationStatus::Running);

    let result = execute_pull_locked(
        persistence,
        events,
        &op_id,
        &project_id,
        &rules_record,
        ignore_slot_owner_id,
        git_override,
        started,
    )
    .await;

    if let Err(error) = &result {
        if let Ok(db) = persistence.connection() {
            let violation = RuleViolation {
                code: "git_execution_error".to_owned(),
                path: None,
                task_id: None,
                message: error.to_string(),
            };
            let _ =
                finish_git_operation(&db, &op_id, GitOperationStatus::Failed, None, &[violation]);
        }
        emit_git_event(events, &project_id, &op_id, GitOperationStatus::Failed);
    }

    result
}

#[allow(clippy::too_many_arguments)]
async fn execute_pull_locked(
    persistence: &PersistenceService,
    events: Option<&AppEventBus>,
    op_id: &str,
    project_id: &str,
    rules_record: &super::ProjectGitRulesRecord,
    ignore_slot_owner_id: Option<&str>,
    git_override: Option<&Path>,
    started: Instant,
) -> Result<PullExecutionOutcomeV1, PersistenceServiceError> {
    if !rules_record.enabled {
        return blocked_pull(
            persistence,
            events,
            op_id,
            project_id,
            vec![RuleViolation {
                code: violation_codes::TOOL_DISABLED.to_owned(),
                path: None,
                task_id: None,
                message: "Git tool is disabled for this project.".to_owned(),
            }],
        );
    }

    let (workspace_path, pulsation_active, automation_running): (String, bool, bool) = {
        let db = persistence.connection()?;
        let workspace: String = db.query_row(
            "SELECT workspace_path FROM projects WHERE id=?1",
            [project_id],
            |row| row.get(0),
        )?;
        let active_pulse: bool = db.query_row(
            "SELECT EXISTS(
               SELECT 1 FROM project_pipeline_state
                WHERE project_id=?1 AND active_pulsation_id IS NOT NULL
             ) OR EXISTS(
               SELECT 1 FROM pulsations
                WHERE project_id=?1 AND status NOT IN ('done','failed')
             )",
            [project_id],
            |row| row.get(0),
        )?;
        let active_automation: bool = db.query_row(
            "SELECT EXISTS(
               SELECT 1 FROM automation_slots
                WHERE project_id=?1 AND (?2 IS NULL OR owner_id <> ?2)
             )",
            params![project_id, ignore_slot_owner_id],
            |row| row.get(0),
        )?;
        (workspace, active_pulse, active_automation)
    };

    let mut guard_violations = Vec::new();
    if pulsation_active {
        guard_violations.push(RuleViolation {
            code: violation_codes::PULSATION_ACTIVE.to_owned(),
            path: None,
            task_id: None,
            message: "Cannot pull while a pulsation is active or queued in this project."
                .to_owned(),
        });
    }
    if automation_running {
        guard_violations.push(RuleViolation {
            code: violation_codes::AUTOMATION_RUNNING.to_owned(),
            path: None,
            task_id: None,
            message: "Cannot pull while another automation step is running in this project."
                .to_owned(),
        });
    }
    if !guard_violations.is_empty() {
        return blocked_pull(persistence, events, op_id, project_id, guard_violations);
    }

    let redactor = SecretRedactor::new().map_err(|error| {
        PersistenceServiceError::Runtime(format!("Secret redactor unavailable: {error}"))
    })?;
    let git_bin = match git_override {
        Some(path) => validate_executable_path(path),
        None => resolve_host_binary(HostBinary::Git),
    }
    .map_err(|error| PersistenceServiceError::Runtime(error.to_string()))?;

    let probe = inspect_repo_state(&git_bin, Path::new(&workspace_path), &redactor).await?;
    let Some(repo_root) = probe.repo_root.as_deref() else {
        return blocked_pull(
            persistence,
            events,
            op_id,
            project_id,
            vec![RuleViolation {
                code: violation_codes::NOT_A_GIT_REPOSITORY.to_owned(),
                path: None,
                task_id: None,
                message: "Project workspace is not inside a git repository.".to_owned(),
            }],
        );
    };

    let mut blocking = Vec::new();
    if probe.detached_head || probe.current_branch.is_none() {
        blocking.push(RuleViolation {
            code: violation_codes::DETACHED_HEAD.to_owned(),
            path: None,
            task_id: None,
            message: "Repository HEAD is detached. Switch to a branch before pulling.".to_owned(),
        });
    }
    if probe.operation_in_progress {
        blocking.push(RuleViolation {
            code: violation_codes::OPERATION_IN_PROGRESS.to_owned(),
            path: None,
            task_id: None,
            message: "A git merge, rebase, cherry-pick, or revert is currently in progress."
                .to_owned(),
        });
    }
    if probe.index_locked {
        blocking.push(RuleViolation {
            code: violation_codes::INDEX_LOCKED.to_owned(),
            path: None,
            task_id: None,
            message: "Repository .git/index.lock exists.".to_owned(),
        });
    }

    let branch = probe.current_branch.clone().unwrap_or_default();
    let remote = rules_record.rules.pull.remote.trim().to_owned();
    let remote_check = run_git_rw(
        &git_bin,
        repo_root,
        &["remote", "get-url", "--", &remote],
        STATUS_TIMEOUT_SECS,
        &redactor,
    )
    .await?;
    if !remote_check.success {
        blocking.push(RuleViolation {
            code: violation_codes::REMOTE_MISSING.to_owned(),
            path: None,
            task_id: None,
            message: format!("Configure git remote {remote} before pulling."),
        });
    }

    if !blocking.is_empty() {
        return blocked_pull(persistence, events, op_id, project_id, blocking);
    }

    let old_head = probe.head_sha.clone();
    let pull_out = run_git_rw(
        &git_bin,
        repo_root,
        &["pull", "--ff-only", "--", &remote, &branch],
        REMOTE_TIMEOUT_SECS,
        &redactor,
    )
    .await?;

    if !pull_out.success {
        let combined = format!("{}\n{}", pull_out.stdout, pull_out.stderr);
        let lower = combined.to_ascii_lowercase();
        let detail = if pull_out.stderr.trim().is_empty() {
            pull_out.stdout.trim()
        } else {
            pull_out.stderr.trim()
        };

        let overwritten = extract_overwritten_paths(&pull_out.stderr);
        let violations = if !overwritten.is_empty() || lower.contains("your local changes") {
            if overwritten.is_empty() {
                vec![RuleViolation {
                    code: violation_codes::LOCAL_CHANGES_WOULD_BE_OVERWRITTEN.to_owned(),
                    path: None,
                    task_id: None,
                    message: format!(
                        "Local workspace changes would be overwritten by pull: {detail}"
                    ),
                }]
            } else {
                overwritten
                    .into_iter()
                    .map(|path| RuleViolation {
                        code: violation_codes::LOCAL_CHANGES_WOULD_BE_OVERWRITTEN.to_owned(),
                        path: Some(path.clone()),
                        task_id: None,
                        message: format!("Local changes in {path} would be overwritten by pull."),
                    })
                    .collect()
            }
        } else if is_non_fast_forward(&lower) {
            vec![RuleViolation {
                code: violation_codes::NON_FAST_FORWARD.to_owned(),
                path: None,
                task_id: None,
                message: format!(
                    "Pull aborted because branch {branch} cannot fast-forward. ({detail})"
                ),
            }]
        } else if is_auth_failure(&lower) {
            vec![RuleViolation {
                code: violation_codes::CREDENTIALS_MISSING.to_owned(),
                path: None,
                task_id: None,
                message: format!(
                    "Git credentials are missing or rejected. Run gh auth login and gh auth setup-git, or authenticate with Git Credential Manager in your terminal. ({detail})"
                ),
            }]
        } else {
            vec![RuleViolation {
                code: "git_pull_failed".to_owned(),
                path: None,
                task_id: None,
                message: format!("git pull failed: {detail}"),
            }]
        };
        let status = if violations.iter().any(|v| v.code == "git_pull_failed") {
            GitOperationStatus::Failed
        } else {
            GitOperationStatus::Blocked
        };
        {
            let db = persistence.connection()?;
            finish_git_operation(&db, op_id, status, None, &violations)?;
        }
        emit_git_event(events, project_id, op_id, status);
        return Ok(PullExecutionOutcomeV1 {
            operation_id: op_id.to_owned(),
            status,
            receipt: None,
            violations,
            changed_paths: Vec::new(),
        });
    }

    let new_head_out = run_git_rw(
        &git_bin,
        repo_root,
        &["rev-parse", "-q", "--verify", "HEAD"],
        STATUS_TIMEOUT_SECS,
        &redactor,
    )
    .await?;
    let new_head = new_head_out
        .success
        .then(|| new_head_out.stdout.trim().to_owned())
        .filter(|s| !s.is_empty());
    let fast_forwarded = old_head.as_deref() != new_head.as_deref() && new_head.is_some();

    let mut changed_paths = Vec::new();
    let mut change_entries: Vec<ExternalChangeV1> = Vec::new();
    let mut new_hashes: BTreeMap<String, Option<String>> = BTreeMap::new();

    if fast_forwarded && let Some(new_sha) = new_head.as_deref() {
        if let Some(old_sha) = old_head.as_deref() {
            let diff_out = run_git_rw(
                &git_bin,
                repo_root,
                &["diff", "--name-status", "--no-renames", old_sha, new_sha],
                STATUS_TIMEOUT_SECS,
                &redactor,
            )
            .await?;
            if diff_out.success {
                for line in diff_out.stdout.lines() {
                    let mut parts = line.splitn(2, '\t');
                    let Some(status_char) = parts.next().map(str::trim) else {
                        continue;
                    };
                    let Some(raw_repo_path) = parts.next().map(str::trim) else {
                        continue;
                    };
                    if raw_repo_path.is_empty() {
                        continue;
                    }
                    let repo_path = raw_repo_path.replace('\\', "/");
                    let ws_rel = strip_prefix(&probe.repo_prefix, &repo_path);
                    let disk_path = repo_root.join(&repo_path);
                    let sha = hash_disk_file_if_present(&disk_path);
                    let kind = match status_char {
                        "A" => ChangeKind::Created,
                        "D" => ChangeKind::Deleted,
                        _ => ChangeKind::Modified,
                    };
                    changed_paths.push(ws_rel.clone());
                    new_hashes.insert(ws_rel.clone(), sha);
                    change_entries.push(ExternalChangeV1 {
                        path: ws_rel,
                        change: kind,
                        from_path: None,
                        added_lines: None,
                        removed_lines: None,
                        hunks: None,
                    });
                }
            }
        } else {
            let tree_out = run_git_rw(
                &git_bin,
                repo_root,
                &["ls-tree", "-r", "--name-only", new_sha],
                STATUS_TIMEOUT_SECS,
                &redactor,
            )
            .await?;
            if tree_out.success {
                for raw_repo_path in tree_out.stdout.lines() {
                    let repo_path = raw_repo_path.trim().replace('\\', "/");
                    if repo_path.is_empty() {
                        continue;
                    }
                    let ws_rel = strip_prefix(&probe.repo_prefix, &repo_path);
                    let sha = hash_disk_file_if_present(&repo_root.join(&repo_path));
                    changed_paths.push(ws_rel.clone());
                    new_hashes.insert(ws_rel.clone(), sha);
                    change_entries.push(ExternalChangeV1 {
                        path: ws_rel,
                        change: ChangeKind::Created,
                        from_path: None,
                        added_lines: None,
                        removed_lines: None,
                        hunks: None,
                    });
                }
            }
        }
    }

    let summary = if fast_forwarded {
        format!(
            "Fast-forwarded {branch} from {remote} ({} changed path(s)).",
            changed_paths.len()
        )
    } else {
        format!("Already up to date with {remote}/{branch}.")
    };
    let receipt = PullReceiptV1 {
        project_id: project_id.to_owned(),
        branch,
        remote,
        fast_forwarded,
        summary,
        rules_version: rules_record.version,
        rules_sha256: rules_record.rules_sha256.clone(),
        duration_ms: started.elapsed().as_millis() as u64,
    };
    let receipt_json = serde_json::to_string(&receipt).map_err(|error| {
        PersistenceServiceError::Runtime(format!("Failed to serialize PullReceiptV1: {error}"))
    })?;

    let now = chrono::Utc::now().to_rfc3339();
    {
        let mut db = persistence.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (rel_path, sha_opt) in &new_hashes {
            tx.execute(
                "INSERT INTO project_file_baselines(project_id,relative_path,content_sha256,updated_at)
                 VALUES(?1,?2,?3,?4)
                 ON CONFLICT(project_id,relative_path) DO UPDATE SET
                   content_sha256=excluded.content_sha256,
                   updated_at=excluded.updated_at",
                params![project_id, rel_path, sha_opt, now],
            )?;
        }
        if !change_entries.is_empty() {
            let notice_bundle = within_budget(op_id, change_entries);
            let notice_text =
                render_notice(&notice_bundle).unwrap_or_else(|| receipt.summary.clone());
            let clean_notice = redactor.redact(&notice_text).text;
            tx.execute(
                "INSERT INTO logs(project_id, pulsation_id, level, message, created_at)
                 VALUES(?1, NULL, 'info', ?2, ?3)",
                params![project_id, clean_notice, now],
            )?;
        }
        finish_git_operation(
            &tx,
            op_id,
            GitOperationStatus::Succeeded,
            Some(&receipt_json),
            &[],
        )?;
        tx.commit()?;
    }
    emit_git_event(events, project_id, op_id, GitOperationStatus::Succeeded);

    Ok(PullExecutionOutcomeV1 {
        operation_id: op_id.to_owned(),
        status: GitOperationStatus::Succeeded,
        receipt: Some(receipt),
        violations: Vec::new(),
        changed_paths,
    })
}

fn strip_prefix(repo_prefix: &str, repo_path: &str) -> String {
    if !repo_prefix.is_empty()
        && let Some(stripped) = repo_path.strip_prefix(repo_prefix)
    {
        stripped.trim_start_matches('/').to_owned()
    } else {
        repo_path.to_owned()
    }
}

fn hash_disk_file_if_present(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Some(
        hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
    )
}
