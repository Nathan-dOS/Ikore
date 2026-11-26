//! Strictly read-only inspection of git and GitHub CLI readiness for Settings › Git
//! (Feature 08 M1). Scrubs URL userinfo credentials and strips `gh auth status`
//! tokens so secrets never reach the UI, database logs, or model context.

use std::path::Path;

use crate::secret_redactor::SecretRedactor;

use super::process::{
    HostBinary, ResolvedHostBinary, STATUS_TIMEOUT_SECS, os_args, resolve_host_binary,
    run_host_command, validate_executable_path,
};
use super::{GitStatusV1, ToolBinaryStatus};

/// Reads local git and `gh` readiness for `workspace_path` without modifying
/// the repository, index, or config.
pub async fn inspect_git_status_for_path(workspace_path: &Path, remote_name: &str) -> GitStatusV1 {
    let git_bin = resolve_host_binary(HostBinary::Git);
    let gh_bin = resolve_host_binary(HostBinary::Gh);
    inspect_with_resolved(workspace_path, remote_name, git_bin, gh_bin).await
}

pub async fn inspect_with_explicit_binaries(
    workspace_path: &Path,
    remote_name: &str,
    git_override: Option<&Path>,
    gh_override: Option<&Path>,
) -> GitStatusV1 {
    let git_bin = match git_override {
        Some(path) => validate_executable_path(path),
        None => resolve_host_binary(HostBinary::Git),
    };
    let gh_bin = match gh_override {
        Some(path) => validate_executable_path(path),
        None => resolve_host_binary(HostBinary::Gh),
    };
    inspect_with_resolved(workspace_path, remote_name, git_bin, gh_bin).await
}

async fn inspect_with_resolved(
    workspace_path: &Path,
    remote_name: &str,
    git_resolved: Result<ResolvedHostBinary, super::GitToolError>,
    gh_resolved: Result<ResolvedHostBinary, super::GitToolError>,
) -> GitStatusV1 {
    let remote = if remote_name.trim().is_empty() {
        "origin".to_owned()
    } else {
        remote_name.trim().to_owned()
    };
    let redactor = match SecretRedactor::new() {
        Ok(r) => r,
        Err(error) => {
            return unavailable_status(
                workspace_path,
                &remote,
                format!("Secret redactor unavailable: {error}"),
            );
        }
    };

    let cwd = if workspace_path.is_dir() {
        workspace_path
    } else {
        Path::new(".")
    };

    let mut git_status = ToolBinaryStatus {
        available: false,
        executable_path: None,
        sha256: None,
        version: None,
        error: None,
    };
    let mut in_git_repository = false;
    let mut repository_root = None;
    let mut user_name = None;
    let mut user_email = None;
    let mut current_branch = None;
    let mut detached_head = false;
    let mut remote_url = None;
    let mut credential_helper = None;

    match git_resolved {
        Ok(binary) => {
            git_status.executable_path = Some(binary.path.to_string_lossy().into_owned());
            git_status.sha256 = Some(binary.sha256.clone());
            if let Ok(out) = run_host_command(
                &binary,
                cwd,
                &os_args(&["--version"]),
                true,
                STATUS_TIMEOUT_SECS,
                &redactor,
            )
            .await
                && out.success
            {
                git_status.available = true;
                git_status.version = first_non_empty_line(&out.stdout);
            } else {
                git_status.error = Some("Unable to query git --version.".into());
            }

            if git_status.available && workspace_path.is_dir() {
                if let Ok(top) = run_host_command(
                    &binary,
                    workspace_path,
                    &os_args(&["rev-parse", "--show-toplevel"]),
                    true,
                    STATUS_TIMEOUT_SECS,
                    &redactor,
                )
                .await
                    && top.success
                {
                    in_git_repository = true;
                    repository_root = first_non_empty_line(&top.stdout);
                }

                user_name = read_git_config(&binary, workspace_path, "user.name", &redactor).await;
                user_email =
                    read_git_config(&binary, workspace_path, "user.email", &redactor).await;
                credential_helper =
                    read_git_config(&binary, workspace_path, "credential.helper", &redactor)
                        .await
                        .map(|val| redactor.redact(&val).text);

                if in_git_repository {
                    if let Ok(branch_out) = run_host_command(
                        &binary,
                        workspace_path,
                        &os_args(&["symbolic-ref", "--short", "HEAD"]),
                        true,
                        STATUS_TIMEOUT_SECS,
                        &redactor,
                    )
                    .await
                    {
                        if branch_out.success {
                            current_branch = first_non_empty_line(&branch_out.stdout);
                        } else if let Ok(rev_out) = run_host_command(
                            &binary,
                            workspace_path,
                            &os_args(&["rev-parse", "--verify", "HEAD"]),
                            true,
                            STATUS_TIMEOUT_SECS,
                            &redactor,
                        )
                        .await
                            && rev_out.success
                        {
                            detached_head = true;
                        }
                    }

                    if let Ok(remote_out) = run_host_command(
                        &binary,
                        workspace_path,
                        &os_args(&["remote", "get-url", &remote]),
                        true,
                        STATUS_TIMEOUT_SECS,
                        &redactor,
                    )
                    .await
                        && remote_out.success
                        && let Some(raw_url) = first_non_empty_line(&remote_out.stdout)
                    {
                        remote_url = Some(scrub_remote_url(&raw_url, &redactor));
                    }
                }
            }
        }
        Err(error) => {
            git_status.error = Some(error.to_string());
        }
    }

    let mut gh_status = ToolBinaryStatus {
        available: false,
        executable_path: None,
        sha256: None,
        version: None,
        error: None,
    };
    let mut gh_authenticated = false;
    let mut gh_account = None;
    let mut gh_status_summary = "GitHub CLI (gh) not found on PATH.".to_owned();

    match gh_resolved {
        Ok(binary) => {
            gh_status.executable_path = Some(binary.path.to_string_lossy().into_owned());
            gh_status.sha256 = Some(binary.sha256.clone());
            if let Ok(out) = run_host_command(
                &binary,
                cwd,
                &os_args(&["--version"]),
                true,
                STATUS_TIMEOUT_SECS,
                &redactor,
            )
            .await
                && out.success
            {
                gh_status.available = true;
                gh_status.version = first_non_empty_line(&out.stdout);
            } else {
                gh_status.error = Some("Unable to query gh --version.".into());
            }

            if gh_status.available {
                match run_host_command(
                    &binary,
                    cwd,
                    &os_args(&["auth", "status"]),
                    true,
                    STATUS_TIMEOUT_SECS,
                    &redactor,
                )
                .await
                {
                    Ok(auth_out) => {
                        let combined = format!("{}\n{}", auth_out.stdout, auth_out.stderr);
                        let parsed = parse_gh_auth_status(&combined, auth_out.success, &redactor);
                        gh_authenticated = parsed.authenticated;
                        gh_account = parsed.account;
                        gh_status_summary = parsed.summary;
                    }
                    Err(error) => {
                        gh_status_summary = redactor.redact(&error.to_string()).text;
                    }
                }
            }
        }
        Err(error) => {
            gh_status.error = Some(error.to_string());
        }
    }

    let mut setup_hints = Vec::new();
    if !git_status.available {
        setup_hints.push("Install Git and ensure git is available on PATH.".to_owned());
    }
    if user_name.is_none() {
        setup_hints.push("git config --global user.name \"Your Name\"".to_owned());
    }
    if user_email.is_none() {
        setup_hints.push("git config --global user.email \"you@example.com\"".to_owned());
    }
    if !gh_status.available {
        setup_hints.push("Optional: install GitHub CLI (gh) for GitHub remotes.".to_owned());
    } else if !gh_authenticated {
        setup_hints.push("gh auth login".to_owned());
        setup_hints.push("gh auth setup-git".to_owned());
    } else if credential_helper.is_none() {
        setup_hints.push("gh auth setup-git".to_owned());
    }

    GitStatusV1 {
        workspace_path: workspace_path.to_string_lossy().into_owned(),
        in_git_repository,
        repository_root,
        git: git_status,
        gh: gh_status,
        user_name,
        user_email,
        current_branch,
        detached_head,
        remote_name: remote,
        remote_url,
        credential_helper,
        gh_authenticated,
        gh_account,
        gh_status_summary,
        setup_hints,
    }
}

fn unavailable_status(workspace_path: &Path, remote_name: &str, message: String) -> GitStatusV1 {
    GitStatusV1 {
        workspace_path: workspace_path.to_string_lossy().into_owned(),
        in_git_repository: false,
        repository_root: None,
        git: ToolBinaryStatus {
            available: false,
            executable_path: None,
            sha256: None,
            version: None,
            error: Some(message.clone()),
        },
        gh: ToolBinaryStatus {
            available: false,
            executable_path: None,
            sha256: None,
            version: None,
            error: Some(message.clone()),
        },
        user_name: None,
        user_email: None,
        current_branch: None,
        detached_head: false,
        remote_name: remote_name.to_owned(),
        remote_url: None,
        credential_helper: None,
        gh_authenticated: false,
        gh_account: None,
        gh_status_summary: message,
        setup_hints: Vec::new(),
    }
}

async fn read_git_config(
    binary: &ResolvedHostBinary,
    cwd: &Path,
    key: &str,
    redactor: &SecretRedactor,
) -> Option<String> {
    let out = run_host_command(
        binary,
        cwd,
        &os_args(&["config", "--get", key]),
        true,
        STATUS_TIMEOUT_SECS,
        redactor,
    )
    .await
    .ok()?;
    if out.success {
        first_non_empty_line(&out.stdout)
    } else {
        None
    }
}

fn first_non_empty_line(raw: &str) -> Option<String> {
    raw.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned)
}

/// Strips any `userinfo@` segment from HTTP(S)/SSH remote URLs (including bare
/// token credentials like `https://ghp_xxx@github.com/org/repo.git`) and
/// runs deterministic secret redaction.
pub fn scrub_remote_url(raw: &str, redactor: &SecretRedactor) -> String {
    let trimmed = raw.trim();
    let scrubbed = if let Some(scheme_pos) = trimmed.find("://") {
        let after_scheme = &trimmed[scheme_pos + 3..];
        let authority_end = after_scheme
            .find(['/', '?', '#'])
            .unwrap_or(after_scheme.len());
        let authority = &after_scheme[..authority_end];
        if let Some(at_pos) = authority.rfind('@') {
            format!(
                "{}://***@{}",
                &trimmed[..scheme_pos],
                &after_scheme[at_pos + 1..]
            )
        } else {
            trimmed.to_owned()
        }
    } else {
        trimmed.to_owned()
    };
    redactor.redact(&scrubbed).text
}

pub struct ParsedGhAuth {
    pub authenticated: bool,
    pub account: Option<String>,
    pub summary: String,
}

/// Parses `gh auth status` output without retaining token lines or raw secrets.
pub fn parse_gh_auth_status(
    output: &str,
    command_succeeded: bool,
    redactor: &SecretRedactor,
) -> ParsedGhAuth {
    let mut account: Option<String> = None;
    let mut logged_in = false;
    for raw_line in output.lines() {
        let line = raw_line.trim();
        let lower = line.to_ascii_lowercase();
        // Drop any line mentioning a token before further inspection.
        if lower.contains("token:") || lower.contains("oauth") {
            continue;
        }
        if lower.contains("logged in to") {
            logged_in = true;
            if let Some(idx) = lower.find("account ") {
                let tail = line[idx + "account ".len()..].trim();
                let user = tail
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .trim_matches(|ch| ch == '(' || ch == ')' || ch == ',');
                if !user.is_empty() {
                    account = Some(redactor.redact(user).text);
                }
            }
        }
    }
    let authenticated = command_succeeded || logged_in;
    let summary = if authenticated {
        match &account {
            Some(user) => format!("Signed in to GitHub CLI as {user}."),
            None => "Signed in to GitHub CLI.".to_owned(),
        }
    } else {
        "Not signed in to GitHub CLI.".to_owned()
    };
    ParsedGhAuth {
        authenticated,
        account: account.map(|a| redactor.redact(&a).text),
        summary: redactor.redact(&summary).text,
    }
}
