//! Plumbing commit executor with per-project locking, CAS `update-ref`, P6
//! `already_in_head` detection, G3 real-index reconciliation, and durable
//! `git_operations` + `pulsation_commit_state` persistence (Feature 08 M2).

use std::collections::{BTreeMap, HashMap};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ikore_contracts::git_tool::{
    BranchMode, CommitExecutionOutcomeV1, CommitPlanV1, CommitReceiptEntryV1, CommitReceiptV1,
    GitOperationKind, GitOperationStatus, RuleViolation, format_commit_message, violation_codes,
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use tokio::sync::Mutex as AsyncMutex;

use crate::events::{AppEvent, AppEventBus};
use crate::persistence_service::{PersistenceService, PersistenceServiceError};
use crate::secret_redactor::SecretRedactor;

use super::plan::{build_commit_plan_for_pulsation, ensure_branch_is_unprotected, today_yyyymmdd};
use super::process::{
    COMMIT_TIMEOUT_SECS, HostBinary, ResolvedHostBinary, STATUS_TIMEOUT_SECS, os_args_owned,
    resolve_host_binary, run_host_command, run_host_command_with_input_and_env,
    validate_executable_path,
};
use super::queue::{begin_git_operation, finish_git_operation};

const INDEX_LOCK_RETRY_MS: u64 = 3_000;

/// Coordinates host git operations so at most one write operation executes per project at a time.
#[derive(Default)]
pub struct GitProjectLocks {
    by_project: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
}

impl GitProjectLocks {
    pub fn lock_for(&self, project_id: &str) -> Arc<AsyncMutex<()>> {
        let mut guard = self
            .by_project
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard
            .entry(project_id.trim().to_owned())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitOperationEvent {
    pub project_id: String,
    pub operation_id: String,
    pub kind: GitOperationKind,
    pub status: GitOperationStatus,
}

struct TempPathsGuard(Vec<PathBuf>);

impl Drop for TempPathsGuard {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = std::fs::remove_file(path);
        }
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

async fn run_git_with_index_and_input(
    git: &ResolvedHostBinary,
    cwd: &Path,
    args: &[OsString],
    index_file: &Path,
    stdin_bytes: Option<&[u8]>,
    redactor: &SecretRedactor,
) -> Result<super::process::CommandOutput, PersistenceServiceError> {
    let refs = os_args_owned(args);
    let idx_os = index_file.as_os_str();
    run_host_command_with_input_and_env(
        git,
        cwd,
        &refs,
        false,
        COMMIT_TIMEOUT_SECS,
        stdin_bytes,
        &[("GIT_INDEX_FILE", idx_os)],
        redactor,
    )
    .await
    .map_err(|error| PersistenceServiceError::Runtime(error.to_string()))
}

fn emit_git_event(
    events: &AppEventBus,
    project_id: &str,
    operation_id: &str,
    kind: GitOperationKind,
    status: GitOperationStatus,
) {
    events.publish(AppEvent::GitOperation(GitOperationEvent {
        project_id: project_id.to_owned(),
        operation_id: operation_id.to_owned(),
        kind,
        status,
    }));
}

async fn wait_for_index_lock_clear(git_dir: &Path) -> bool {
    let lock_path = git_dir.join("index.lock");
    if !lock_path.exists() {
        return true;
    }
    let deadline = Instant::now() + Duration::from_millis(INDEX_LOCK_RETRY_MS);
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(150)).await;
        if !lock_path.exists() {
            return true;
        }
    }
    !lock_path.exists()
}

/// Commits a finished pulsation (`Done`) for `project_id` under host rules using
/// git plumbing (`hash-object`, `update-index`, `write-tree`, `commit-tree`, `update-ref`).
#[allow(clippy::too_many_arguments)]
pub async fn execute_commit_for_pulsation(
    persistence: &PersistenceService,
    locks: &GitProjectLocks,
    events: &AppEventBus,
    project_id: &str,
    pulsation_id: &str,
    git_override: Option<&Path>,
    external_plan: Option<&CommitPlanV1>,
) -> Result<CommitExecutionOutcomeV1, PersistenceServiceError> {
    let project_id = project_id.trim().to_owned();
    let pulsation_id = pulsation_id.trim().to_owned();
    let project_lock = locks.lock_for(&project_id);
    let _guard = project_lock.lock().await;

    let started = Instant::now();
    let rules_record = persistence.get_project_git_rules(&project_id)?;
    let op_id = {
        let db = persistence.connection()?;
        begin_git_operation(
            &db,
            &project_id,
            GitOperationKind::Commit,
            Some(&pulsation_id),
            rules_record.version,
            &rules_record.rules_sha256,
        )?
    };
    emit_git_event(
        events,
        &project_id,
        &op_id,
        GitOperationKind::Commit,
        GitOperationStatus::Running,
    );

    let result = execute_commit_locked(
        persistence,
        events,
        &op_id,
        &project_id,
        &pulsation_id,
        git_override,
        external_plan,
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
        emit_git_event(
            events,
            &project_id,
            &op_id,
            GitOperationKind::Commit,
            GitOperationStatus::Failed,
        );
    }

    result
}

#[allow(clippy::too_many_arguments)]
async fn execute_commit_locked(
    persistence: &PersistenceService,
    events: &AppEventBus,
    op_id: &str,
    project_id: &str,
    pulsation_id: &str,
    git_override: Option<&Path>,
    external_plan: Option<&CommitPlanV1>,
    started: Instant,
) -> Result<CommitExecutionOutcomeV1, PersistenceServiceError> {
    let redactor = SecretRedactor::new().map_err(|error| {
        PersistenceServiceError::Runtime(format!("Secret redactor unavailable: {error}"))
    })?;
    let git_bin = match git_override {
        Some(path) => validate_executable_path(path),
        None => resolve_host_binary(HostBinary::Git),
    }
    .map_err(|error| PersistenceServiceError::Runtime(error.to_string()))?;

    // Build initial plan (may see transient index.lock; retry lock once before blocking).
    let yyyymmdd = today_yyyymmdd();
    let mut built = build_commit_plan_for_pulsation(
        persistence,
        project_id,
        pulsation_id,
        git_override,
        &yyyymmdd,
        external_plan,
    )
    .await?;

    if built
        .preview
        .plan
        .blocking_violations
        .iter()
        .any(|v| v.code == violation_codes::INDEX_LOCKED)
        && let Some(root) = built.repo_root.as_deref()
    {
        let gd = run_git_rw(
            &git_bin,
            root,
            &["rev-parse", "--git-dir"],
            STATUS_TIMEOUT_SECS,
            &redactor,
        )
        .await?;
        if gd.success {
            let raw = PathBuf::from(gd.stdout.trim());
            let git_dir = if raw.is_absolute() {
                raw
            } else {
                root.join(raw)
            };
            if wait_for_index_lock_clear(&git_dir).await {
                built = build_commit_plan_for_pulsation(
                    persistence,
                    project_id,
                    pulsation_id,
                    git_override,
                    &yyyymmdd,
                    external_plan,
                )
                .await?;
            }
        }
    }

    // Check P6 `already_in_head` before blocking or writing anything to git.
    let only_branch_exists_or_none = built
        .preview
        .plan
        .blocking_violations
        .iter()
        .all(|v| v.code == violation_codes::BRANCH_EXISTS);
    if built.preview.already_in_head
        && built.rules_record.enabled
        && only_branch_exists_or_none
        && !built.preview.plan.groups.is_empty()
    {
        let now = chrono::Utc::now().to_rfc3339();
        let mut db = persistence.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE pulsation_commit_state
                SET state='skipped', skip_reason='already_in_head', head_commit=?2, updated_at=?3
              WHERE pulsation_id=?1",
            params![pulsation_id, built.base_head_full, now],
        )?;
        finish_git_operation(
            &tx,
            op_id,
            GitOperationStatus::Succeeded,
            None,
            &built.preview.plan.warnings,
        )?;
        tx.commit()?;
        emit_git_event(
            events,
            project_id,
            op_id,
            GitOperationKind::Commit,
            GitOperationStatus::Succeeded,
        );
        return Ok(CommitExecutionOutcomeV1 {
            operation_id: op_id.to_owned(),
            status: GitOperationStatus::Succeeded,
            receipt: None,
            violations: built.preview.plan.warnings,
            skipped_already_in_head: true,
        });
    }

    let mut blocking = built.preview.plan.blocking_violations.clone();
    if blocking.is_empty() && built.preview.plan.groups.is_empty() {
        blocking.push(RuleViolation {
            code: "empty_commit_plan".to_owned(),
            path: None,
            task_id: None,
            message: "This pulsation produced no published file changes to commit.".to_owned(),
        });
    }

    if !blocking.is_empty() {
        let mut all_violations = blocking;
        all_violations.extend(built.preview.plan.warnings.clone());
        let db = persistence.connection()?;
        finish_git_operation(
            &db,
            op_id,
            GitOperationStatus::Blocked,
            None,
            &all_violations,
        )?;
        emit_git_event(
            events,
            project_id,
            op_id,
            GitOperationKind::Commit,
            GitOperationStatus::Blocked,
        );
        return Ok(CommitExecutionOutcomeV1 {
            operation_id: op_id.to_owned(),
            status: GitOperationStatus::Blocked,
            receipt: None,
            violations: all_violations,
            skipped_already_in_head: false,
        });
    }

    let repo_root = built
        .repo_root
        .as_deref()
        .ok_or_else(|| PersistenceServiceError::Runtime("Repository root missing.".into()))?;
    let plan = &built.preview.plan;
    let rules = &built.rules_record.rules;

    if let Err(violation) = ensure_branch_is_unprotected(&plan.branch, &rules.branch.protected) {
        let db = persistence.connection()?;
        finish_git_operation(
            &db,
            op_id,
            GitOperationStatus::Blocked,
            None,
            std::slice::from_ref(&violation),
        )?;
        emit_git_event(
            events,
            project_id,
            op_id,
            GitOperationKind::Commit,
            GitOperationStatus::Blocked,
        );
        return Ok(CommitExecutionOutcomeV1 {
            operation_id: op_id.to_owned(),
            status: GitOperationStatus::Blocked,
            receipt: None,
            violations: vec![violation],
            skipped_already_in_head: false,
        });
    }

    // Step G2: checkout/create pulsation branch cleanly without touching working tree files.
    if rules.branch.mode == BranchMode::PerPulsation
        && built.current_branch.as_deref() != Some(plan.branch.as_str())
    {
        let switch_out = run_git_rw(
            &git_bin,
            repo_root,
            &["switch", "-c", &plan.branch],
            COMMIT_TIMEOUT_SECS,
            &redactor,
        )
        .await?;
        if !switch_out.success {
            return Err(PersistenceServiceError::Runtime(format!(
                "Failed to switch to branch {}: {}",
                plan.branch,
                switch_out.stderr.trim()
            )));
        }
    }

    let git_dir_out = run_git_rw(
        &git_bin,
        repo_root,
        &["rev-parse", "--git-dir"],
        STATUS_TIMEOUT_SECS,
        &redactor,
    )
    .await?;
    if !git_dir_out.success {
        return Err(PersistenceServiceError::Runtime(
            "Failed to resolve .git directory.".into(),
        ));
    }
    let raw_git_dir = PathBuf::from(git_dir_out.stdout.trim());
    let git_dir = if raw_git_dir.is_absolute() {
        raw_git_dir
    } else {
        repo_root.join(raw_git_dir)
    };
    let ikore_tmp_dir = git_dir.join("ikore");
    std::fs::create_dir_all(&ikore_tmp_dir)?;

    let ref_name = format!("refs/heads/{}", plan.branch);
    let initial_tip_out = run_git_rw(
        &git_bin,
        repo_root,
        &["rev-parse", "-q", "--verify", &ref_name],
        STATUS_TIMEOUT_SECS,
        &redactor,
    )
    .await?;
    let mut tip_sha: Option<String> = if initial_tip_out.success {
        let s = initial_tip_out.stdout.trim().to_owned();
        (!s.is_empty()).then_some(s)
    } else {
        None
    };

    let blob_by_rel: BTreeMap<&str, &super::validate::PlannedBlob> = built
        .blobs
        .iter()
        .map(|b| (b.workspace_rel_path.as_str(), b))
        .collect();

    let author_identity = built
        .author_name
        .as_deref()
        .zip(built.author_email.as_deref());

    let mut temp_guard = TempPathsGuard(Vec::new());
    let mut committed_entries = Vec::with_capacity(plan.groups.len());
    // Track final committed blob SHA & mode per repo_path for G3 real-index reconciliation.
    let mut final_blobs: Vec<(String, Option<String>, String, String)> = Vec::new();

    // Base tree blobs per repo_path (for G3 comparison).
    let mut base_tree_blobs: BTreeMap<String, String> = BTreeMap::new();
    if let Some(base_head) = built.base_head_full.as_deref() {
        for blob in &built.blobs {
            if let Some((_mode, oid)) =
                read_tree_mode_and_oid(&git_bin, repo_root, base_head, &blob.repo_path, &redactor)
                    .await?
            {
                base_tree_blobs.insert(blob.repo_path.clone(), oid);
            }
        }
    }

    for (idx, group) in plan.groups.iter().enumerate() {
        let tmp_index = ikore_tmp_dir.join(format!("index-{op_id}-{idx}"));
        let tmp_msg = ikore_tmp_dir.join(format!("msg-{op_id}-{idx}.txt"));
        let _ = std::fs::remove_file(&tmp_index);
        temp_guard.0.push(tmp_index.clone());
        temp_guard.0.push(tmp_msg.clone());

        if let Some(current_tip) = tip_sha.as_deref() {
            let read_tree = run_git_with_index_and_input(
                &git_bin,
                repo_root,
                &[OsString::from("read-tree"), OsString::from(current_tip)],
                &tmp_index,
                None,
                &redactor,
            )
            .await?;
            if !read_tree.success {
                return Err(PersistenceServiceError::Runtime(format!(
                    "git read-tree failed: {}",
                    read_tree.stderr.trim()
                )));
            }
        }

        let mut group_lines = 0usize;
        for rel_path in &group.paths {
            let blob = blob_by_rel.get(rel_path.as_str()).ok_or_else(|| {
                PersistenceServiceError::Runtime(format!(
                    "Missing blob snapshot for planned path {rel_path}."
                ))
            })?;
            group_lines += blob.added_lines + blob.removed_lines;

            let mode = if let Some(current_tip) = tip_sha.as_deref() {
                read_tree_mode_and_oid(&git_bin, repo_root, current_tip, &blob.repo_path, &redactor)
                    .await?
                    .map(|(m, _)| m)
                    .unwrap_or_else(|| "100644".to_owned())
            } else {
                "100644".to_owned()
            };

            let hash_out = run_host_command_with_input_and_env(
                &git_bin,
                repo_root,
                &[
                    OsStr::new("hash-object"),
                    OsStr::new("-w"),
                    OsStr::new("--path"),
                    OsStr::new(&blob.repo_path),
                    OsStr::new("--stdin"),
                ],
                false,
                COMMIT_TIMEOUT_SECS,
                Some(&blob.content_bytes),
                &[],
                &redactor,
            )
            .await
            .map_err(|error| PersistenceServiceError::Runtime(error.to_string()))?;
            if !hash_out.success {
                return Err(PersistenceServiceError::Runtime(format!(
                    "git hash-object failed for {}: {}",
                    blob.repo_path,
                    hash_out.stderr.trim()
                )));
            }
            let blob_oid = hash_out.stdout.trim().to_owned();
            let cacheinfo = format!("{mode},{blob_oid},{}", blob.repo_path);
            let update_idx = run_git_with_index_and_input(
                &git_bin,
                repo_root,
                &[
                    OsString::from("update-index"),
                    OsString::from("--add"),
                    OsString::from("--cacheinfo"),
                    OsString::from(cacheinfo),
                ],
                &tmp_index,
                None,
                &redactor,
            )
            .await?;
            if !update_idx.success {
                return Err(PersistenceServiceError::Runtime(format!(
                    "git update-index failed for {}: {}",
                    blob.repo_path,
                    update_idx.stderr.trim()
                )));
            }
            let base_oid = base_tree_blobs.get(&blob.repo_path).cloned();
            final_blobs.push((blob.repo_path.clone(), base_oid, mode, blob_oid));
        }

        let write_tree = run_git_with_index_and_input(
            &git_bin,
            repo_root,
            &[OsString::from("write-tree")],
            &tmp_index,
            None,
            &redactor,
        )
        .await?;
        if !write_tree.success {
            return Err(PersistenceServiceError::Runtime(format!(
                "git write-tree failed: {}",
                write_tree.stderr.trim()
            )));
        }
        let tree_sha = write_tree.stdout.trim().to_owned();

        let message = format_commit_message(group, &rules.message, author_identity);
        std::fs::write(&tmp_msg, message.as_bytes())?;

        let mut commit_args = vec![OsString::from("commit-tree"), OsString::from(&tree_sha)];
        if let Some(parent_sha) = tip_sha.as_deref() {
            commit_args.push(OsString::from("-p"));
            commit_args.push(OsString::from(parent_sha));
        }
        commit_args.push(OsString::from("-F"));
        commit_args.push(tmp_msg.as_os_str().to_owned());

        let commit_out = run_git_with_index_and_input(
            &git_bin,
            repo_root,
            &commit_args,
            &tmp_index,
            None,
            &redactor,
        )
        .await?;
        if !commit_out.success {
            return Err(PersistenceServiceError::Runtime(format!(
                "git commit-tree failed: {}",
                commit_out.stderr.trim()
            )));
        }
        let new_commit_sha = commit_out.stdout.trim().to_owned();

        let expected_old = tip_sha
            .as_deref()
            .unwrap_or("0000000000000000000000000000000000000000");
        let update_ref = run_git_rw(
            &git_bin,
            repo_root,
            &["update-ref", &ref_name, &new_commit_sha, expected_old],
            COMMIT_TIMEOUT_SECS,
            &redactor,
        )
        .await?;
        if !update_ref.success {
            return Err(PersistenceServiceError::Runtime(format!(
                "Concurrent ref change blocked update-ref on {ref_name}: {}",
                update_ref.stderr.trim()
            )));
        }

        tip_sha = Some(new_commit_sha.clone());
        committed_entries.push(CommitReceiptEntryV1 {
            sha: new_commit_sha,
            message,
            files: group.paths.clone(),
            lines: group_lines,
            task_ids: group.task_ids.clone(),
        });
    }

    drop(temp_guard);

    // Step G3: reconcile real index ONLY where its current blob matches baseline (or absent).
    let mut warnings = built.preview.plan.warnings.clone();
    // Keep only the latest entry per repo_path in `final_blobs`.
    let mut reconciled_map: BTreeMap<String, (Option<String>, String, String)> = BTreeMap::new();
    for (repo_path, base_oid, mode, new_oid) in final_blobs {
        reconciled_map.insert(repo_path, (base_oid, mode, new_oid));
    }
    for (repo_path, (base_oid, mode, new_oid)) in reconciled_map {
        let stage_out = run_git_rw(
            &git_bin,
            repo_root,
            &["ls-files", "-s", "--", &repo_path],
            STATUS_TIMEOUT_SECS,
            &redactor,
        )
        .await?;
        let current_index_oid = if stage_out.success {
            stage_out
                .stdout
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .map(str::to_owned)
        } else {
            None
        };

        if current_index_oid == base_oid {
            let cacheinfo = format!("{mode},{new_oid},{repo_path}");
            let upd = run_git_rw(
                &git_bin,
                repo_root,
                &["update-index", "--add", "--cacheinfo", &cacheinfo],
                COMMIT_TIMEOUT_SECS,
                &redactor,
            )
            .await?;
            if !upd.success {
                warnings.push(RuleViolation {
                    code: "index_refresh_warning".to_owned(),
                    path: Some(repo_path),
                    task_id: None,
                    message: format!("Failed to update real index: {}", upd.stderr.trim()),
                });
            }
        } else {
            warnings.push(RuleViolation {
                code: "index_modified_by_user".to_owned(),
                path: Some(repo_path.clone()),
                task_id: None,
                message: format!("Left developer-staged index entry untouched for {repo_path}."),
            });
        }
    }
    let _ = run_git_rw(
        &git_bin,
        repo_root,
        &["update-index", "-q", "--refresh"],
        STATUS_TIMEOUT_SECS,
        &redactor,
    )
    .await;

    let git_version = run_git_rw(
        &git_bin,
        repo_root,
        &["--version"],
        STATUS_TIMEOUT_SECS,
        &redactor,
    )
    .await
    .ok()
    .and_then(|o| o.success.then(|| o.stdout.trim().to_owned()))
    .unwrap_or_else(|| "git".to_owned());

    let head_commit = tip_sha.unwrap_or_default();
    let receipt = CommitReceiptV1 {
        project_id: project_id.to_owned(),
        pulsation_id: pulsation_id.to_owned(),
        branch: plan.branch.clone(),
        base_head: built.base_head_full.unwrap_or_default(),
        commits: committed_entries,
        rules_version: built.rules_record.version,
        rules_sha256: built.rules_record.rules_sha256.clone(),
        git_version,
        warnings: warnings.clone(),
        duration_ms: started.elapsed().as_millis() as u64,
    };
    let receipt_json = serde_json::to_string(&receipt).map_err(|error| {
        PersistenceServiceError::Runtime(format!("Failed to serialize CommitReceiptV1: {error}"))
    })?;
    let now = chrono::Utc::now().to_rfc3339();
    {
        let mut db = persistence.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE pulsation_commit_state
                SET state='committed', skip_reason=NULL, branch=?2, head_commit=?3, updated_at=?4
              WHERE pulsation_id=?1",
            params![pulsation_id, plan.branch, head_commit, now],
        )?;
        finish_git_operation(
            &tx,
            op_id,
            GitOperationStatus::Succeeded,
            Some(&receipt_json),
            &warnings,
        )?;
        tx.commit()?;
    }

    emit_git_event(
        events,
        project_id,
        op_id,
        GitOperationKind::Commit,
        GitOperationStatus::Succeeded,
    );

    Ok(CommitExecutionOutcomeV1 {
        operation_id: op_id.to_owned(),
        status: GitOperationStatus::Succeeded,
        receipt: Some(receipt),
        violations: warnings,
        skipped_already_in_head: false,
    })
}

async fn read_tree_mode_and_oid(
    git: &ResolvedHostBinary,
    repo_root: &Path,
    treeish: &str,
    repo_path: &str,
    redactor: &SecretRedactor,
) -> Result<Option<(String, String)>, PersistenceServiceError> {
    let out = run_git_rw(
        git,
        repo_root,
        &["ls-tree", treeish, "--", repo_path],
        STATUS_TIMEOUT_SECS,
        redactor,
    )
    .await?;
    if !out.success {
        return Ok(None);
    }
    let Some(line) = out.stdout.lines().next() else {
        return Ok(None);
    };
    let mut parts = line.split_whitespace();
    let mode = parts.next().map(str::to_owned);
    let _kind = parts.next();
    let oid = parts.next().map(str::to_owned);
    Ok(mode.zip(oid))
}

impl PersistenceService {
    /// Read-only commit preview for `pulsation_id` (Settings › Git M2).
    pub async fn preview_commit_for_pulsation(
        &self,
        project_id: &str,
        pulsation_id: &str,
    ) -> Result<ikore_contracts::git_tool::GitCommitPreviewV1, PersistenceServiceError> {
        let built = build_commit_plan_for_pulsation(
            self,
            project_id,
            pulsation_id,
            None,
            &today_yyyymmdd(),
            None,
        )
        .await?;
        Ok(built.preview)
    }

    /// Reads the `pulsation_commit_state` queue state row for `pulsation_id`, if present.
    pub fn get_pulsation_commit_state(
        &self,
        pulsation_id: &str,
    ) -> Result<Option<String>, PersistenceServiceError> {
        let db = self.connection()?;
        let state = db
            .query_row(
                "SELECT state FROM pulsation_commit_state WHERE pulsation_id=?1",
                [pulsation_id.trim()],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(state)
    }
}
