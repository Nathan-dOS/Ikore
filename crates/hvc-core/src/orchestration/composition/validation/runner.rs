//! The command runner (§1-§2 of the validation contract): launches one
//! configured profile against a materialized candidate and writes the
//! receipt. Direct argv launch, reduced environment, hidden window,
//! process-tree job, bounded drained logs, timeout and cancellation. The
//! receipt records what actually happened; `passed` is never inferred.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use super::super::{
    CheckKind, DiagnosticV1, LogExcerptV1, ReceiptStatus, Severity, Sha256Hex, Timestamp, Uuid,
    ValidationReceiptV1,
};
use super::candidate::Candidate;
use super::diagnostics::{self, PathResolver};
use super::profile::ValidationProfileSnapshot;
use crate::orchestration::SchemaV1;
use crate::terminals::{ProcessTreeJob, configure_hidden};

/// Host correlation a receipt carries.
#[derive(Debug, Clone)]
pub struct ReceiptContext {
    pub stage_execution_id: Uuid,
    pub parent_generation: u32,
    pub profile_snapshot_id: Uuid,
    /// Host-selected extra environment (build caches such as
    /// `CARGO_TARGET_DIR`); values are paths chosen by the host, never
    /// secrets.
    pub extra_environment: Vec<(String, String)>,
}

/// Host environment names copied into every command: what a Windows
/// process and the Rust toolchain need to operate, and nothing else.
const HOST_ENVIRONMENT: &[&str] = &[
    "SystemRoot",
    "SYSTEMDRIVE",
    "ComSpec",
    "PATHEXT",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "APPDATA",
    "LOCALAPPDATA",
    "ProgramData",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "RUSTUP_TOOLCHAIN",
    "HOME",
];

fn restricted_path(executable: &Path) -> std::ffi::OsString {
    let mut directories: Vec<PathBuf> = Vec::new();
    if let Some(parent) = executable.parent() {
        directories.push(parent.to_path_buf());
    }
    if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
        directories.push(PathBuf::from(home).join(".cargo").join("bin"));
    }
    if let Some(root) = std::env::var_os("SystemRoot") {
        directories.push(PathBuf::from(&root).join("System32"));
        directories.push(PathBuf::from(root));
    }
    if !cfg!(windows) {
        directories.push(PathBuf::from("/usr/local/bin"));
        directories.push(PathBuf::from("/usr/bin"));
        directories.push(PathBuf::from("/bin"));
    }
    std::env::join_paths(directories).unwrap_or_default()
}

fn environment_digest(profile_names: &[String], extra: &[(String, String)]) -> Sha256Hex {
    let mut lines: Vec<String> = HOST_ENVIRONMENT.iter().map(|n| (*n).to_owned()).collect();
    for name in profile_names {
        // Project variables are bound by a digest of their redacted value.
        let value = std::env::var(name).unwrap_or_default();
        let redacted = crate::secret_redactor::redact_secrets(&value)
            .map(|r| r.text)
            .unwrap_or_default();
        lines.push(format!("{name}={}", Sha256Hex::of(redacted.as_bytes())));
    }
    for (name, value) in extra {
        lines.push(format!("{name}={}", Sha256Hex::of(value.as_bytes())));
    }
    lines.sort();
    Sha256Hex::of(lines.join("\n").as_bytes())
}

/// Drains a stream to completion, keeping at most what the shared budget
/// allows and discarding the rest so the child never blocks on a full pipe.
async fn drain<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
    budget: Arc<AtomicUsize>,
) -> (Vec<u8>, bool) {
    let mut kept = Vec::new();
    let mut truncated = false;
    let mut buffer = [0u8; 8192];
    loop {
        let read = match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        let mut remaining = budget.load(Ordering::SeqCst);
        loop {
            let take = read.min(remaining);
            match budget.compare_exchange(
                remaining,
                remaining - take,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => {
                    if take > 0 {
                        kept.extend_from_slice(&buffer[..take]);
                    }
                    if take < read {
                        truncated = true;
                    }
                    break;
                }
                Err(current) => remaining = current,
            }
        }
    }
    (kept, truncated)
}

fn excerpt(bytes: &[u8], truncated: bool) -> LogExcerptV1 {
    let text = String::from_utf8_lossy(bytes);
    let text = crate::secret_redactor::redact_secrets(&text)
        .map(|r| r.text)
        .unwrap_or_else(|_| text.into_owned());
    LogExcerptV1 { text, truncated }
}

fn empty_receipt(
    snapshot: &ValidationProfileSnapshot,
    candidate: &Candidate,
    context: &ReceiptContext,
    status: ReceiptStatus,
    diagnostics: Vec<DiagnosticV1>,
) -> ValidationReceiptV1 {
    ValidationReceiptV1 {
        schema_version: SchemaV1,
        receipt_id: Uuid::generate(),
        stage_execution_id: context.stage_execution_id.clone(),
        parent_generation: context.parent_generation,
        check_ref: snapshot.profile.check_ref.clone(),
        profile_snapshot_id: context.profile_snapshot_id.clone(),
        profile_sha256: snapshot.profile_sha256.clone(),
        candidate_sha256: candidate.candidate_sha256.clone(),
        input_manifest_sha256: candidate
            .input_manifest_sha256(&snapshot.profile.input_globs)
            .unwrap_or_else(|_| Sha256Hex::of(b"")),
        toolchain_sha256: snapshot.toolchain_sha256.clone(),
        environment_sha256: environment_digest(
            &snapshot.profile.environment_names,
            &context.extra_environment,
        ),
        status,
        exit_code: None,
        started_at: None,
        finished_at: None,
        stdout: LogExcerptV1 {
            text: String::new(),
            truncated: false,
        },
        stderr: LogExcerptV1 {
            text: String::new(),
            truncated: false,
        },
        diagnostics,
        baseline_receipt_id: None,
    }
}

fn error_diagnostic(message: &str) -> DiagnosticV1 {
    DiagnosticV1 {
        severity: Severity::Error,
        code: super::super::Handle::new("runner_error").unwrap(),
        message: message.chars().take(2_000).collect(),
        source_ref: None,
    }
}

/// Runs one profile against the candidate and returns its receipt. Never
/// returns an error: whatever prevented execution becomes an `unavailable`
/// or `error` receipt with a diagnostic.
pub async fn run_check(
    snapshot: &ValidationProfileSnapshot,
    candidate: &Candidate,
    context: &ReceiptContext,
    resolver: &(dyn PathResolver + Sync),
    cancellation: &CancellationToken,
) -> ValidationReceiptV1 {
    let profile = &snapshot.profile;
    let Some(executable) = snapshot
        .resolved_executable
        .as_deref()
        .map(PathBuf::from)
        .filter(|path| path.is_file())
    else {
        return empty_receipt(
            snapshot,
            candidate,
            context,
            ReceiptStatus::Unavailable,
            vec![error_diagnostic(&format!(
                "Executable `{}` is not installed or not resolvable; the host installs nothing.",
                profile.executable
            ))],
        );
    };
    if cancellation.is_cancelled() {
        return empty_receipt(snapshot, candidate, context, ReceiptStatus::NotRun, vec![]);
    }
    let cwd = if profile.cwd_relative_path == "." {
        candidate.dir.clone()
    } else {
        candidate.dir.join(
            profile
                .cwd_relative_path
                .replace('/', std::path::MAIN_SEPARATOR_STR),
        )
    };
    if !cwd.starts_with(&candidate.dir) || !cwd.is_dir() {
        return empty_receipt(
            snapshot,
            candidate,
            context,
            ReceiptStatus::Error,
            vec![error_diagnostic(
                "The working directory is not inside the candidate.",
            )],
        );
    }
    let mut command = Command::new(&executable);
    command
        .args(&profile.argv)
        .current_dir(&cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .env_clear();
    for name in HOST_ENVIRONMENT {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command.env("PATH", restricted_path(&executable));
    for name in &profile.environment_names {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    for (name, value) in &context.extra_environment {
        command.env(name, value);
    }
    configure_hidden(&mut command);

    let started_at = Timestamp::now();
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return empty_receipt(
                snapshot,
                candidate,
                context,
                ReceiptStatus::Unavailable,
                vec![error_diagnostic(&format!(
                    "The command could not be started: {error}"
                ))],
            );
        }
    };
    // Lifecycle control before any work is allowed to matter (§1).
    let job = match ProcessTreeJob::assign(&child) {
        Ok(job) => job,
        Err(error) => {
            let _ = child.kill().await;
            let mut receipt = empty_receipt(
                snapshot,
                candidate,
                context,
                ReceiptStatus::Error,
                vec![error_diagnostic(&format!(
                    "Process-tree control could not be established: {error}"
                ))],
            );
            receipt.started_at = Some(started_at);
            receipt.finished_at = Some(Timestamp::now());
            return receipt;
        }
    };
    let budget = Arc::new(AtomicUsize::new(
        (profile.max_log_bytes as usize).min(super::super::MAX_LOG_BYTES),
    ));
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let stdout_task = tokio::spawn(drain(stdout, budget.clone()));
    let stderr_task = tokio::spawn(drain(stderr, budget));
    let timeout = Duration::from_secs(u64::from(profile.timeout_seconds.clamp(1, 900)));
    let (status, exit_code) = tokio::select! {
        result = tokio::time::timeout(timeout, child.wait()) => match result {
            Ok(Ok(exit)) => (None, exit.code()),
            Ok(Err(error)) => {
                job.terminate();
                (Some((ReceiptStatus::Error, error.to_string())), None)
            }
            Err(_) => {
                job.terminate();
                let _ = child.kill().await;
                (Some((ReceiptStatus::TimedOut, format!("The command exceeded {} seconds and its process tree was terminated.", profile.timeout_seconds))), None)
            }
        },
        _ = cancellation.cancelled() => {
            job.terminate();
            let _ = child.kill().await;
            (Some((ReceiptStatus::Cancelled, "The stage was cancelled while the command ran; its process tree was terminated.".to_owned())), None)
        }
    };
    let finished_at = Timestamp::now();
    let (stdout_bytes, stdout_truncated) = stdout_task.await.unwrap_or_default();
    let (stderr_bytes, stderr_truncated) = stderr_task.await.unwrap_or_default();
    drop(job);
    let stdout = excerpt(&stdout_bytes, stdout_truncated);
    let stderr = excerpt(&stderr_bytes, stderr_truncated);

    let mut diagnostics = parse_diagnostics(profile, &stdout.text, &stderr.text, resolver);
    let status = match status {
        Some((status, message)) => {
            diagnostics.insert(0, error_diagnostic(&message));
            status
        }
        None => {
            let errors = diagnostics
                .iter()
                .any(|diagnostic| diagnostic.severity == Severity::Error);
            if exit_code == Some(0) && !errors {
                ReceiptStatus::Passed
            } else {
                if !errors {
                    diagnostics.push(diagnostics::exit_diagnostic(exit_code, &stderr.text));
                }
                ReceiptStatus::Failed
            }
        }
    };
    diagnostics.truncate(diagnostics::MAX_DIAGNOSTICS);
    let mut receipt = empty_receipt(snapshot, candidate, context, status, diagnostics);
    receipt.exit_code = exit_code;
    receipt.started_at = Some(started_at);
    receipt.finished_at = Some(finished_at);
    receipt.stdout = stdout;
    receipt.stderr = stderr;
    receipt
}

fn parse_diagnostics(
    profile: &super::super::ValidationCommandProfileV1,
    stdout: &str,
    stderr: &str,
    resolver: &(dyn PathResolver + Sync),
) -> Vec<DiagnosticV1> {
    let cargo = profile.toolchain_identity == "cargo";
    // Any tool asked for cargo's JSON message stream is parsed as such.
    if profile
        .argv
        .iter()
        .any(|arg| arg == "--message-format=json")
    {
        return diagnostics::parse_cargo_json(stdout, resolver);
    }
    if cargo && profile.kind == CheckKind::Test {
        let mut combined = String::with_capacity(stdout.len() + stderr.len() + 1);
        combined.push_str(stdout);
        combined.push('\n');
        combined.push_str(stderr);
        return diagnostics::parse_cargo_test(&combined, resolver);
    }
    let mut combined = String::with_capacity(stdout.len() + stderr.len() + 1);
    combined.push_str(stderr);
    combined.push('\n');
    combined.push_str(stdout);
    diagnostics::parse_generic(&combined, resolver)
}
