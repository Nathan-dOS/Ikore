//! Read-only commit plan builder & rule checker (Feature 08 M2).
//!
//! Reads accepted artifacts and applied publications from SQLite, verifies bases
//! against `HEAD` via `git cat-file --filters`, groups tasks into connected
//! components ordered by `pulsation_tasks.position`, counts non-excluded lines via
//! `ikore_contracts::line_diff`, checks LFS/symlinks/submodules/binaries/secrets,
//! and validates every rule without modifying the git repository.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use ikore_contracts::git_tool::{
    BranchMode, CommitGranularity, CommitPlanGroupV1, CommitPlanV1, CommitRulesV1, DocsPlacement,
    GitCommitPreviewV1, MessageScope, ProjectGitRulesRecord, PulsationCommitQueueState,
    RuleViolation, render_branch_name, slugify_branch_segment, violation_codes,
};
use ikore_contracts::line_diff::{LineDiff, unified as unified_line_diff};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::persistence_service::{PersistenceService, PersistenceServiceError};
use crate::secret_redactor::SecretRedactor;

use super::process::{
    HostBinary, RawCommandOutput, ResolvedHostBinary, STATUS_TIMEOUT_SECS, os_args_owned,
    resolve_host_binary, run_host_command, run_host_command_raw_stdout,
    run_host_command_with_input_and_env, validate_executable_path,
};
use super::validate::{
    PlannedBlob, compile_path_matchers, is_protected_branch, path_matches_any, validate_commit_plan,
};

#[derive(Debug, Clone)]
pub struct BuiltCommitPlan {
    pub preview: GitCommitPreviewV1,
    pub blobs: Vec<PlannedBlob>,
    pub repo_root: Option<PathBuf>,
    pub repo_prefix: String,
    pub author_name: Option<String>,
    pub author_email: Option<String>,
    pub current_branch: Option<String>,
    pub base_head_full: Option<String>,
    pub rules_record: ProjectGitRulesRecord,
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub(crate) fn today_yyyymmdd() -> String {
    chrono::Utc::now().format("%Y%m%d").to_string()
}

fn is_probably_test_path(path: &str) -> bool {
    let lower = path.replace('\\', "/").to_ascii_lowercase();
    lower.contains("/tests/")
        || lower.starts_with("tests/")
        || lower.contains("/__tests__/")
        || lower.ends_with("_test.rs")
        || lower.ends_with("_tests.rs")
        || lower.ends_with(".test.ts")
        || lower.ends_with(".test.tsx")
        || lower.ends_with(".test.js")
        || lower.ends_with(".spec.ts")
        || lower.ends_with(".spec.tsx")
        || lower.ends_with(".spec.js")
        || lower.starts_with("test_")
        || lower.contains("/test_")
}

fn is_docs_or_notes_path(path: &str) -> bool {
    let lower = path.replace('\\', "/").to_ascii_lowercase();
    lower.ends_with(".md")
        || lower.ends_with(".txt")
        || lower.ends_with(".rst")
        || lower.starts_with("docs/")
        || lower.starts_with("knowledge/")
}

fn count_diff_lines(base_text: Option<&str>, new_text: &str) -> (usize, usize) {
    let base = base_text.unwrap_or("");
    if base == new_text {
        return (0, 0);
    }
    match unified_line_diff(base, new_text, 256 * 1024) {
        LineDiff::Identical => (0, 0),
        LineDiff::TooLarge => {
            let new_lines = new_text.lines().count();
            let old_lines = base.lines().count();
            (new_lines, old_lines)
        }
        LineDiff::Unified { text, .. } => {
            let mut added = 0usize;
            let mut removed = 0usize;
            for line in text.lines() {
                if line.starts_with("+++") || line.starts_with("---") || line.starts_with("@@") {
                    continue;
                }
                if line.starts_with('+') {
                    added += 1;
                } else if line.starts_with('-') {
                    removed += 1;
                }
            }
            (added, removed)
        }
    }
}

struct TaskMeta {
    id: String,
    position: i64,
    title: String,
    kind: String,
    objective: String,
    criteria: Vec<String>,
    feature_title: Option<String>,
}

struct SnapshotPath {
    rel_path: String,
    desired_sha256: String,
    base_sha256: Option<String>,
    base_text: Option<String>,
    content_bytes: Vec<u8>,
    touches_code: bool,
    is_test_file: bool,
}

struct LoadedDbInputs {
    workspace_path: String,
    feature_name: String,
    pulse_status: String,
    completed_at: String,
    queue_state: Option<PulsationCommitQueueState>,
    has_sealed_attestation: bool,
    tasks: Vec<TaskMeta>,
    file_to_tasks: BTreeMap<String, Vec<String>>,
    snapshots: Vec<SnapshotPath>,
}

async fn run_git_ro(
    git: &ResolvedHostBinary,
    cwd: &Path,
    args: &[&str],
    redactor: &SecretRedactor,
) -> Result<super::process::CommandOutput, PersistenceServiceError> {
    let os: Vec<&std::ffi::OsStr> = args.iter().map(|s| std::ffi::OsStr::new(*s)).collect();
    run_host_command(git, cwd, &os, true, STATUS_TIMEOUT_SECS, redactor)
        .await
        .map_err(|error| PersistenceServiceError::Runtime(error.to_string()))
}

async fn run_git_ro_raw(
    git: &ResolvedHostBinary,
    cwd: &Path,
    args: &[&str],
    redactor: &SecretRedactor,
) -> Result<RawCommandOutput, PersistenceServiceError> {
    let os: Vec<&std::ffi::OsStr> = args.iter().map(|s| std::ffi::OsStr::new(*s)).collect();
    run_host_command_raw_stdout(
        git,
        cwd,
        &os,
        true,
        STATUS_TIMEOUT_SECS,
        None,
        &[],
        redactor,
    )
    .await
    .map_err(|error| PersistenceServiceError::Runtime(error.to_string()))
}

pub(crate) async fn inspect_repo_state(
    git: &ResolvedHostBinary,
    workspace_root: &Path,
    redactor: &SecretRedactor,
) -> Result<RepoProbe, PersistenceServiceError> {
    let mut probe = RepoProbe::default();
    if !workspace_root.is_dir() {
        return Ok(probe);
    }
    let toplevel = run_git_ro(
        git,
        workspace_root,
        &["rev-parse", "--show-toplevel"],
        redactor,
    )
    .await?;
    if !toplevel.success {
        return Ok(probe);
    }
    let root_str = toplevel.stdout.trim();
    if root_str.is_empty() {
        return Ok(probe);
    }
    probe.in_repo = true;
    probe.repo_root = Some(PathBuf::from(root_str));

    let prefix_out = run_git_ro(
        git,
        workspace_root,
        &["rev-parse", "--show-prefix"],
        redactor,
    )
    .await?;
    if prefix_out.success {
        let trimmed = prefix_out.stdout.trim().trim_start_matches('/').to_owned();
        probe.repo_prefix = trimmed;
    }

    let git_dir_out =
        run_git_ro(git, workspace_root, &["rev-parse", "--git-dir"], redactor).await?;
    if git_dir_out.success {
        let raw_dir = PathBuf::from(git_dir_out.stdout.trim());
        let git_dir = if raw_dir.is_absolute() {
            raw_dir
        } else {
            workspace_root.join(raw_dir)
        };
        probe.index_locked = git_dir.join("index.lock").exists();
        probe.operation_in_progress = git_dir.join("MERGE_HEAD").exists()
            || git_dir.join("REBASE_HEAD").exists()
            || git_dir.join("CHERRY_PICK_HEAD").exists()
            || git_dir.join("REVERT_HEAD").exists()
            || git_dir.join("rebase-merge").exists()
            || git_dir.join("rebase-apply").exists();
        probe.git_dir = Some(git_dir);
    }

    let name_out = run_git_ro(
        git,
        workspace_root,
        &["config", "--get", "user.name"],
        redactor,
    )
    .await?;
    if name_out.success {
        let v = name_out.stdout.trim().to_owned();
        if !v.is_empty() {
            probe.user_name = Some(v);
        }
    }
    let email_out = run_git_ro(
        git,
        workspace_root,
        &["config", "--get", "user.email"],
        redactor,
    )
    .await?;
    if email_out.success {
        let v = email_out.stdout.trim().to_owned();
        if !v.is_empty() {
            probe.user_email = Some(v);
        }
    }

    let head_sha_out = run_git_ro(
        git,
        workspace_root,
        &["rev-parse", "-q", "--verify", "HEAD"],
        redactor,
    )
    .await?;
    if head_sha_out.success {
        let sha = head_sha_out.stdout.trim().to_owned();
        if !sha.is_empty() {
            probe.head_sha = Some(sha);
        }
    }

    let branch_out = run_git_ro(
        git,
        workspace_root,
        &["symbolic-ref", "--short", "-q", "HEAD"],
        redactor,
    )
    .await?;
    if branch_out.success {
        let branch = branch_out.stdout.trim().to_owned();
        if !branch.is_empty() {
            probe.current_branch = Some(branch);
        }
    } else if probe.head_sha.is_some() {
        probe.detached_head = true;
    }

    Ok(probe)
}

#[derive(Debug, Default)]
pub(crate) struct RepoProbe {
    pub in_repo: bool,
    pub repo_root: Option<PathBuf>,
    pub repo_prefix: String,
    pub git_dir: Option<PathBuf>,
    pub index_locked: bool,
    pub operation_in_progress: bool,
    pub user_name: Option<String>,
    pub user_email: Option<String>,
    pub head_sha: Option<String>,
    pub current_branch: Option<String>,
    pub detached_head: bool,
}

pub(crate) fn normalize_repo_path(repo_prefix: &str, rel_path: &str) -> String {
    let clean_rel = rel_path
        .replace('\\', "/")
        .trim_start_matches('/')
        .to_owned();
    let clean_prefix = repo_prefix.replace('\\', "/");
    if clean_prefix.is_empty() {
        clean_rel
    } else {
        format!("{}{clean_rel}", clean_prefix)
    }
}

/// Builds a full read-only commit plan & validation preview for `pulsation_id`.
pub async fn build_commit_plan_for_pulsation(
    persistence: &PersistenceService,
    project_id: &str,
    pulsation_id: &str,
    git_override: Option<&Path>,
    yyyymmdd: &str,
    external_plan: Option<&CommitPlanV1>,
) -> Result<BuiltCommitPlan, PersistenceServiceError> {
    let project_id = project_id.trim().to_owned();
    let pulsation_id = pulsation_id.trim().to_owned();
    if project_id.is_empty() || pulsation_id.is_empty() {
        return Err(PersistenceServiceError::Validation(
            "Project id and pulsation id are required.".into(),
        ));
    }

    let rules_record = persistence.get_project_git_rules(&project_id)?;
    let _ = persistence.list_pulsation_commit_queue(&project_id)?;

    let LoadedDbInputs {
        workspace_path,
        feature_name,
        pulse_status,
        completed_at,
        queue_state,
        has_sealed_attestation,
        tasks,
        file_to_tasks,
        snapshots,
    } = {
        let db = persistence.connection()?;
        load_db_inputs(&db, &project_id, &pulsation_id)?
    };

    let redactor = SecretRedactor::new().map_err(|error| {
        PersistenceServiceError::Runtime(format!("Secret redactor unavailable: {error}"))
    })?;
    let git_bin = match git_override {
        Some(path) => validate_executable_path(path),
        None => resolve_host_binary(HostBinary::Git),
    }
    .map_err(|error| PersistenceServiceError::Runtime(error.to_string()))?;

    let workspace_root = PathBuf::from(&workspace_path);
    let probe = inspect_repo_state(&git_bin, &workspace_root, &redactor).await?;

    let mut eligibility_violations = Vec::new();
    if !rules_record.enabled {
        eligibility_violations.push(RuleViolation {
            code: violation_codes::TOOL_DISABLED.to_owned(),
            path: None,
            task_id: None,
            message: "The git tool is disabled for this project.".to_owned(),
        });
    }
    if pulse_status != "done" {
        eligibility_violations.push(RuleViolation {
            code: violation_codes::PULSATION_NOT_DONE.to_owned(),
            path: None,
            task_id: None,
            message: format!(
                "Only finished (Done) pulsations can be committed (current status: {pulse_status})."
            ),
        });
    }
    {
        let db = persistence.connection()?;
        let earlier_pending: bool = db.query_row(
            "SELECT EXISTS(
               SELECT 1 FROM pulsation_commit_state
                WHERE project_id=?1 AND state='pending'
                  AND (completed_at < ?2 OR (completed_at = ?2 AND pulsation_id < ?3))
             )",
            params![project_id, completed_at, pulsation_id],
            |row| row.get(0),
        )?;
        if earlier_pending {
            eligibility_violations.push(RuleViolation {
                code: violation_codes::EARLIER_PULSATION_PENDING.to_owned(),
                path: None,
                task_id: None,
                message: "An earlier completed pulsation is still pending in the commit queue."
                    .to_owned(),
            });
        }
    }

    if !probe.in_repo {
        eligibility_violations.push(RuleViolation {
            code: violation_codes::NOT_A_GIT_REPOSITORY.to_owned(),
            path: None,
            task_id: None,
            message: "Project workspace is not inside a git repository.".to_owned(),
        });
    } else {
        if probe.detached_head {
            eligibility_violations.push(RuleViolation {
                code: violation_codes::DETACHED_HEAD.to_owned(),
                path: None,
                task_id: None,
                message: "Repository HEAD is detached. Switch to a branch first.".to_owned(),
            });
        }
        if probe.operation_in_progress {
            eligibility_violations.push(RuleViolation {
                code: violation_codes::OPERATION_IN_PROGRESS.to_owned(),
                path: None,
                task_id: None,
                message: "A git merge, rebase, cherry-pick, or revert is currently in progress."
                    .to_owned(),
            });
        }
        if probe.index_locked {
            eligibility_violations.push(RuleViolation {
                code: violation_codes::INDEX_LOCKED.to_owned(),
                path: None,
                task_id: None,
                message: "Repository .git/index.lock exists.".to_owned(),
            });
        }
        if probe.user_name.is_none() || probe.user_email.is_none() {
            eligibility_violations.push(RuleViolation {
                code: violation_codes::IDENTITY_MISSING.to_owned(),
                path: None,
                task_id: None,
                message: "Configure git user.name and user.email before committing.".to_owned(),
            });
        }
    }

    let rules = &rules_record.rules;
    let exclude_matchers = compile_path_matchers(&rules.limits.exclude_from_count)
        .map_err(PersistenceServiceError::Validation)?;

    let mut blobs = Vec::with_capacity(snapshots.len());
    let mut all_match_desired_in_head = !snapshots.is_empty();
    let repo_root_ref = probe.repo_root.as_deref();

    // Pre-fetch git attributes (`filter` and `linguist-generated`) when inside a repo.
    let mut lfs_paths = BTreeSet::new();
    let mut generated_paths = BTreeSet::new();
    if let Some(root) = repo_root_ref
        && !snapshots.is_empty()
    {
        let repo_paths: Vec<String> = snapshots
            .iter()
            .map(|snap| normalize_repo_path(&probe.repo_prefix, &snap.rel_path))
            .collect();
        let stdin_list = repo_paths.join("\n");
        let attr_owned: Vec<std::ffi::OsString> = vec![
            "check-attr".into(),
            "--stdin".into(),
            "filter".into(),
            "linguist-generated".into(),
        ];
        // `git check-attr --stdin attr1 attr2` rejects `--` before `--stdin` paths in some git builds.
        let attr_args = os_args_owned(&attr_owned);
        let out = run_host_command_with_input_and_env(
            &git_bin,
            root,
            &attr_args,
            true,
            STATUS_TIMEOUT_SECS,
            Some(stdin_list.as_bytes()),
            &[],
            &redactor,
        )
        .await
        .map_err(|error| PersistenceServiceError::Runtime(error.to_string()))?;
        if out.success {
            for line in out.stdout.lines() {
                // Format: "<path>: <attr>: <info>"
                let parts: Vec<&str> = line.rsplitn(3, ':').collect();
                if parts.len() == 3 {
                    let info = parts[0].trim();
                    let attr = parts[1].trim();
                    let path = parts[2].trim().to_owned();
                    if attr == "filter" && info == "lfs" {
                        lfs_paths.insert(path);
                    } else if attr == "linguist-generated"
                        && (info == "set" || info.eq_ignore_ascii_case("true"))
                    {
                        generated_paths.insert(path);
                    }
                }
            }
        }
    }

    for snap in &snapshots {
        let actual_desired_sha = sha256_hex(&snap.content_bytes);
        if actual_desired_sha != snap.desired_sha256 {
            all_match_desired_in_head = false;
            eligibility_violations.push(RuleViolation {
                code: violation_codes::CONTENT_HASH_MISMATCH.to_owned(),
                path: Some(snap.rel_path.clone()),
                task_id: None,
                message: format!(
                    "Published artifact bytes for {} do not match recorded SHA-256.",
                    snap.rel_path
                ),
            });
        }

        let repo_path = normalize_repo_path(&probe.repo_prefix, &snap.rel_path);
        let disk_path = workspace_root.join(&snap.rel_path);
        let mut is_symlink = std::fs::symlink_metadata(&disk_path)
            .map(|meta| meta.file_type().is_symlink())
            .unwrap_or(false);
        let mut is_submodule = false;
        let is_lfs = lfs_paths.contains(&repo_path);

        let mut head_file_bytes: Option<Vec<u8>> = None;
        if let (Some(root), Some(head_sha)) = (repo_root_ref, probe.head_sha.as_deref()) {
            // Check tree mode for symlinks (120000) or submodules (160000).
            let ls_tree = run_git_ro(
                &git_bin,
                root,
                &["ls-tree", head_sha, "--", &repo_path],
                &redactor,
            )
            .await?;
            if ls_tree.success {
                for line in ls_tree.stdout.lines() {
                    if let Some(mode) = line.split_whitespace().next() {
                        if mode == "120000" {
                            is_symlink = true;
                        } else if mode == "160000" {
                            is_submodule = true;
                        }
                    }
                }
            }
            if !is_submodule {
                // Check ancestor directory submodule entry as well.
                let mut cur = Path::new(&repo_path).parent();
                while let Some(dir) = cur {
                    let s = dir.to_string_lossy();
                    if s.is_empty() {
                        break;
                    }
                    let dir_tree = run_git_ro(
                        &git_bin,
                        root,
                        &["ls-tree", head_sha, "--", s.as_ref()],
                        &redactor,
                    )
                    .await?;
                    if dir_tree.success
                        && dir_tree
                            .stdout
                            .lines()
                            .any(|line| line.starts_with("160000 "))
                    {
                        is_submodule = true;
                        break;
                    }
                    cur = dir.parent();
                }
            }

            let spec = format!("{head_sha}:{repo_path}");
            let exists = run_git_ro(&git_bin, root, &["cat-file", "-e", &spec], &redactor).await?;
            if exists.success {
                let filters_out =
                    run_git_ro_raw(&git_bin, root, &["cat-file", "--filters", &spec], &redactor)
                        .await?;
                if filters_out.success {
                    let bytes = filters_out.stdout;
                    let head_filtered_sha = sha256_hex(&bytes);
                    if Some(head_filtered_sha.as_str()) != snap.base_sha256.as_deref()
                        && head_filtered_sha != snap.desired_sha256
                    {
                        eligibility_violations.push(RuleViolation {
                            code: violation_codes::BASE_MISMATCH.to_owned(),
                            path: Some(snap.rel_path.clone()),
                            task_id: None,
                            message: format!(
                                "HEAD version of {} does not match the pulsation baseline.",
                                snap.rel_path
                            ),
                        });
                    }
                    if head_filtered_sha != snap.desired_sha256 {
                        all_match_desired_in_head = false;
                    }
                    head_file_bytes = Some(bytes);
                } else {
                    all_match_desired_in_head = false;
                }
            } else {
                if snap.base_sha256.is_some() {
                    eligibility_violations.push(RuleViolation {
                        code: violation_codes::BASE_MISMATCH.to_owned(),
                        path: Some(snap.rel_path.clone()),
                        task_id: None,
                        message: format!(
                            "Baseline expected {} to exist in HEAD, but it is absent.",
                            snap.rel_path
                        ),
                    });
                }
                all_match_desired_in_head = false;
            }
        } else {
            all_match_desired_in_head = false;
        }

        let is_binary =
            snap.content_bytes.contains(&0) || std::str::from_utf8(&snap.content_bytes).is_err();
        let (added_lines, removed_lines) = if is_binary {
            (0, 0)
        } else {
            let new_text = std::str::from_utf8(&snap.content_bytes).unwrap_or_default();
            let head_text = head_file_bytes
                .as_deref()
                .and_then(|bytes| std::str::from_utf8(bytes).ok())
                .or(snap.base_text.as_deref());
            count_diff_lines(head_text, new_text)
        };
        let excluded_from_count = generated_paths.contains(&repo_path)
            || path_matches_any(&snap.rel_path, &exclude_matchers).is_some()
            || path_matches_any(&repo_path, &exclude_matchers).is_some();

        blobs.push(PlannedBlob {
            workspace_rel_path: snap.rel_path.clone(),
            repo_path,
            content_bytes: snap.content_bytes.clone(),
            base_sha256: snap.base_sha256.clone(),
            desired_sha256: snap.desired_sha256.clone(),
            added_lines,
            removed_lines,
            excluded_from_count,
            touches_code: snap.touches_code,
            is_test_file: snap.is_test_file,
            is_binary,
            is_deleted: false,
            is_renamed: false,
            is_symlink,
            is_submodule,
            is_lfs,
        });
    }

    let target_branch = match rules.branch.mode {
        BranchMode::PerPulsation => render_branch_name(
            &rules.branch.pattern,
            &pulsation_id,
            &feature_name,
            yyyymmdd,
        ),
        BranchMode::CurrentBranch => probe.current_branch.clone().unwrap_or_default(),
    };

    let branch_already_exists = if let Some(root) = repo_root_ref
        && !target_branch.is_empty()
    {
        let ref_name = format!("refs/heads/{target_branch}");
        run_git_ro(
            &git_bin,
            root,
            &["show-ref", "--verify", "--quiet", &ref_name],
            &redactor,
        )
        .await?
        .success
    } else {
        false
    };

    let base_plan = if let Some(external) = external_plan {
        let mut p = external.clone();
        p.project_id = project_id.clone();
        p.pulsation_id = pulsation_id.clone();
        if p.branch.trim().is_empty() {
            p.branch = target_branch.clone();
        }
        if p.base_head.trim().is_empty() {
            p.base_head = probe.head_sha.clone().unwrap_or_default();
        }
        p.warnings.clear();
        p.blocking_violations = eligibility_violations;
        p
    } else {
        let groups = build_commit_groups(
            &pulsation_id,
            &feature_name,
            has_sealed_attestation,
            rules,
            &tasks,
            &file_to_tasks,
            &blobs,
        );
        CommitPlanV1 {
            project_id: project_id.clone(),
            pulsation_id: pulsation_id.clone(),
            branch: target_branch.clone(),
            base_head: probe.head_sha.clone().unwrap_or_default(),
            groups,
            warnings: Vec::new(),
            blocking_violations: eligibility_violations,
        }
    };

    let author_identity = probe.user_name.as_deref().zip(probe.user_email.as_deref());
    let validation = validate_commit_plan(
        &base_plan,
        &blobs,
        rules,
        &redactor,
        author_identity,
        probe.current_branch.as_deref(),
        branch_already_exists,
    );

    let mut plan = base_plan;
    plan.warnings = validation.warnings;
    plan.blocking_violations = validation.blocking_violations;

    let preview = GitCommitPreviewV1 {
        project_id,
        pulsation_id,
        feature_name,
        queue_state,
        already_in_head: all_match_desired_in_head && probe.head_sha.is_some(),
        plan,
        rules_version: rules_record.version,
        rules_sha256: rules_record.rules_sha256.clone(),
    };

    Ok(BuiltCommitPlan {
        preview,
        blobs,
        repo_root: probe.repo_root,
        repo_prefix: probe.repo_prefix,
        author_name: probe.user_name,
        author_email: probe.user_email,
        current_branch: probe.current_branch,
        base_head_full: probe.head_sha,
        rules_record,
    })
}

fn load_db_inputs(
    db: &Connection,
    project_id: &str,
    pulsation_id: &str,
) -> Result<LoadedDbInputs, PersistenceServiceError> {
    let (workspace_path, feature_name, pulse_status, completed_at): (
        String,
        String,
        String,
        String,
    ) = db
        .query_row(
            "SELECT pr.workspace_path, p.feature_name, p.status, p.updated_at
               FROM pulsations p
               JOIN projects pr ON pr.id=p.project_id
              WHERE p.id=?1 AND p.project_id=?2",
            params![pulsation_id, project_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?
        .ok_or_else(|| PersistenceServiceError::NotFound("Pulsation not found.".into()))?;

    let queue_state = db
        .query_row(
            "SELECT state FROM pulsation_commit_state WHERE pulsation_id=?1",
            [pulsation_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .and_then(|s| PulsationCommitQueueState::parse(&s));

    let has_sealed_attestation: bool = db
        .query_row(
            "SELECT EXISTS(
               SELECT 1 FROM pulsation_attestations
                WHERE pulsation_id=?1 AND signature_state='signed'
             )",
            [pulsation_id],
            |row| row.get(0),
        )
        .unwrap_or(false);

    let mut tasks = Vec::new();
    let mut task_stmt = db.prepare(
        "SELECT pt.task_id, pt.position, wt.draft_json, ff.title
           FROM pulsation_tasks pt
           JOIN work_tasks wt ON wt.id=pt.task_id
           LEFT JOIN forge_features ff ON ff.id=wt.feature_id
          WHERE pt.pulsation_id=?1
          ORDER BY pt.position ASC, pt.rowid ASC",
    )?;
    for row in task_stmt.query_map([pulsation_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })? {
        let (task_id, position, draft_json, feature_title) = row?;
        let draft: Value = serde_json::from_str(&draft_json).unwrap_or(Value::Null);
        let title = draft
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or(&task_id)
            .to_owned();
        let kind = draft
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("Feature")
            .to_owned();
        let objective = draft
            .get("objective")
            .and_then(Value::as_str)
            .or_else(|| draft.get("summary").and_then(Value::as_str))
            .unwrap_or("")
            .to_owned();
        let criteria = draft
            .get("acceptanceCriteria")
            .or_else(|| draft.get("criteria"))
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        tasks.push(TaskMeta {
            id: task_id,
            position,
            title,
            kind,
            objective,
            criteria,
            feature_title,
        });
    }

    // Bases from pulsation_file_bases (keyed lower-case).
    let mut base_map: BTreeMap<String, (Option<String>, Option<String>)> = BTreeMap::new();
    let mut base_stmt = db.prepare(
        "SELECT path_key, state, content_sha256, content
           FROM pulsation_file_bases
          WHERE pulsation_id=?1",
    )?;
    for row in base_stmt.query_map([pulsation_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })? {
        let (path_key, state, sha, text) = row?;
        let resolved_sha = if state == "absent" { None } else { sha };
        base_map.insert(path_key, (resolved_sha, text));
    }

    // Latest accepted applied publication per relative_path (last write wins).
    let mut pub_stmt = db.prepare(
        "SELECT a.relative_path, o.desired_sha256, o.expected_sha256, a.content,
                a.producer_stage, a.producer_job_id, a.generation, a.output_position
           FROM artifacts a
           JOIN artifact_publications o ON o.artifact_id=a.id
          WHERE a.pulsation_id=?1
            AND a.acceptance='accepted'
            AND o.state='applied'
            AND o.removed_at IS NULL
            AND a.relative_path IS NOT NULL
          ORDER BY a.producer_state_version ASC, a.output_position ASC, a.rowid ASC",
    )?;
    struct RawPub {
        rel_path: String,
        desired_sha256: String,
        fallback_base: Option<String>,
        content: Vec<u8>,
        stages: BTreeSet<String>,
        producers: Vec<(String, i64, i64, usize)>,
    }
    let mut by_key: BTreeMap<String, RawPub> = BTreeMap::new();
    for row in pub_stmt.query_map([pulsation_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, Option<i64>>(6)?,
            row.get::<_, Option<i64>>(7)?,
        ))
    })? {
        let (rel_path, desired_sha256, expected_sha256, content, stage, job_id, generation, pos) =
            row?;
        let key = rel_path.to_ascii_lowercase();
        let bytes = content.unwrap_or_default().into_bytes();
        let stage_str = stage.unwrap_or_default();
        let entry = by_key.entry(key).or_insert_with(|| RawPub {
            rel_path: rel_path.clone(),
            desired_sha256: desired_sha256.clone(),
            fallback_base: expected_sha256,
            content: bytes.clone(),
            stages: BTreeSet::new(),
            producers: Vec::new(),
        });
        entry.rel_path = rel_path;
        entry.desired_sha256 = desired_sha256;
        entry.content = bytes;
        if !stage_str.is_empty() {
            entry.stages.insert(stage_str.clone());
        }
        if let (Some(j), Some(g), Some(p)) = (job_id, generation, pos) {
            entry
                .producers
                .push((j, g, p.max(0), entry.producers.len()));
        }
    }

    // Build task_evidence lookup: (job_id, generation, output_position) -> Vec<task_id>.
    let mut ev_stmt = db.prepare(
        "SELECT job_id, generation, stage, task_id, evidence_json
           FROM task_evidence
          WHERE pulsation_id=?1 AND stage IN ('Code','Test','Integrator')",
    )?;
    let mut file_to_tasks: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut job_pos_to_tasks: BTreeMap<(String, i64, i64), Vec<(String, String)>> = BTreeMap::new();
    for row in ev_stmt.query_map([pulsation_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
        ))
    })? {
        let (job_id, generation, stage, task_id, ev_json) = row?;
        let ev: Value = serde_json::from_str(&ev_json).unwrap_or(Value::Null);
        if let Some(positions) = ev.get("artifactPositions").and_then(Value::as_array) {
            for p in positions.iter().filter_map(Value::as_i64) {
                job_pos_to_tasks
                    .entry((job_id.clone(), generation, p))
                    .or_default()
                    .push((task_id.clone(), stage.clone()));
            }
        }
    }

    let mut snapshots = Vec::with_capacity(by_key.len());
    for (key, raw) in by_key {
        let mut associated = BTreeSet::new();
        for (job_id, generation, pos, _) in &raw.producers {
            if let Some(pairs) = job_pos_to_tasks.get(&(job_id.clone(), *generation, *pos)) {
                for (task_id, _) in pairs {
                    associated.insert(task_id.clone());
                }
            }
        }
        if !associated.is_empty() {
            file_to_tasks.insert(raw.rel_path.clone(), associated.into_iter().collect());
        }
        let (base_sha256, base_text) = base_map
            .get(&key)
            .cloned()
            .unwrap_or((raw.fallback_base, None));
        let is_test = raw.stages.contains("Test") || is_probably_test_path(&raw.rel_path);
        let touches_code =
            raw.stages.contains("Code") && !is_test && !is_docs_or_notes_path(&raw.rel_path);
        snapshots.push(SnapshotPath {
            rel_path: raw.rel_path,
            desired_sha256: raw.desired_sha256,
            base_sha256,
            base_text,
            content_bytes: raw.content,
            touches_code,
            is_test_file: is_test,
        });
    }

    Ok(LoadedDbInputs {
        workspace_path,
        feature_name,
        pulse_status,
        completed_at,
        queue_state,
        has_sealed_attestation,
        tasks,
        file_to_tasks,
        snapshots,
    })
}

fn build_commit_groups(
    pulsation_id: &str,
    feature_name: &str,
    has_sealed_attestation: bool,
    rules: &CommitRulesV1,
    tasks: &[TaskMeta],
    file_to_tasks: &BTreeMap<String, Vec<String>>,
    blobs: &[PlannedBlob],
) -> Vec<CommitPlanGroupV1> {
    if blobs.is_empty() {
        return Vec::new();
    }
    let blob_by_path: BTreeMap<&str, &PlannedBlob> = blobs
        .iter()
        .map(|b| (b.workspace_rel_path.as_str(), b))
        .collect();

    if tasks.is_empty() {
        let mut trailers = vec![format!("Ikore-Pulsation: {pulsation_id}")];
        if has_sealed_attestation {
            trailers.push(format!("Ikore-Attestation: {pulsation_id}"));
        }
        let paths: Vec<String> = blobs.iter().map(|b| b.workspace_rel_path.clone()).collect();
        let (counted_files, counted_lines) = sum_counted(&paths, &blob_by_path);
        let commit_type = rules
            .message
            .types
            .first()
            .cloned()
            .unwrap_or_else(|| "feat".to_owned());
        let scope = (rules.message.scope == MessageScope::FeatureSlug)
            .then(|| slugify_branch_segment(feature_name, 32));
        return vec![CommitPlanGroupV1 {
            task_ids: Vec::new(),
            commit_type,
            scope,
            subject: truncate_subject(feature_name, 60),
            body: format!("Apply approved changes for {feature_name}."),
            trailers,
            paths,
            counted_files,
            counted_lines,
        }];
    }

    if rules.granularity == CommitGranularity::PerPulsation {
        let task_ids: Vec<String> = tasks.iter().map(|t| t.id.clone()).collect();
        let paths: Vec<String> = blobs.iter().map(|b| b.workspace_rel_path.clone()).collect();
        let (counted_files, counted_lines) = sum_counted(&paths, &blob_by_path);
        let first = &tasks[0];
        let commit_type = rules
            .message
            .type_by_task_kind
            .get(&first.kind)
            .cloned()
            .unwrap_or_else(|| "feat".to_owned());
        let scope = match rules.message.scope {
            MessageScope::FeatureSlug => Some(slugify_branch_segment(
                first.feature_title.as_deref().unwrap_or(feature_name),
                32,
            )),
            MessageScope::None => None,
        };
        let mut trailers = vec![format!("Ikore-Pulsation: {pulsation_id}")];
        for id in &task_ids {
            trailers.push(format!("Ikore-Task: {id}"));
        }
        if has_sealed_attestation {
            trailers.push(format!("Ikore-Attestation: {pulsation_id}"));
        }
        let body = tasks
            .iter()
            .map(|t| format!("- {}: {}", t.id, t.title))
            .collect::<Vec<_>>()
            .join("\n");
        return vec![CommitPlanGroupV1 {
            task_ids,
            commit_type,
            scope,
            subject: truncate_subject(feature_name, 60),
            body,
            trailers,
            paths,
            counted_files,
            counted_lines,
        }];
    }

    // Union-Find over task indices for shared files (`PerTask` granularity).
    let n = tasks.len();
    let idx_by_task: BTreeMap<&str, usize> = tasks
        .iter()
        .enumerate()
        .map(|(i, t)| (t.id.as_str(), i))
        .collect();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    fn union(parent: &mut [usize], a: usize, b: usize) {
        let ra = find(parent, a);
        let rb = find(parent, b);
        if ra != rb {
            if ra < rb {
                parent[rb] = ra;
            } else {
                parent[ra] = rb;
            }
        }
    }

    for owner_tasks in file_to_tasks.values() {
        let mapped: Vec<usize> = owner_tasks
            .iter()
            .filter_map(|id| idx_by_task.get(id.as_str()).copied())
            .collect();
        if let Some(&first) = mapped.first() {
            for &other in &mapped[1..] {
                union(&mut parent, first, other);
            }
        }
    }

    let mut comp_tasks: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..n {
        let root = find(&mut parent, i);
        comp_tasks.entry(root).or_default().push(i);
    }

    let mut comp_paths: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    let mut unowned_paths: Vec<String> = Vec::new();
    for blob in blobs {
        if let Some(owners) = file_to_tasks.get(&blob.workspace_rel_path)
            && let Some(first_task) = owners.first()
            && let Some(&t_idx) = idx_by_task.get(first_task.as_str())
        {
            let root = find(&mut parent, t_idx);
            comp_paths
                .entry(root)
                .or_default()
                .push(blob.workspace_rel_path.clone());
            continue;
        }
        unowned_paths.push(blob.workspace_rel_path.clone());
    }

    // Order components by earliest pulsation_tasks position.
    let mut ordered_roots: Vec<(i64, usize)> = comp_tasks
        .iter()
        .map(|(&root, indices)| {
            let min_pos = indices
                .iter()
                .map(|&i| tasks[i].position)
                .min()
                .unwrap_or(i64::MAX);
            (min_pos, root)
        })
        .collect();
    ordered_roots.sort();

    if rules.docs_placement == DocsPlacement::WithTasks
        && !unowned_paths.is_empty()
        && let Some(&(_, first_root)) = ordered_roots.first()
    {
        comp_paths
            .entry(first_root)
            .or_default()
            .append(&mut unowned_paths);
    }

    let mut groups = Vec::new();
    for (_, root) in ordered_roots {
        let Some(indices) = comp_tasks.get(&root) else {
            continue;
        };
        let paths = comp_paths.remove(&root).unwrap_or_default();
        if paths.is_empty() {
            continue;
        }
        let primary = &tasks[indices[0]];
        let commit_type = rules
            .message
            .type_by_task_kind
            .get(&primary.kind)
            .cloned()
            .unwrap_or_else(|| "feat".to_owned());
        let scope = match rules.message.scope {
            MessageScope::FeatureSlug => Some(slugify_branch_segment(
                primary.feature_title.as_deref().unwrap_or(feature_name),
                32,
            )),
            MessageScope::None => None,
        };
        let subject = if indices.len() == 1 {
            truncate_subject(&primary.title, 60)
        } else {
            truncate_subject(
                &format!("{} (+{} related tasks)", primary.title, indices.len() - 1),
                60,
            )
        };
        let mut body_parts = Vec::new();
        for &idx in indices {
            let t = &tasks[idx];
            if !t.objective.trim().is_empty() {
                body_parts.push(t.objective.trim().to_owned());
            } else {
                body_parts.push(t.title.trim().to_owned());
            }
            for c in &t.criteria {
                if !c.trim().is_empty() {
                    body_parts.push(format!("- {}", c.trim()));
                }
            }
        }
        let body = body_parts.join("\n");
        let task_ids: Vec<String> = indices.iter().map(|&i| tasks[i].id.clone()).collect();
        let mut trailers = vec![format!("Ikore-Pulsation: {pulsation_id}")];
        for tid in &task_ids {
            trailers.push(format!("Ikore-Task: {tid}"));
        }
        if has_sealed_attestation {
            trailers.push(format!("Ikore-Attestation: {pulsation_id}"));
        }
        let (counted_files, counted_lines) = sum_counted(&paths, &blob_by_path);
        groups.push(CommitPlanGroupV1 {
            task_ids,
            commit_type,
            scope,
            subject,
            body,
            trailers,
            paths,
            counted_files,
            counted_lines,
        });
    }

    if !unowned_paths.is_empty() {
        let mut trailers = vec![format!("Ikore-Pulsation: {pulsation_id}")];
        if has_sealed_attestation {
            trailers.push(format!("Ikore-Attestation: {pulsation_id}"));
        }
        let (counted_files, counted_lines) = sum_counted(&unowned_paths, &blob_by_path);
        let scope = (rules.message.scope == MessageScope::FeatureSlug)
            .then(|| slugify_branch_segment(feature_name, 32));
        let docs_type = if rules.message.types.iter().any(|t| t == "docs") {
            "docs".to_owned()
        } else {
            rules
                .message
                .types
                .first()
                .cloned()
                .unwrap_or_else(|| "chore".to_owned())
        };
        groups.push(CommitPlanGroupV1 {
            task_ids: Vec::new(),
            commit_type: docs_type,
            scope,
            subject: truncate_subject(&format!("update notes for {feature_name}"), 60),
            body: format!("Record documentation updates from pulsation {pulsation_id}."),
            trailers,
            paths: unowned_paths,
            counted_files,
            counted_lines,
        });
    }

    groups
}

fn sum_counted(paths: &[String], blob_by_path: &BTreeMap<&str, &PlannedBlob>) -> (usize, usize) {
    let mut files = 0usize;
    let mut lines = 0usize;
    for path in paths {
        if let Some(blob) = blob_by_path.get(path.as_str())
            && !blob.excluded_from_count
        {
            files += 1;
            lines += blob.added_lines + blob.removed_lines;
        }
    }
    (files, lines)
}

fn truncate_subject(raw: &str, max_chars: usize) -> String {
    let clean = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let first = clean.trim();
    if first.chars().count() <= max_chars {
        first.to_owned()
    } else {
        first.chars().take(max_chars).collect()
    }
}

pub(crate) fn ensure_branch_is_unprotected(
    branch: &str,
    protected: &[String],
) -> Result<(), RuleViolation> {
    if is_protected_branch(branch, protected) {
        Err(RuleViolation {
            code: violation_codes::PROTECTED_BRANCH.to_owned(),
            path: None,
            task_id: None,
            message: format!("Branch {branch} is protected and cannot be written."),
        })
    } else {
        Ok(())
    }
}
