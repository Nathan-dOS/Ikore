//! Safe subprocess launcher and executable discovery for host-managed git
//! and GitHub CLI operations (Feature 08 M1).
//!
//! Uses direct argv execution (never a shell), refuses wrappers and shortcuts,
//! hashes the resolved executable bytes, hides windows on Windows, bounds
//! child process trees with `ProcessTreeJob`, preserves user environment for
//! credential helpers while disabling interactive prompts, and redacts any
//! sensitive output before it leaves this module.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::secret_redactor::SecretRedactor;
use crate::terminals::{ProcessTreeJob, ProcessTreeSpawnError, hash_launch_files};

use super::GitToolError;

pub const STATUS_TIMEOUT_SECS: u64 = 15;
pub const COMMIT_TIMEOUT_SECS: u64 = 60;
pub const REMOTE_TIMEOUT_SECS: u64 = 180;
const MAX_OUTPUT_BYTES: usize = 512 * 1024;
const DRAIN_GRACE: Duration = Duration::from_secs(3);
const DRAIN_AFTER_TERMINATE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostBinary {
    Git,
    Gh,
}

impl HostBinary {
    pub const fn executable_stem(self) -> &'static str {
        match self {
            Self::Git => "git",
            Self::Gh => "gh",
        }
    }

    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Git => "git",
            Self::Gh => "gh",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedHostBinary {
    pub path: PathBuf,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub status_code: Option<i32>,
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Validates and hashes an explicit executable path or resolves the binary from `PATH`.
pub fn resolve_host_binary(binary: HostBinary) -> Result<ResolvedHostBinary, GitToolError> {
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    resolve_host_binary_in_path(binary, &path_var)
}

pub fn resolve_host_binary_in_path(
    binary: HostBinary,
    path_env: &OsStr,
) -> Result<ResolvedHostBinary, GitToolError> {
    let stem = binary.executable_stem();
    for dir in std::env::split_paths(path_env) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        for candidate in candidate_paths(&dir, stem) {
            if !candidate.is_file() {
                continue;
            }
            return validate_executable_path(&candidate);
        }
    }
    Err(GitToolError::InvalidExecutable(format!(
        "Could not find {} on PATH.",
        binary.display_name()
    )))
}

pub fn validate_executable_path(path: &Path) -> Result<ResolvedHostBinary, GitToolError> {
    reject_wrapper_extension(path)?;
    if !path.is_file() {
        return Err(GitToolError::InvalidExecutable(format!(
            "Executable path does not exist: {}",
            path.display()
        )));
    }
    let canonical = std::fs::canonicalize(path).map_err(|error| {
        GitToolError::InvalidExecutable(format!(
            "Failed to resolve executable path {}: {error}",
            path.display()
        ))
    })?;
    reject_wrapper_extension(&canonical)?;
    let sha256 = hash_launch_files(&[&canonical]).map_err(|error| {
        GitToolError::InvalidExecutable(format!(
            "Failed to fingerprint executable {}: {error}",
            canonical.display()
        ))
    })?;
    Ok(ResolvedHostBinary {
        path: canonical,
        sha256,
    })
}

fn candidate_paths(dir: &Path, stem: &str) -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        // Inspect native `.exe` alongside disallowed wrapper extensions so a shadowed
        // `.cmd`/`.bat`/`.ps1` wrapper is explicitly rejected instead of skipped silently.
        vec![
            dir.join(format!("{stem}.exe")),
            dir.join(format!("{stem}.cmd")),
            dir.join(format!("{stem}.bat")),
            dir.join(format!("{stem}.ps1")),
            dir.join(format!("{stem}.lnk")),
            dir.join(format!("{stem}.vbs")),
            dir.join(format!("{stem}.js")),
            dir.join(format!("{stem}.com")),
            dir.join(stem),
        ]
    }
    #[cfg(not(windows))]
    {
        vec![
            dir.join(stem),
            dir.join(format!("{stem}.sh")),
            dir.join(format!("{stem}.cmd")),
        ]
    }
}

fn reject_wrapper_extension(path: &Path) -> Result<(), GitToolError> {
    let ext = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if matches!(
        ext.as_str(),
        "cmd" | "bat" | "ps1" | "lnk" | "vbs" | "vbe" | "js" | "jse" | "wsf" | "wsh" | "sh"
    ) {
        return Err(GitToolError::InvalidExecutable(format!(
            "Wrapper scripts and shortcuts cannot be used for host git execution: {}",
            path.display()
        )));
    }
    Ok(())
}

/// Runs a resolved executable with direct argv, hidden window, `ProcessTreeJob`,
/// forced non-interactive git prompts, and deterministic output redaction.
pub async fn run_host_command(
    binary: &ResolvedHostBinary,
    cwd: &Path,
    args: &[&OsStr],
    read_only: bool,
    timeout_secs: u64,
    redactor: &SecretRedactor,
) -> Result<CommandOutput, GitToolError> {
    let mut command = Command::new(&binary.path);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        // Preserve the user environment so credential helpers work (Feature 08 §Processos),
        // while locking down prompts and interactive credential UI.
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never");
    if read_only {
        command.env("GIT_OPTIONAL_LOCKS", "0");
    }
    #[cfg(windows)]
    {
        // CREATE_NO_WINDOW keeps background git/gh invocations invisible on Windows.
        command.creation_flags(0x0800_0000);
    }

    let (mut child, job) = match ProcessTreeJob::spawn(&mut command) {
        Ok(controlled) => controlled,
        Err(ProcessTreeSpawnError::Spawn(error) | ProcessTreeSpawnError::Control(error)) => {
            return Err(GitToolError::Process(error.to_string()));
        }
    };

    let budget = Arc::new(AtomicUsize::new(MAX_OUTPUT_BYTES));
    let stdout_pipe = child
        .stdout
        .take()
        .ok_or_else(|| GitToolError::Process("Stdout pipe unavailable.".into()))?;
    let stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| GitToolError::Process("Stderr pipe unavailable.".into()))?;
    let stdout_task = tokio::spawn(drain_limited(stdout_pipe, budget.clone()));
    let stderr_task = tokio::spawn(drain_limited(stderr_pipe, budget));
    let aborters = [stdout_task.abort_handle(), stderr_task.abort_handle()];

    let timeout = Duration::from_secs(timeout_secs.max(1));
    let wait_outcome = tokio::time::timeout(timeout, child.wait()).await;
    let exit = match wait_outcome {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => {
            job.terminate();
            for handle in aborters {
                handle.abort();
            }
            return Err(GitToolError::Process(error.to_string()));
        }
        Err(_) => {
            job.terminate();
            let _ = child.kill().await;
            for handle in aborters {
                handle.abort();
            }
            return Err(GitToolError::TimedOut(timeout_secs));
        }
    };

    let streams = async { (stdout_task.await, stderr_task.await) };
    tokio::pin!(streams);
    let joined = match tokio::time::timeout(DRAIN_GRACE, &mut streams).await {
        Ok(pair) => Some(pair),
        Err(_) => {
            job.terminate();
            tokio::time::timeout(DRAIN_AFTER_TERMINATE, &mut streams)
                .await
                .ok()
        }
    };
    drop(job);

    let (raw_stdout, raw_stderr) = match joined {
        Some((Ok(out), Ok(err))) => (out, err),
        _ => {
            for handle in aborters {
                handle.abort();
            }
            (Vec::new(), Vec::new())
        }
    };

    let stdout_lossy = String::from_utf8_lossy(&raw_stdout);
    let stderr_lossy = String::from_utf8_lossy(&raw_stderr);
    let stdout = redactor.redact(stdout_lossy.as_ref()).text;
    let stderr = redactor.redact(stderr_lossy.as_ref()).text;

    Ok(CommandOutput {
        status_code: exit.code(),
        success: exit.success(),
        stdout,
        stderr,
    })
}

pub fn os_args<'a>(items: &'a [&'a str]) -> Vec<&'a OsStr> {
    items.iter().map(|s| OsStr::new(*s)).collect()
}

pub fn os_args_owned(items: &[OsString]) -> Vec<&OsStr> {
    items.iter().map(OsString::as_os_str).collect()
}

async fn drain_limited<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
    budget: Arc<AtomicUsize>,
) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let remaining = budget.load(Ordering::Relaxed);
                if remaining == 0 {
                    break;
                }
                let take = n.min(remaining);
                budget.fetch_sub(take, Ordering::Relaxed);
                out.extend_from_slice(&buf[..take]);
            }
        }
    }
    out
}
