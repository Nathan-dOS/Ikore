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
