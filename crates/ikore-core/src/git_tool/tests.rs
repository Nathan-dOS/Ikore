use std::process::Command;
use std::sync::Arc;

use tempfile::TempDir;

use super::process::{HostBinary, resolve_host_binary_in_path, validate_executable_path};
use super::rules::{canonical_rules_json_and_sha256, validate_commit_rules};
use super::status::{inspect_with_explicit_binaries, parse_gh_auth_status, scrub_remote_url};
use super::{
    CommitRulesV1, SaveProjectGitRulesOutcome, SaveProjectGitRulesRequest, fixed_git_rules,
};
use crate::app_core::{AppCore, AppCoreDependencies, AppPaths};
use crate::llm::{CredentialStore, LlmError};
use crate::secret_redactor::SecretRedactor;

#[derive(Debug, Default)]
struct NoopCredentialStore;

impl CredentialStore for NoopCredentialStore {
    fn get(&self, _credential_ref: &str) -> Result<Option<String>, LlmError> {
        Ok(None)
    }
    fn set(&self, _credential_ref: &str, _secret: &str) -> Result<(), LlmError> {
        Ok(())
    }
    fn delete(&self, _credential_ref: &str) -> Result<bool, LlmError> {
        Ok(false)
    }
}

fn init_test_core() -> (TempDir, Arc<AppCore>) {
    let dir = TempDir::new().unwrap();
    let deps = AppCoreDependencies {
        credentials: Arc::new(NoopCredentialStore),
        ..AppCoreDependencies::default()
    };
    let core = AppCore::initialize_with_dependencies(
        AppPaths::from_data_dir(dir.path()),
        tokio::runtime::Handle::current(),
        deps,
    )
    .unwrap();
    (dir, core)
}

#[test]
fn rules_validate_field_by_field_including_regex_and_task_kinds() {
    let valid = CommitRulesV1::default();
    assert!(validate_commit_rules(&valid).is_empty());

    let mut invalid = CommitRulesV1::default();
    invalid.message.pattern = "[unclosed".into();
    invalid.message.types = vec!["feat".into()]; // Missing types used by Bug/Refactor/...
    invalid.message.subject_max_chars = 15;
    invalid.message.body_wrap = 300;
    invalid.limits.max_changed_lines = 0;
    invalid.limits.max_files = 0;
    invalid.paths.forbidden = Vec::new();
    invalid.branch.pattern = "bad/{unknown}".into();
    invalid.push.remote = "-bad".into();
    invalid.pull.remote = "https://remote".into();

    let errors = validate_commit_rules(&invalid);
    assert!(
        errors
            .iter()
            .any(|e| e.field == "message.pattern" && e.code == "invalid_regex")
    );
    assert!(errors.iter().any(|e| e.field == "message.subjectMaxChars"));
    assert!(errors.iter().any(|e| e.field == "message.bodyWrap"));
    assert!(errors.iter().any(|e| e.field == "limits.maxChangedLines"));
    assert!(errors.iter().any(|e| e.field == "limits.maxFiles"));
    assert!(errors.iter().any(|e| e.field == "paths.forbidden"));
    assert!(errors.iter().any(|e| e.field == "branch.pattern"));
    assert!(errors.iter().any(|e| e.field == "push.remote"));
    assert!(errors.iter().any(|e| e.field == "pull.remote"));
    assert!(
        errors
            .iter()
            .any(|e| e.field == "message.typeByTaskKind.Bug")
    );
    for error in &errors {
        assert!(
            !error.message.contains('`'),
            "backtick in {}",
            error.message
        );
    }
}

#[tokio::test]
async fn saving_rules_versions_hashes_and_deletes_with_project() {
    let (_dir, core) = init_test_core();
    let workspace = TempDir::new().unwrap();
    let project = core
        .persistence()
        .create_project(crate::persistence_service::CreateProjectRequest {
            workspace_path: workspace.path().to_string_lossy().into_owned(),
            name: Some("Git M1".into()),
        })
        .unwrap()
        .project;

    let initial = core
        .persistence()
        .get_project_git_rules(&project.id)
        .unwrap();
    assert_eq!(initial.version, 0);
    assert!(!initial.enabled, "git tool must be disabled by default");
    assert_eq!(initial.rules_sha256.len(), 64);

    let mut invalid = CommitRulesV1::default();
    invalid.message.pattern = "(".into();
    let outcome = core
        .persistence()
        .save_project_git_rules(SaveProjectGitRulesRequest {
            project_id: project.id.clone(),
            enabled: true,
            rules: invalid,
        })
        .unwrap();
    assert!(matches!(outcome, SaveProjectGitRulesOutcome::Invalid(_)));

    let mut custom = CommitRulesV1::default();
    custom.limits.max_changed_lines = 650;
    custom.limits.exclude_from_count.push("custom.lock".into());
    let (expected_json, expected_sha) = canonical_rules_json_and_sha256(&custom).unwrap();
    assert!(expected_json.contains("custom.lock"));

    let saved = match core
        .persistence()
        .save_project_git_rules(SaveProjectGitRulesRequest {
            project_id: project.id.clone(),
            enabled: true,
            rules: custom.clone(),
        })
        .unwrap()
    {
        SaveProjectGitRulesOutcome::Saved(rec) => *rec,
        other => panic!("expected saved record, got {other:?}"),
    };
    assert_eq!(saved.version, 1);
    assert!(saved.enabled);
    assert_eq!(saved.rules_sha256, expected_sha);

    let restored = core
        .persistence()
        .restore_default_project_git_rules(&project.id)
        .unwrap();
    assert_eq!(restored.version, 2);
    assert!(restored.enabled, "restore defaults keeps enabled state");
    assert_eq!(restored.rules, CommitRulesV1::default());

    let versions = core
        .persistence()
        .list_project_git_rule_versions(&project.id)
        .unwrap();
    assert_eq!(versions.len(), 2);

    let outcome = core
        .delete_project_permanently(&project.id, &project.name)
        .unwrap();
    assert!(outcome.deleted);
    let db = core.persistence().connection().unwrap();
    let count: i64 = db
        .query_row("SELECT count(*) FROM project_git_rules", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn canary_tokens_never_appear_in_remote_url_or_gh_auth_status() {
    let redactor = SecretRedactor::new().unwrap();
    let remote_canary = "ghp_CANARY0123456789SecretRemoteToken";
    let bare_url = format!("https://{remote_canary}@github.com/Nathan-dOS/Ikore.git");
    let userpass_url = format!("https://oauth2:{remote_canary}@github.com/Nathan-dOS/Ikore.git");

    let scrubbed_bare = scrub_remote_url(&bare_url, &redactor);
    let scrubbed_userpass = scrub_remote_url(&userpass_url, &redactor);
    assert!(!scrubbed_bare.contains(remote_canary));
    assert!(!scrubbed_userpass.contains(remote_canary));
    assert_eq!(scrubbed_bare, "https://***@github.com/Nathan-dOS/Ikore.git");
    assert_eq!(
        scrubbed_userpass,
        "https://***@github.com/Nathan-dOS/Ikore.git"
    );

    let gh_canary = "gho_CANARY0123456789GitHubCliToken";
    let pat_canary = "github_pat_CANARY0123456789PatValue";
    let fake_gh_output = format!(
        "github.com\n  ✓ Logged in to github.com account Nathan-dOS (keyring)\n  - Token: {gh_canary}\n  - Extra: {pat_canary}\n"
    );
    let parsed = parse_gh_auth_status(&fake_gh_output, true, &redactor);
    assert!(parsed.authenticated);
    assert_eq!(parsed.account.as_deref(), Some("Nathan-dOS"));
    assert!(!parsed.summary.contains(gh_canary));
    assert!(!parsed.summary.contains(pat_canary));
}

#[test]
fn wrapper_scripts_and_shortcuts_are_rejected_and_fixed_rules_have_no_backticks() {
    let dir = TempDir::new().unwrap();
    for ext in ["cmd", "bat", "ps1", "lnk"] {
        let wrapper = dir.path().join(format!("git.{ext}"));
        std::fs::write(&wrapper, "@echo off\n").unwrap();
        let err = validate_executable_path(&wrapper).unwrap_err();
        assert!(
            err.to_string().contains("Wrapper scripts"),
            "expected wrapper error for .{ext}, got: {err}"
        );
    }
    let path_err =
        resolve_host_binary_in_path(HostBinary::Git, dir.path().as_os_str()).unwrap_err();
    assert!(path_err.to_string().contains("Wrapper scripts"));

    for rule in fixed_git_rules() {
        assert!(!rule.title.contains('`'));
        assert!(!rule.description.contains('`'));
    }
}

#[tokio::test]
async fn inspect_git_status_reads_real_repo_without_writing_anything() {
    let repo_dir = TempDir::new().unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-b", "feature/status-check"])
            .current_dir(repo_dir.path())
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .args(["config", "user.name", "QA Tester"])
            .current_dir(repo_dir.path())
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .args(["config", "user.email", "qa@example.com"])
            .current_dir(repo_dir.path())
            .status()
            .unwrap()
            .success()
    );
    let canary = "ghp_CANARY9876543210NeverExpose";
    assert!(
        Command::new("git")
            .args([
                "remote",
                "add",
                "origin",
                &format!("https://oauth2:{canary}@github.com/Nathan-dOS/Ikore.git"),
            ])
            .current_dir(repo_dir.path())
            .status()
            .unwrap()
            .success()
    );

    let missing_gh = repo_dir.path().join("no-such-gh.exe");
    let status =
        inspect_with_explicit_binaries(repo_dir.path(), "origin", None, Some(&missing_gh)).await;

    assert!(status.git.available);
    assert!(status.git.sha256.as_ref().is_some_and(|s| s.len() == 64));
    assert!(status.in_git_repository);
    assert_eq!(
        status.current_branch.as_deref(),
        Some("feature/status-check")
    );
    assert_eq!(status.user_name.as_deref(), Some("QA Tester"));
    assert_eq!(status.user_email.as_deref(), Some("qa@example.com"));
    let serialized = serde_json::to_string(&status).unwrap();
    assert!(
        !serialized.contains(canary),
        "canary leaked into GitStatusV1"
    );
    assert_eq!(
        status.remote_url.as_deref(),
        Some("https://***@github.com/Nathan-dOS/Ikore.git")
    );
    assert!(!status.gh.available);
}

fn run_git_cmd(repo: &std::path::Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("run git command");
    assert!(
        out.status.success(),
        "git {:?} failed: stdout={}, stderr={}",
        args,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn init_real_repo_with_initial_files(files: &[(&str, &str)]) -> TempDir {
    let dir = TempDir::new().unwrap();
    run_git_cmd(dir.path(), &["init", "-b", "main"]);
    run_git_cmd(dir.path(), &["config", "user.name", "QA Tester"]);
    run_git_cmd(dir.path(), &["config", "user.email", "qa@example.com"]);
    run_git_cmd(dir.path(), &["config", "core.autocrlf", "false"]);
    for (rel, body) in files {
        let full = dir.path().join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&full, body).unwrap();
    }
    run_git_cmd(dir.path(), &["add", "--all"]);
    run_git_cmd(dir.path(), &["commit", "-m", "chore: initial commit"]);
    dir
}

struct SeedTask<'a> {
    id: &'a str,
    position: i64,
    title: &'a str,
    kind: &'a str,
}

struct SeedArtifact<'a> {
    id: &'a str,
    rel_path: &'a str,
    content: &'a str,
    base_content: Option<&'a str>,
    stage: &'a str,
    job_id: &'a str,
    output_position: i64,
    task_ids: &'a [&'a str],
}

fn sha256_str(s: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn create_test_project(
    core: &AppCore,
    name: &str,
    repo_path: &std::path::Path,
) -> crate::persistence_service::Project {
    core.persistence()
        .create_project(crate::persistence_service::CreateProjectRequest {
            workspace_path: repo_path.to_string_lossy().into_owned(),
            name: Some(name.to_owned()),
        })
        .unwrap()
        .project
}

fn seed_done_pulsation_for_git(
    core: &AppCore,
    project_id: &str,
    pulsation_id: &str,
    feature_name: &str,
    completed_at: &str,
    tasks: &[SeedTask<'_>],
    artifacts: &[SeedArtifact<'_>],
) {
    use rusqlite::params;
    let db = core.persistence().connection().unwrap();
    db.execute(
        "INSERT INTO pulsations(id, project_id, feature_name, stage, status, created_at, updated_at, queue_sequence)
         VALUES (?1, ?2, ?3, 'Integrator', 'done', ?4, ?4, (SELECT COALESCE(MAX(queue_sequence),0)+1 FROM pulsations WHERE project_id=?2))",
        params![pulsation_id, project_id, feature_name, completed_at],
    )
    .unwrap();

    for t in tasks {
        let draft_json = serde_json::json!({
            "title": t.title,
            "kind": t.kind,
            "objective": format!("Implement {}", t.title),
            "acceptanceCriteria": [format!("Verify {}", t.title)],
        })
        .to_string();
        db.execute(
            "INSERT INTO work_tasks(id, project_id, revision, draft_json, state, completed_pulsation_id, created_at, updated_at)
             VALUES (?1, ?2, 1, ?3, 'done', ?4, ?5, ?5)",
            params![t.id, project_id, draft_json, pulsation_id, completed_at],
        )
        .unwrap();
        db.execute(
            "INSERT INTO pulsation_tasks(pulsation_id, task_id, revision, position) VALUES (?1, ?2, 1, ?3)",
            params![pulsation_id, t.id, t.position],
        )
        .unwrap();
    }

    let mut jobs_inserted = std::collections::BTreeSet::new();
    let mut task_job_positions: std::collections::BTreeMap<(&str, &str, &str), Vec<i64>> =
        std::collections::BTreeMap::new();
    let mut base_seen = std::collections::BTreeSet::new();

    for (idx, a) in artifacts.iter().enumerate() {
        if jobs_inserted.insert(a.job_id) {
            db.execute(
                "INSERT INTO jobs(id, pulsation_id, kind, status, created_at, updated_at, project_id, input_revision, generation, state_version, job_class, idempotency_key)
                 VALUES (?1, ?2, 'stage_run', 'done', ?3, ?3, ?4, 1, 1, 1, 'main', ?5)",
                params![a.job_id, pulsation_id, completed_at, project_id, format!("idem:{pulsation_id}:{}", a.job_id)],
            )
            .unwrap();
        }
        let desired_sha = sha256_str(a.content);
        let base_sha = a.base_content.map(sha256_str);
        db.execute(
            "INSERT INTO artifacts(
                id, pulsation_id, name, kind, relative_path, content_sha256, created_at, updated_at,
                content, producer_job_id, producer_stage, generation, input_revision,
                producer_state_version, output_position, acceptance, publication_state
             ) VALUES (?1, ?2, ?3, 'file', ?3, ?4, ?5, ?5, ?6, ?7, ?8, 1, 1, 1, ?9, 'accepted', 'applied')",
            params![
                a.id,
                pulsation_id,
                a.rel_path,
                desired_sha,
                completed_at,
                a.content,
                a.job_id,
                a.stage,
                a.output_position
            ],
        )
        .unwrap();
        db.execute(
            "INSERT INTO artifact_publications(
                artifact_id, project_id, pulsation_id, relative_path, expected_sha256, desired_sha256, state, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'applied', ?7)",
            params![a.id, project_id, pulsation_id, a.rel_path, base_sha, desired_sha, completed_at],
        )
        .unwrap();

        let path_key = a.rel_path.to_ascii_lowercase();
        if base_seen.insert(path_key.clone()) {
            let (state, sha, text) = match a.base_content {
                Some(txt) => ("present", Some(sha256_str(txt)), Some(txt)),
                None => ("absent", None, None),
            };
            db.execute(
                "INSERT INTO pulsation_file_bases(
                    pulsation_id, path_key, relative_path, state, content_sha256, content, first_artifact_id, captured_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![pulsation_id, path_key, a.rel_path, state, sha, text, a.id, completed_at],
            )
            .unwrap();
        }
        for tid in a.task_ids {
            task_job_positions
                .entry((a.job_id, a.stage, *tid))
                .or_default()
                .push(a.output_position);
        }
        let _ = idx;
    }

    for ((job_id, stage, tid), positions) in task_job_positions {
        let ev_json = serde_json::json!({ "artifactPositions": positions }).to_string();
        db.execute(
            "INSERT INTO task_evidence(pulsation_id, job_id, generation, stage, task_id, evidence_json)
             VALUES (?1, ?2, 1, ?3, ?4, ?5)",
            params![pulsation_id, job_id, stage, tid, ev_json],
        )
        .unwrap();
    }
}

#[tokio::test]
async fn commit_plan_groups_tasks_merges_shared_file_preserves_dirty_worktree_and_builds_report() {
    let repo = init_real_repo_with_initial_files(&[
        ("README.md", "# Demo\n"),
        ("src/shared.rs", "pub fn shared() -> i32 { 1 }\n"),
        ("Cargo.lock", "lock = 1\n"),
    ]);
    // Local untracked worktree change must remain untouched across plumbing commits.
    std::fs::write(repo.path().join("dirty-local.txt"), "do not stage\n").unwrap();

    let (_dir, core) = init_test_core();
    let project = create_test_project(&core, "Git M2 Demo", repo.path());
    let rules_outcome = core
        .save_project_git_rules(SaveProjectGitRulesRequest {
            project_id: project.id.clone(),
            enabled: true,
            rules: CommitRulesV1::default(),
        })
        .unwrap();
    assert!(matches!(
        rules_outcome,
        SaveProjectGitRulesOutcome::Saved(_)
    ));

    seed_done_pulsation_for_git(
        &core,
        &project.id,
        "pulse-m2-001",
        "Task grouping and docs",
        "2026-11-26T01:00:00Z",
        &[
            SeedTask {
                id: "task-a",
                position: 1,
                title: "add shared parser",
                kind: "Feature",
            },
            SeedTask {
                id: "task-b",
                position: 2,
                title: "fix edge case in shared parser",
                kind: "Bug",
            },
            SeedTask {
                id: "task-c",
                position: 3,
                title: "extract helper module",
                kind: "Refactor",
            },
        ],
        &[
            SeedArtifact {
                id: "art-1",
                rel_path: "src/shared.rs",
                content: "pub fn shared() -> i32 { 42 }\n",
                base_content: Some("pub fn shared() -> i32 { 1 }\n"),
                stage: "Code",
                job_id: "job-code-1",
                output_position: 0,
                task_ids: &["task-a", "task-b"],
            },
            SeedArtifact {
                id: "art-2",
                rel_path: "tests/shared_test.rs",
                content: "#[test]\nfn shared_ok() { assert_eq!(42, 42); }\n",
                base_content: None,
                stage: "Test",
                job_id: "job-test-1",
                output_position: 1,
                task_ids: &["task-a"],
            },
            SeedArtifact {
                id: "art-3",
                rel_path: "Cargo.lock",
                content: "lock = 1\n+added\n",
                base_content: Some("lock = 1\n"),
                stage: "Code",
                job_id: "job-code-1",
                output_position: 2,
                task_ids: &["task-b"],
            },
            SeedArtifact {
                id: "art-4",
                rel_path: "src/helper.rs",
                content: "pub fn help() {}\n",
                base_content: None,
                stage: "Code",
                job_id: "job-code-2",
                output_position: 0,
                task_ids: &["task-c"],
            },
            SeedArtifact {
                id: "art-5",
                rel_path: "tests/helper_test.rs",
                content: "#[test]\nfn help_ok() {}\n",
                base_content: None,
                stage: "Test",
                job_id: "job-test-2",
                output_position: 1,
                task_ids: &["task-c"],
            },
            SeedArtifact {
                id: "art-6",
                rel_path: "docs/notes.md",
                content: "# Release notes\n",
                base_content: None,
                stage: "Integrator",
                job_id: "job-int-1",
                output_position: 0,
                task_ids: &[],
            },
        ],
    );

    let preview = core
        .preview_git_commit(&project.id, "pulse-m2-001")
        .await
        .unwrap();
    assert!(
        preview.plan.blocking_violations.is_empty(),
        "unexpected blocking violations: {:?}",
        preview.plan.blocking_violations
    );
    assert!(!preview.already_in_head);
    // T1 + T2 merged (shared file), T3 separate, trailing docs separate => 3 commits.
    assert_eq!(preview.plan.groups.len(), 3);
    assert_eq!(preview.plan.groups[0].task_ids, vec!["task-a", "task-b"]);
    assert_eq!(preview.plan.groups[1].task_ids, vec!["task-c"]);
    assert!(preview.plan.groups[2].task_ids.is_empty());
    assert_eq!(preview.plan.groups[2].commit_type, "docs");
    // Cargo.lock excluded from counted_files.
    assert!(!preview.plan.groups[0].paths.is_empty());
    assert_eq!(preview.plan.groups[0].counted_files, 2);

    // Simulate publication having written accepted artifacts to the workspace on disk, plus a dirty tracked edit.
    for (rel, body) in [
        ("src/shared.rs", "pub fn shared() -> i32 { 42 }\n"),
        (
            "tests/shared_test.rs",
            "#[test]\nfn shared_ok() { assert_eq!(42, 42); }\n",
        ),
        ("Cargo.lock", "lock = 1\n+added\n"),
        ("src/helper.rs", "pub fn help() {}\n"),
        ("tests/helper_test.rs", "#[test]\nfn help_ok() {}\n"),
        ("docs/notes.md", "# Release notes\n"),
    ] {
        let p = repo.path().join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, body).unwrap();
    }
    std::fs::write(repo.path().join("README.md"), "# Demo dirty local\n").unwrap();

    // Enqueue and execute via AppCore.
    {
        let db = core.persistence().connection().unwrap();
        super::queue::enqueue_done_pulsation_in_tx(
            &db,
            &project.id,
            "pulse-m2-001",
            "2026-11-26T01:00:00Z",
        )
        .unwrap();
    }
    let outcome = core
        .execute_git_commit(&project.id, "pulse-m2-001", None)
        .await
        .unwrap();
    assert_eq!(outcome.status, super::GitOperationStatus::Succeeded);
    let receipt = outcome.receipt.expect("commit receipt");
    assert_eq!(receipt.commits.len(), 3);
    assert!(receipt.branch.starts_with("ikore/pulse-m2-"));
    assert!(
        !receipt.commits[0]
            .message
            .to_ascii_lowercase()
            .contains("co-authored-by")
    );
    assert!(
        receipt.commits[0]
            .message
            .contains("Ikore-Pulsation: pulse-m2-001")
    );

    // Target branch has 3 new commits above main; main stayed at initial commit; dirty files preserved.
    let main_count = run_git_cmd(repo.path(), &["rev-list", "--count", "main"]);
    let branch_count = run_git_cmd(repo.path(), &["rev-list", "--count", &receipt.branch]);
    assert_eq!(main_count, "1");
    assert_eq!(branch_count, "4");
    assert_eq!(
        std::fs::read_to_string(repo.path().join("dirty-local.txt")).unwrap(),
        "do not stage\n"
    );
    assert_eq!(
        std::fs::read_to_string(repo.path().join("README.md")).unwrap(),
        "# Demo dirty local\n"
    );
    let porcelain = run_git_cmd(repo.path(), &["status", "--porcelain"]);
    assert!(
        porcelain
            .lines()
            .all(|l| l.starts_with("?? dirty-local.txt") || l.trim() == "M README.md"),
        "unexpected git status after plumbing commit: {porcelain}"
    );

    // Pulsation report includes commit metadata (#git-commit).
    let report = core
        .persistence()
        .build_pulsation_report("pulse-m2-001")
        .unwrap();
    let git_report = report
        .git_commit
        .as_ref()
        .expect("git_commit section in report");
    assert_eq!(git_report.branch, receipt.branch);
    let html = crate::reports::render_pulsation_report(&report);
    assert!(html.contains("id=\"git-commit\""));
}

#[tokio::test]
async fn commit_plan_respects_autocrlf_and_gitattributes_without_base_mismatch() {
    let repo = init_real_repo_with_initial_files(&[
        (".gitattributes", "* text=auto\n"),
        ("src/win.rs", "fn win() {\n    println!(\"ok\");\n}\n"),
    ]);
    run_git_cmd(repo.path(), &["config", "core.autocrlf", "true"]);

    let (_dir, core) = init_test_core();
    let project = create_test_project(&core, "Git CRLF Demo", repo.path());
    core.save_project_git_rules(SaveProjectGitRulesRequest {
        project_id: project.id.clone(),
        enabled: true,
        rules: CommitRulesV1::default(),
    })
    .unwrap();

    seed_done_pulsation_for_git(
        &core,
        &project.id,
        "pulse-crlf",
        "Windows line endings",
        "2026-11-26T01:10:00Z",
        &[SeedTask {
            id: "t-win",
            position: 1,
            title: "update win helper",
            kind: "Feature",
        }],
        &[
            SeedArtifact {
                id: "art-w1",
                rel_path: "src/win.rs",
                content: "fn win() {\r\n    println!(\"updated\");\r\n}\r\n",
                base_content: Some("fn win() {\r\n    println!(\"ok\");\r\n}\r\n"),
                stage: "Code",
                job_id: "job-w1",
                output_position: 0,
                task_ids: &["t-win"],
            },
            SeedArtifact {
                id: "art-w2",
                rel_path: "tests/win_test.rs",
                content: "#[test]\nfn ok() {}\n",
                base_content: None,
                stage: "Test",
                job_id: "job-w2",
                output_position: 1,
                task_ids: &["t-win"],
            },
        ],
    );
    {
        let db = core.persistence().connection().unwrap();
        super::queue::enqueue_done_pulsation_in_tx(
            &db,
            &project.id,
            "pulse-crlf",
            "2026-11-26T01:10:00Z",
        )
        .unwrap();
    }
    let outcome = core
        .execute_git_commit(&project.id, "pulse-crlf", None)
        .await
        .unwrap();
    assert_eq!(outcome.status, super::GitOperationStatus::Succeeded);
}

#[tokio::test]
async fn blocking_violations_and_secret_canary_leave_repository_untouched() {
    let repo = init_real_repo_with_initial_files(&[("README.md", "# Base\n")]);
    let head_before = run_git_cmd(repo.path(), &["rev-parse", "HEAD"]);
    let canary = "ghp_CANARY0123456789SecretInArtifact";

    let (_dir, core) = init_test_core();
    let project = create_test_project(&core, "Git Block Demo", repo.path());
    let mut rules = CommitRulesV1::default();
    rules.branch.mode = super::BranchMode::CurrentBranch; // points at protected "main"
    core.save_project_git_rules(SaveProjectGitRulesRequest {
        project_id: project.id.clone(),
        enabled: true,
        rules,
    })
    .unwrap();

    seed_done_pulsation_for_git(
        &core,
        &project.id,
        "pulse-blocked",
        "Blocked commit",
        "2026-11-26T01:20:00Z",
        &[SeedTask {
            id: "t-blk",
            position: 1,
            title: "touch code without tests",
            kind: "Feature",
        }],
        &[SeedArtifact {
            id: "art-blk",
            rel_path: "src/secret.rs",
            content: &format!("pub const TOKEN: &str = \"{canary}\";\n"),
            base_content: None,
            stage: "Code",
            job_id: "job-blk",
            output_position: 0,
            task_ids: &["t-blk"],
        }],
    );
    {
        let db = core.persistence().connection().unwrap();
        super::queue::enqueue_done_pulsation_in_tx(
            &db,
            &project.id,
            "pulse-blocked",
            "2026-11-26T01:20:00Z",
        )
        .unwrap();
    }

    let outcome = core
        .execute_git_commit(&project.id, "pulse-blocked", None)
        .await
        .unwrap();
    assert_eq!(outcome.status, super::GitOperationStatus::Blocked);
    let codes: std::collections::BTreeSet<&str> =
        outcome.violations.iter().map(|v| v.code.as_str()).collect();
    assert!(codes.contains("protected_branch"));
    assert!(codes.contains("missing_tests"));
    assert!(codes.contains("secret_detected"));
    let serialized = serde_json::to_string(&outcome).unwrap();
    assert!(
        !serialized.contains(canary),
        "secret canary leaked into outcome"
    );

    let head_after = run_git_cmd(repo.path(), &["rev-parse", "HEAD"]);
    assert_eq!(head_before, head_after);
    let ops = core.list_git_operations(&project.id, 10).unwrap();
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].status, super::GitOperationStatus::Blocked);
}

#[tokio::test]
async fn p5_backfill_p6_already_in_head_p7_skip_and_g3_user_staged_preserved() {
    let already_src = "pub fn already() -> i32 { 7 }\n";
    let already_test_src = "#[test]\nfn already_ok() {}\n";
    let repo = init_real_repo_with_initial_files(&[
        ("src/already.rs", already_src),
        ("tests/already_test.rs", already_test_src),
        ("src/staged.rs", "pub fn staged() -> i32 { 0 }\n"),
    ]);

    let (_dir, core) = init_test_core();
    let project = create_test_project(&core, "Queue And P5/P6/P7", repo.path());

    // Two pulsations completed BEFORE enabling git tool (P5 backfill test).
    seed_done_pulsation_for_git(
        &core,
        &project.id,
        "pulse-order-1",
        "First done pulsation",
        "2026-11-26T00:01:00Z",
        &[SeedTask {
            id: "t-o1",
            position: 1,
            title: "first work",
            kind: "Feature",
        }],
        &[
            SeedArtifact {
                id: "art-o1",
                rel_path: "src/staged.rs",
                content: "pub fn staged() -> i32 { 1 }\n",
                base_content: Some("pub fn staged() -> i32 { 0 }\n"),
                stage: "Code",
                job_id: "job-o1",
                output_position: 0,
                task_ids: &["t-o1"],
            },
            SeedArtifact {
                id: "art-o1-t",
                rel_path: "tests/staged_test.rs",
                content: "#[test] fn t() {}\n",
                base_content: None,
                stage: "Test",
                job_id: "job-o1-t",
                output_position: 1,
                task_ids: &["t-o1"],
            },
        ],
    );
    seed_done_pulsation_for_git(
        &core,
        &project.id,
        "pulse-order-2",
        "Second already in head",
        "2026-11-26T00:02:00Z",
        &[SeedTask {
            id: "t-o2",
            position: 1,
            title: "already landed",
            kind: "Feature",
        }],
        &[
            SeedArtifact {
                id: "art-o2",
                rel_path: "src/already.rs",
                content: already_src,
                base_content: Some("pub fn old() -> i32 { 0 }\n"),
                stage: "Code",
                job_id: "job-o2",
                output_position: 0,
                task_ids: &["t-o2"],
            },
            SeedArtifact {
                id: "art-o2-t",
                rel_path: "tests/already_test.rs",
                content: already_test_src,
                base_content: None,
                stage: "Test",
                job_id: "job-o2-t",
                output_position: 1,
                task_ids: &["t-o2"],
            },
        ],
    );

    core.save_project_git_rules(SaveProjectGitRulesRequest {
        project_id: project.id.clone(),
        enabled: true,
        rules: CommitRulesV1::default(),
    })
    .unwrap();

    let queue = core.list_pulsation_commit_queue(&project.id).unwrap();
    assert_eq!(queue.len(), 2);
    assert_eq!(queue[0].pulsation_id, "pulse-order-1");
    assert_eq!(queue[1].pulsation_id, "pulse-order-2");

    // Out-of-order commit of second pulsation is blocked while first is pending.
    let ooo = core
        .execute_git_commit(&project.id, "pulse-order-2", None)
        .await
        .unwrap();
    assert_eq!(ooo.status, super::GitOperationStatus::Blocked);
    assert!(
        ooo.violations
            .iter()
            .any(|v| v.code == super::violation_codes::EARLIER_PULSATION_PENDING)
    );

    // G3 test: user stages a custom edit on src/staged.rs before committing pulse-order-1.
    std::fs::write(
        repo.path().join("src/staged.rs"),
        "pub fn staged() -> i32 { 999 }\n",
    )
    .unwrap();
    run_git_cmd(repo.path(), &["add", "src/staged.rs"]);
    let user_staged_blob = run_git_cmd(repo.path(), &["ls-files", "-s", "--", "src/staged.rs"]);

    let first_outcome = core
        .execute_git_commit(&project.id, "pulse-order-1", None)
        .await
        .unwrap();
    assert_eq!(first_outcome.status, super::GitOperationStatus::Succeeded);
    assert!(
        first_outcome
            .violations
            .iter()
            .any(|v| v.code == "index_modified_by_user")
    );
    // Developer's staged blob stayed untouched in real index.
    let index_after = run_git_cmd(repo.path(), &["ls-files", "-s", "--", "src/staged.rs"]);
    assert_eq!(user_staged_blob, index_after);

    // P6 test: second pulsation matches HEAD content -> marks skipped(already_in_head) without creating commits.
    let head_before_p2 = run_git_cmd(repo.path(), &["rev-parse", "HEAD"]);
    let second_outcome = core
        .execute_git_commit(&project.id, "pulse-order-2", None)
        .await
        .unwrap();
    assert_eq!(second_outcome.status, super::GitOperationStatus::Succeeded);
    assert!(second_outcome.skipped_already_in_head);
    let head_after = run_git_cmd(repo.path(), &["rev-parse", "HEAD"]);
    assert_eq!(head_before_p2, head_after);
    assert_eq!(
        core.persistence()
            .get_pulsation_commit_state("pulse-order-2")
            .unwrap()
            .as_deref(),
        Some("skipped")
    );

    // P7 skip test on a third pulsation.
    seed_done_pulsation_for_git(
        &core,
        &project.id,
        "pulse-order-3",
        "Skipped pulsation",
        "2026-11-26T00:03:00Z",
        &[],
        &[],
    );
    {
        let db = core.persistence().connection().unwrap();
        super::queue::enqueue_done_pulsation_in_tx(
            &db,
            &project.id,
            "pulse-order-3",
            "2026-11-26T00:03:00Z",
        )
        .unwrap();
    }
    let skipped = core
        .skip_pulsation_commit(super::SkipPulsationCommitRequest {
            project_id: project.id.clone(),
            pulsation_id: "pulse-order-3".into(),
            reason: "Landed manually upstream".into(),
        })
        .unwrap();
    assert_eq!(skipped.state, super::PulsationCommitQueueState::Skipped);
    assert_eq!(
        skipped.skip_reason.as_deref(),
        Some("Landed manually upstream")
    );
}
