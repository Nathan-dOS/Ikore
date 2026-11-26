//! Host-managed git push executor (Feature 08 M3).
//!
//! Pushes only the active non-protected branch via direct argv execution
//! (`git push --porcelain [-u] <remote> refs/heads/<branch>:refs/heads/<branch>`),
//! never uses force, tags, mirror, or all-ref pushes, scrubs secrets from all
//! outputs, and classifies rejections vs missing credentials cleanly.

use std::ffi::OsString;
use std::path::Path;
use std::time::Instant;

use ikore_contracts::git_tool::{
    GitOperationKind, GitOperationStatus, PushExecutionOutcomeV1, PushReceiptV1, RuleViolation,
    violation_codes,
};

use crate::events::{AppEvent, AppEventBus};
use crate::git_tool::GitOperationEvent;
use crate::persistence_service::{PersistenceService, PersistenceServiceError};
use crate::secret_redactor::SecretRedactor;

use super::commit::GitProjectLocks;
use super::plan::{ensure_branch_is_unprotected, inspect_repo_state};
use super::process::{
    HostBinary, REMOTE_TIMEOUT_SECS, ResolvedHostBinary, STATUS_TIMEOUT_SECS, os_args_owned,
    resolve_host_binary, run_host_command, validate_executable_path,
};
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
            kind: GitOperationKind::Push,
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

pub(crate) fn is_auth_failure(lower_text: &str) -> bool {
    lower_text.contains("authentication failed")
        || lower_text.contains("could not read username")
        || lower_text.contains("terminal prompts disabled")
        || lower_text.contains("permission denied (publickey)")
        || lower_text.contains("invalid username or")
        || lower_text.contains("logon failed")
        || lower_text.contains("credential-cache")
        || lower_text.contains("no anonymous")
        || lower_text.contains("401")
        || lower_text.contains("403")
        || lower_text.contains("fatal: authentication")
}

fn is_push_rejection(lower_text: &str) -> bool {
    lower_text.contains("[rejected]")
        || lower_text.contains("non-fast-forward")
        || lower_text.contains("fetch first")
        || lower_text.contains("failed to push some refs")
        || lower_text.contains("updates were rejected")
}

fn blocked_push(
    persistence: &PersistenceService,
    events: Option<&AppEventBus>,
    op_id: &str,
    project_id: &str,
    violations: Vec<RuleViolation>,
) -> Result<PushExecutionOutcomeV1, PersistenceServiceError> {
    {
        let db = persistence.connection()?;
        finish_git_operation(&db, op_id, GitOperationStatus::Blocked, None, &violations)?;
    }
    emit_git_event(events, project_id, op_id, GitOperationStatus::Blocked);
    Ok(PushExecutionOutcomeV1 {
        operation_id: op_id.to_owned(),
        status: GitOperationStatus::Blocked,
        receipt: None,
        violations,
    })
}

/// Pushes the active repository branch to the configured remote without force (Feature 08 M3).
pub async fn execute_push_for_project(
    persistence: &PersistenceService,
    locks: &GitProjectLocks,
    events: Option<&AppEventBus>,
    project_id: &str,
    git_override: Option<&Path>,
) -> Result<PushExecutionOutcomeV1, PersistenceServiceError> {
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
            GitOperationKind::Push,
            None,
            rules_record.version,
            &rules_record.rules_sha256,
        )?
    };
    emit_git_event(events, &project_id, &op_id, GitOperationStatus::Running);

    let result = execute_push_locked(
        persistence,
        events,
        &op_id,
        &project_id,
        &rules_record,
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

async fn execute_push_locked(
    persistence: &PersistenceService,
    events: Option<&AppEventBus>,
    op_id: &str,
    project_id: &str,
    rules_record: &super::ProjectGitRulesRecord,
    git_override: Option<&Path>,
    started: Instant,
) -> Result<PushExecutionOutcomeV1, PersistenceServiceError> {
    if !rules_record.enabled {
        return blocked_push(
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

    let workspace_path: String = {
        let db = persistence.connection()?;
        db.query_row(
            "SELECT workspace_path FROM projects WHERE id=?1",
            [project_id],
            |row| row.get(0),
        )?
    };
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
        return blocked_push(
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
            message: "Repository HEAD is detached. Switch to a branch before pushing.".to_owned(),
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
    if !branch.is_empty()
        && let Err(violation) =
            ensure_branch_is_unprotected(&branch, &rules_record.rules.branch.protected)
    {
        blocking.push(violation);
    }

    let remote = rules_record.rules.push.remote.trim().to_owned();
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
            message: format!("Configure git remote {remote} before pushing."),
        });
    }

    if !blocking.is_empty() {
        return blocked_push(persistence, events, op_id, project_id, blocking);
    }

    let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
    let mut args = vec!["push", "--porcelain"];
    if rules_record.rules.push.set_upstream {
        args.push("-u");
    }
    args.push("--");
    args.push(&remote);
    args.push(&refspec);

    let push_out = run_git_rw(&git_bin, repo_root, &args, REMOTE_TIMEOUT_SECS, &redactor).await?;
    if !push_out.success {
        let combined = format!("{}\n{}", push_out.stdout, push_out.stderr);
        let lower = combined.to_ascii_lowercase();
        let detail = if push_out.stderr.trim().is_empty() {
            push_out.stdout.trim()
        } else {
            push_out.stderr.trim()
        };
        let violation = if is_auth_failure(&lower) {
            RuleViolation {
                code: violation_codes::CREDENTIALS_MISSING.to_owned(),
                path: None,
                task_id: None,
                message: format!(
                    "Git credentials are missing or rejected. Run gh auth login and gh auth setup-git, or authenticate with Git Credential Manager in your terminal. ({detail})"
                ),
            }
        } else if is_push_rejection(&lower) {
            RuleViolation {
                code: violation_codes::PUSH_REJECTED.to_owned(),
                path: None,
                task_id: None,
                message: format!(
                    "Remote {remote} rejected the push (non-fast-forward). Pull with fast-forward first instead of rewriting history. ({detail})"
                ),
            }
        } else {
            RuleViolation {
                code: "git_push_failed".to_owned(),
                path: None,
                task_id: None,
                message: format!("git push failed: {detail}"),
            }
        };
        let status = if violation.code == "git_push_failed" {
            GitOperationStatus::Failed
        } else {
            GitOperationStatus::Blocked
        };
        {
            let db = persistence.connection()?;
            finish_git_operation(&db, op_id, status, None, std::slice::from_ref(&violation))?;
        }
        emit_git_event(events, project_id, op_id, status);
        return Ok(PushExecutionOutcomeV1 {
            operation_id: op_id.to_owned(),
            status,
            receipt: None,
            violations: vec![violation],
        });
    }

    let summary = {
        let raw = push_out.stdout.trim();
        if raw.is_empty() {
            format!("Pushed {branch} to {remote}.")
        } else {
            raw.lines().take(12).collect::<Vec<_>>().join(" | ")
        }
    };
    let receipt = PushReceiptV1 {
        project_id: project_id.to_owned(),
        branch,
        remote,
        set_upstream: rules_record.rules.push.set_upstream,
        summary,
        rules_version: rules_record.version,
        rules_sha256: rules_record.rules_sha256.clone(),
        duration_ms: started.elapsed().as_millis() as u64,
    };
    let receipt_json = serde_json::to_string(&receipt).map_err(|error| {
        PersistenceServiceError::Runtime(format!("Failed to serialize PushReceiptV1: {error}"))
    })?;
    {
        let db = persistence.connection()?;
        finish_git_operation(
            &db,
            op_id,
            GitOperationStatus::Succeeded,
            Some(&receipt_json),
            &[],
        )?;
    }
    emit_git_event(events, project_id, op_id, GitOperationStatus::Succeeded);

    Ok(PushExecutionOutcomeV1 {
        operation_id: op_id.to_owned(),
        status: GitOperationStatus::Succeeded,
        receipt: Some(receipt),
        violations: Vec::new(),
    })
}
