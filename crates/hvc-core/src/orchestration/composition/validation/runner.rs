//! The command runner (§1-§2 of the validation contract): launches one
//! configured profile against a materialized candidate and writes the
//! receipt. Direct argv launch, reduced environment, hidden window,
//! process-tree job, bounded drained logs, timeout and cancellation. The
//! receipt records what actually happened; `passed` is never inferred.

use std::collections::BTreeMap;
use std::ffi::OsString;
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

/// How long the output streams may stay open after the command exited before the host treats
/// them as held by a descendant and ends the process tree. Reading what a finished command
/// already wrote takes milliseconds.
const DRAIN_GRACE: Duration = Duration::from_secs(5);
/// Once the tree is ended, how long the readers get to reach the end of the streams.
const DRAIN_AFTER_TERMINATE: Duration = Duration::from_secs(2);

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

/// Exact environment used for one run. Values are deliberately neither
/// serializable nor printable: only the safe fingerprint leaves this object.
pub struct EffectiveEnvironment {
    values: BTreeMap<OsString, OsString>,
    digest: Sha256Hex,
    comparable: bool,
}

fn digest_environment(values: &BTreeMap<OsString, OsString>) -> (Sha256Hex, bool) {
    let mut bytes = Vec::new();
    let mut comparable = true;
    for (name, value) in values {
        let (Some(name), Some(value)) = (name.to_str(), value.to_str()) else {
            comparable = false;
            continue;
        };
        bytes.extend_from_slice(name.as_bytes());
        bytes.push(0);
        match crate::secret_redactor::redact_secrets(value) {
            Ok(redacted) => {
                // Never persist a hash of a detected credential. Qualified
                // profiles refuse such an environment rather than reusing
                // different credentials under the same redacted digest.
                comparable &= redacted.count == 0;
                bytes
                    .extend_from_slice(Sha256Hex::of(redacted.text.as_bytes()).as_str().as_bytes());
            }
            Err(_) => comparable = false,
        }
        bytes.push(0);
    }
    // Cargo also reads nonsecret configuration from its home. Bind those
    // bytes, not merely the directory name, without reading credentials files.
    let cargo_home = values
        .get(&OsString::from("CARGO_HOME"))
        .map(PathBuf::from)
        .or_else(|| {
            values
                .get(&OsString::from("HOME"))
                .map(|home| PathBuf::from(home).join(".cargo"))
        })
        .or_else(|| {
            values
                .get(&OsString::from("USERPROFILE"))
                .map(|home| PathBuf::from(home).join(".cargo"))
        });
    if let Some(home) = cargo_home {
        for name in ["config", "config.toml"] {
            bytes.extend_from_slice(name.as_bytes());
            bytes.push(0);
            match std::fs::symlink_metadata(home.join(name)) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    bytes.extend_from_slice(b"absent")
                }
                Ok(metadata)
                    if metadata.is_file()
                        && !metadata.file_type().is_symlink()
                        && metadata.len() <= 1024 * 1024 =>
                {
                    match std::fs::read_to_string(home.join(name)) {
                        Ok(text) if text.len() as u64 == metadata.len() => {
                            match crate::secret_redactor::redact_secrets(&text) {
                                Ok(redacted) if redacted.count == 0 => bytes.extend_from_slice(
                                    Sha256Hex::of(text.as_bytes()).as_str().as_bytes(),
                                ),
                                _ => comparable = false,
                            }
                        }
                        _ => comparable = false,
                    }
                }
                _ => comparable = false,
            }
            bytes.push(0);
        }
    }
    (Sha256Hex::of(&bytes), comparable)
}

pub fn effective_environment(
    snapshot: &ValidationProfileSnapshot,
    context: &ReceiptContext,
) -> EffectiveEnvironment {
    let mut values = BTreeMap::new();
    for name in HOST_ENVIRONMENT {
        if let Some(value) = std::env::var_os(name) {
            values.insert(OsString::from(name), value);
        }
    }
    let executable = snapshot
        .resolved_executable
        .as_deref()
        .map(Path::new)
        .unwrap_or_else(|| Path::new(&snapshot.profile.executable));
    values.insert(OsString::from("PATH"), restricted_path(executable));
    for name in &snapshot.profile.environment_names {
        if let Some(value) = std::env::var_os(name) {
            values.insert(OsString::from(name), value);
        }
    }
    for (name, value) in &snapshot.effective_environment {
        values.insert(OsString::from(name), OsString::from(value));
    }
    if let Some(toolchain) = &snapshot.cargo_toolchain {
        values.insert(
            OsString::from("RUSTC"),
            OsString::from(&toolchain.rustc.path),
        );
        values.insert(
            OsString::from("RUSTDOC"),
            OsString::from(&toolchain.rustdoc.path),
        );
    }
    for (name, value) in &context.extra_environment {
        values.insert(OsString::from(name), OsString::from(value));
    }
    let (digest, comparable) = digest_environment(&values);
    EffectiveEnvironment {
        values,
        digest,
        comparable,
    }
}

pub fn effective_environment_digest(
    snapshot: &ValidationProfileSnapshot,
    context: &ReceiptContext,
) -> Sha256Hex {
    effective_environment(snapshot, context).digest
}

/// What a reader kept of one output stream.
#[derive(Default)]
struct Drained {
    kept: Vec<u8>,
    truncated: bool,
    /// The stream did not reach its end: a read failed, the reader task ended abnormally or
    /// it was stopped because the stream stayed open.
    incomplete: bool,
}

impl Drained {
    fn incomplete() -> Self {
        Self {
            incomplete: true,
            ..Self::default()
        }
    }
}

/// Drains a stream to completion, keeping at most what the shared budget
/// allows and discarding the rest so the child never blocks on a full pipe.
async fn drain<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
    budget: Arc<AtomicUsize>,
) -> Drained {
    let mut kept = Vec::new();
    let mut truncated = false;
    let mut incomplete = false;
    let mut buffer = [0u8; 8192];
    loop {
        let read = match reader.read(&mut buffer).await {
            Ok(0) => break,
            Err(_) => {
                incomplete = true;
                break;
            }
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
    Drained {
        kept,
        truncated,
        incomplete,
    }
}

fn excerpt(bytes: &[u8], truncated: bool) -> Result<LogExcerptV1, String> {
    if truncated {
        // A bounded prefix can end halfway through a credential. Withholding
        // the stream is safe even when its final delimiter was discarded.
        return Ok(LogExcerptV1 {
            text: "[output withheld: validation log limit exceeded]".into(),
            truncated: true,
        });
    }
    fn sanitize_json(value: &mut serde_json::Value) -> Result<(), String> {
        match value {
            serde_json::Value::String(text) => {
                *text = crate::secret_redactor::redact_secrets(text)?.text
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    sanitize_json(value)?;
                }
            }
            serde_json::Value::Object(fields) => {
                let mut safe = serde_json::Map::new();
                for (key, mut value) in std::mem::take(fields) {
                    sanitize_json(&mut value)?;
                    safe.insert(crate::secret_redactor::redact_secrets(&key)?.text, value);
                }
                *fields = safe;
            }
            _ => {}
        }
        Ok(())
    }
    let text = String::from_utf8_lossy(bytes);
    let mut decoded = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(line) {
            sanitize_json(&mut value)?;
            decoded.push_str(
                &serde_json::to_string(&value)
                    .map_err(|_| "validation_log_serialization_failed")?,
            );
            if line.ends_with('\n') {
                decoded.push('\n');
            }
        } else {
            decoded.push_str(line);
        }
    }
    let text = crate::secret_redactor::redact_secrets(&decoded).map(|r| r.text)?;
    Ok(LogExcerptV1 { text, truncated })
}

fn empty_receipt(
    snapshot: &ValidationProfileSnapshot,
    candidate: &Candidate,
    context: &ReceiptContext,
    status: ReceiptStatus,
    diagnostics: Vec<DiagnosticV1>,
    environment: &EffectiveEnvironment,
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
        environment_sha256: environment.digest.clone(),
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
    let message = crate::secret_redactor::redact_secrets(message)
        .map(|r| r.text)
        .unwrap_or_else(|_| "Validation detail withheld: sanitization unavailable.".into());
    DiagnosticV1 {
        severity: Severity::Error,
        code: super::super::Handle::new("runner_error")
            .expect("built-in runner error code is a valid handle"),
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
    let environment = effective_environment(snapshot, context);
    run_check_in_environment(
        snapshot,
        candidate,
        context,
        resolver,
        cancellation,
        &environment,
        true,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_check_in_environment(
    snapshot: &ValidationProfileSnapshot,
    candidate: &Candidate,
    context: &ReceiptContext,
    resolver: &(dyn PathResolver + Sync),
    cancellation: &CancellationToken,
    environment: &EffectiveEnvironment,
    interpret_diagnostics: bool,
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
            environment,
        );
    };
    if cancellation.is_cancelled() {
        return empty_receipt(
            snapshot,
            candidate,
            context,
            ReceiptStatus::NotRun,
            vec![],
            environment,
        );
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
            environment,
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
    command.envs(&environment.values);
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
                environment,
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
                environment,
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
    let readers = [stdout_task.abort_handle(), stderr_task.abort_handle()];
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
    // The streams end only when every process holding them has exited. A descendant that
    // inherited them must not keep the receipt past the run's own deadline.
    let streams = async { (stdout_task.await, stderr_task.await) };
    tokio::pin!(streams);
    let mut held_open = false;
    let joined = match tokio::time::timeout(DRAIN_GRACE, &mut streams).await {
        Ok(joined) => Some(joined),
        Err(_) => {
            held_open = true;
            job.terminate();
            tokio::time::timeout(DRAIN_AFTER_TERMINATE, &mut streams)
                .await
                .ok()
        }
    };
    drop(job);
    let (stdout_drained, stderr_drained) = match joined {
        Some((stdout, stderr)) => (
            stdout.unwrap_or_else(|_| Drained::incomplete()),
            stderr.unwrap_or_else(|_| Drained::incomplete()),
        ),
        None => {
            for reader in &readers {
                reader.abort();
            }
            (Drained::incomplete(), Drained::incomplete())
        }
    };
    let stdout_truncated = stdout_drained.truncated;
    let stderr_truncated = stderr_drained.truncated;
    let streams_incomplete = stdout_drained.incomplete || stderr_drained.incomplete;
    let stdout_result = excerpt(&stdout_drained.kept, stdout_truncated);
    let stderr_result = excerpt(&stderr_drained.kept, stderr_truncated);
    let unavailable = stdout_result.is_err() || stderr_result.is_err();
    let withheld = || LogExcerptV1 {
        text: "[output withheld: sanitization unavailable]".into(),
        truncated: false,
    };
    let stdout = stdout_result.unwrap_or_else(|_| withheld());
    let stderr = stderr_result.unwrap_or_else(|_| withheld());
    let status = if unavailable {
        Some((
            ReceiptStatus::Error,
            "Validation evidence is incomplete or sanitization is unavailable.".to_owned(),
        ))
    } else if status.is_none() && (held_open || streams_incomplete) {
        // The command finished, but its output may not be all there: never a pass.
        Some((
            ReceiptStatus::Error,
            if held_open {
                "Validation evidence is incomplete: a process the command started kept its output open after it exited, and the process tree was terminated."
            } else {
                "Validation evidence is incomplete: the command output could not be read to its end."
            }
            .to_owned(),
        ))
    } else {
        status
    };

    let mut diagnostics = if interpret_diagnostics {
        parse_diagnostics(profile, &stdout.text, &stderr.text, resolver)
    } else {
        Vec::new()
    };
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
    let mut receipt = empty_receipt(
        snapshot,
        candidate,
        context,
        status,
        diagnostics,
        environment,
    );
    receipt.exit_code = exit_code;
    receipt.started_at = Some(started_at);
    receipt.finished_at = Some(finished_at);
    receipt.stdout = stdout;
    receipt.stderr = stderr;
    receipt
}

pub(crate) fn tools_unchanged(snapshot: &ValidationProfileSnapshot) -> bool {
    let Some(toolchain) = &snapshot.cargo_toolchain else {
        return snapshot
            .resolved_executable
            .as_deref()
            .zip(snapshot.resolved_executable_sha256.as_ref())
            .is_some_and(|(path, expected)| {
                crate::terminals::hash_launch_files(&[Path::new(path)])
                    .is_ok_and(|hash| hash == expected.as_str())
            });
    };
    [&toolchain.cargo, &toolchain.rustc, &toolchain.rustdoc]
        .iter()
        .all(|tool| {
            crate::terminals::hash_launch_files(&[Path::new(&tool.path)])
                .is_ok_and(|hash| hash == tool.sha256.as_str())
        })
}

fn metadata_is_supported(text: &str, candidate: &Candidate) -> Result<(), String> {
    let metadata: serde_json::Value = serde_json::from_str(text)
        .map_err(|_| "Cargo metadata output was not understood.".to_owned())?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| "Cargo metadata did not contain its package graph.".to_owned())?;
    let members = metadata["workspace_members"]
        .as_array()
        .filter(|members| !members.is_empty())
        .ok_or_else(|| "Cargo metadata did not identify workspace members.".to_owned())?;
    let root = candidate
        .dir
        .canonicalize()
        .map_err(|_| "The candidate root could not be verified.".to_owned())?;
    let workspace = metadata["workspace_root"]
        .as_str()
        .and_then(|path| Path::new(path).canonicalize().ok());
    if workspace.as_ref() != Some(&root) {
        return Err("The Cargo workspace root is outside the candidate.".into());
    }
    for package in packages {
        // Registry/git caches are declared external inputs. A path dependency
        // has no source identifier and must be captured inside this candidate.
        if package["source"].is_null() {
            let manifest = package["manifest_path"]
                .as_str()
                .and_then(|path| Path::new(path).canonicalize().ok())
                .ok_or_else(|| "A path dependency manifest could not be verified.".to_owned())?;
            if !manifest.starts_with(&root) {
                return Err("A path dependency is outside the captured project root.".into());
            }
        }
    }
    if members
        .iter()
        .any(|member| !packages.iter().any(|package| package["id"] == *member))
    {
        return Err("Cargo metadata omitted a workspace member.".into());
    }
    Ok(())
}

fn preparation_evidence(receipt: &ValidationReceiptV1) -> super::evidence::PreparationEvidence {
    super::evidence::PreparationEvidence {
        status: receipt.status,
        exit_code: receipt.exit_code,
        started_at: receipt.started_at.clone(),
        finished_at: receipt.finished_at.clone(),
        stdout: receipt.stdout.clone(),
        stderr: receipt.stderr.clone(),
    }
}

/// Execute one developer-approved qualified Cargo profile. Preparation uses
/// the exact same frozen environment and total time budget as the main run;
/// no installer, shell or package download is introduced here.
#[allow(clippy::too_many_arguments)]
pub async fn run_qualified_check(
    snapshot: &ValidationProfileSnapshot,
    policy: &super::cargo_policy::CargoValidationPolicyV1,
    candidate: &Candidate,
    context: &ReceiptContext,
    resolver: &(dyn PathResolver + Sync),
    cancellation: &CancellationToken,
) -> (ValidationReceiptV1, super::evidence::RunEvidenceDetailsV1) {
    use super::evidence::{
        CheckResult, InterpretationStatus, RunEvidenceDetailsV1, ToolVersion, ValidationPhase,
    };

    let started = tokio::time::Instant::now();
    let limit = Duration::from_secs(u64::from(snapshot.profile.timeout_seconds.clamp(1, 900)));
    let environment = effective_environment(snapshot, context);
    let mut receipt = empty_receipt(
        snapshot,
        candidate,
        context,
        ReceiptStatus::Unavailable,
        Vec::new(),
        &environment,
    );
    let mut details = RunEvidenceDetailsV1 {
        schema_version: SchemaV1,
        receipt_id: receipt.receipt_id.clone(),
        policy_sha256: super::super::canonical_sha256(policy, &[])
            .unwrap_or_else(|_| Sha256Hex::of(b"invalid-policy")),
        parser_version: policy.parser_version,
        phase: ValidationPhase::Preparation,
        interpretation: InterpretationStatus::NotApplicable,
        result: CheckResult::Unknown,
        candidate_integrity: false,
        preparation: None,
        tool_versions: Vec::new(),
        tests_passed: None,
        tests_failed: None,
        tests_ignored: None,
        limitations: vec![
            "Offline mode restricts Cargo dependency resolution; project scripts and tests are not network-isolated.".into(),
            "Registry/git caches are external inputs; this is not a hermetic build.".into(),
        ],
    };
    if !cfg!(windows) {
        details.limitations.push(
            "Process-tree containment is qualified on Windows x64 only; this platform is informational.".into(),
        );
    }
    let preparation_error = if let Err(error) = policy.validate() {
        Some(error.to_string())
    } else if !environment.comparable {
        Some("The execution environment contains secret-bearing or non-text values and cannot be qualified.".into())
    } else if snapshot.cargo_policy.as_ref() != Some(policy) {
        Some("The Cargo policy differs from the admitted execution snapshot.".into())
    } else if !tools_unchanged(snapshot) {
        Some(snapshot.cargo_unavailable_reason.clone().unwrap_or_else(|| {
            "The selected Cargo toolchain is missing or changed after admission; nothing was installed.".into()
        }))
    } else if let Err(error) = super::candidate::verify_candidate(candidate) {
        Some(format!(
            "The candidate inputs could not be verified: {error}"
        ))
    } else {
        None
    };
    if let Some(message) = preparation_error {
        receipt.diagnostics.push(error_diagnostic(&message));
        return (receipt, details);
    }
    details.candidate_integrity = true;
    if cancellation.is_cancelled() {
        receipt.status = ReceiptStatus::NotRun;
        return (receipt, details);
    }

    let toolchain = snapshot
        .cargo_toolchain
        .as_ref()
        .expect("verified toolchain");
    for (name, tool) in [
        ("cargo", &toolchain.cargo),
        ("rustc", &toolchain.rustc),
        ("rustdoc", &toolchain.rustdoc),
    ] {
        let remaining = limit.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            receipt.status = ReceiptStatus::Error;
            receipt.diagnostics.push(error_diagnostic(
                "Preparation exhausted the profile time budget.",
            ));
            return (receipt, details);
        }
        let mut probe = snapshot.clone();
        probe.resolved_executable = Some(tool.path.clone());
        probe.profile.argv = vec!["--version".into()];
        probe.profile.timeout_seconds = remaining.as_secs().clamp(1, 900) as u32;
        let result = run_check_in_environment(
            &probe,
            candidate,
            context,
            resolver,
            cancellation,
            &environment,
            false,
        )
        .await;
        let version = result
            .stdout
            .text
            .lines()
            .next()
            .map(str::trim)
            .filter(|version| version.starts_with(&format!("{name} ")))
            .map(|version| version.chars().take(512).collect::<String>());
        details.tool_versions.push(ToolVersion {
            tool: name.into(),
            version: version.clone(),
        });
        if result.status != ReceiptStatus::Passed || version.is_none() {
            details.preparation = Some(preparation_evidence(&result));
            receipt.diagnostics.push(error_diagnostic(
                "A toolchain version probe failed or returned an unsupported response.",
            ));
            if cancellation.is_cancelled() {
                receipt.status = ReceiptStatus::NotRun;
            }
            return (receipt, details);
        }
    }

    let remaining = limit.saturating_sub(started.elapsed());
    if remaining.is_zero() {
        receipt.status = ReceiptStatus::Error;
        receipt.diagnostics.push(error_diagnostic(
            "Preparation exhausted the profile time budget.",
        ));
        return (receipt, details);
    }
    let mut preflight = snapshot.clone();
    preflight.profile.argv = vec![
        "metadata".into(),
        "--locked".into(),
        "--offline".into(),
        "--format-version=1".into(),
    ];
    preflight.profile.timeout_seconds = remaining.as_secs().clamp(1, 900) as u32;
    let prepared = run_check_in_environment(
        &preflight,
        candidate,
        context,
        resolver,
        cancellation,
        &environment,
        false,
    )
    .await;
    details.preparation = Some(preparation_evidence(&prepared));
    if prepared.status != ReceiptStatus::Passed {
        receipt.diagnostics.push(error_diagnostic("Cargo preparation did not complete: check the toolchain, lockfile, cached dependencies and declared inputs."));
        if cancellation.is_cancelled() {
            receipt.status = ReceiptStatus::NotRun;
        }
        return (receipt, details);
    }
    if let Err(message) = metadata_is_supported(&prepared.stdout.text, candidate) {
        receipt.diagnostics.push(error_diagnostic(&message));
        return (receipt, details);
    }
    if let Err(error) = super::candidate::verify_candidate(candidate) {
        details.candidate_integrity = false;
        receipt.status = ReceiptStatus::Error;
        receipt.diagnostics.push(error_diagnostic(&format!(
            "Preparation changed the captured inputs: {error}"
        )));
        return (receipt, details);
    }
    let remaining = limit.saturating_sub(started.elapsed());
    if remaining.is_zero() {
        receipt.status = ReceiptStatus::Error;
        receipt.diagnostics.push(error_diagnostic(
            "Preparation exhausted the profile time budget.",
        ));
        return (receipt, details);
    }
    let mut executable_snapshot = snapshot.clone();
    executable_snapshot.profile.timeout_seconds = remaining.as_secs().clamp(1, 900) as u32;
    receipt = run_check_in_environment(
        &executable_snapshot,
        candidate,
        context,
        resolver,
        cancellation,
        &environment,
        false,
    )
    .await;
    details.receipt_id = receipt.receipt_id.clone();
    details.phase = ValidationPhase::Execution;
    let interpreted = diagnostics::interpret_cargo_output(
        snapshot.profile.kind,
        &receipt.stdout.text,
        &receipt.stderr.text,
        resolver,
    );
    details.tests_passed = interpreted.tests_passed;
    details.tests_failed = interpreted.tests_failed;
    details.tests_ignored = interpreted.tests_ignored;
    details.limitations.extend(interpreted.limitations);
    let complete = !receipt.stdout.truncated && !receipt.stderr.truncated;
    details.interpretation = if interpreted.interpreted && complete {
        InterpretationStatus::Supported
    } else if interpreted.build_succeeded.is_some() {
        InterpretationStatus::Partial
    } else {
        InterpretationStatus::Uninterpreted
    };
    for diagnostic in interpreted.diagnostics {
        if receipt.diagnostics.len() < diagnostics::MAX_DIAGNOSTICS {
            receipt.diagnostics.push(diagnostic);
        }
    }
    let result_errors = receipt
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error)
        || interpreted.build_succeeded == Some(false)
        || interpreted.tests_failed.is_some_and(|count| count > 0);
    match receipt.status {
        ReceiptStatus::Passed | ReceiptStatus::Failed => {
            if details.interpretation != InterpretationStatus::Supported {
                receipt.status = ReceiptStatus::Error;
                receipt.diagnostics.insert(
                    0,
                    error_diagnostic(
                        "The required Cargo result could not be completely interpreted.",
                    ),
                );
            } else if receipt.exit_code == Some(0) && !result_errors {
                details.result = CheckResult::Passed;
                receipt.status = ReceiptStatus::Passed;
            } else {
                details.result = CheckResult::Failed;
                receipt.status = ReceiptStatus::Failed;
            }
        }
        _ => {}
    }
    if let Err(error) = super::candidate::verify_candidate(candidate) {
        details.candidate_integrity = false;
        details.result = CheckResult::Unknown;
        receipt.status = ReceiptStatus::Error;
        receipt.diagnostics.insert(
            0,
            error_diagnostic(&format!(
                "The command changed captured candidate inputs: {error}"
            )),
        );
    }
    if !tools_unchanged(snapshot) {
        details.result = CheckResult::Unknown;
        receipt.status = ReceiptStatus::Error;
        receipt.diagnostics.insert(
            0,
            error_diagnostic("The toolchain changed during execution."),
        );
    }
    receipt.diagnostics.truncate(diagnostics::MAX_DIAGNOSTICS);
    (receipt, details)
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

#[cfg(test)]
mod sanitization_tests {
    use super::*;

    #[test]
    fn complete_logs_redact_secrets_and_truncated_prefixes_are_withheld() {
        let secret = b"\xc3\xa9 Authorization: Bearer synthetic-credential-value";
        let complete = excerpt(secret, false).unwrap();
        assert!(!complete.text.contains("synthetic-credential-value"));
        assert!(complete.text.contains("<REDACTED_SECRET:"));
        let truncated =
            excerpt(b"-----BEGIN PRIVATE KEY-----\npartial-private-value", true).unwrap();
        assert!(truncated.truncated);
        assert!(!truncated.text.contains("partial-private-value"));
        assert!(!truncated.text.contains("BEGIN PRIVATE KEY"));
        let escaped = br#"{"message":"sk-proj-\u0061bcdefghijklmnopqrst"}"#;
        let decoded = excerpt(escaped, false).unwrap();
        assert!(!decoded.text.contains("bcdefghijklmnopqrst"));
        assert!(decoded.text.contains("<REDACTED_SECRET:"));
    }

    #[test]
    fn effective_environment_binds_values_and_presence_without_persisting_them() {
        let mut values = BTreeMap::new();
        values.insert(OsString::from("CARGO_HOME"), OsString::from("/cache/first"));
        let (first, comparable) = digest_environment(&values);
        assert!(comparable);
        values.insert(OsString::from("CARGO_HOME"), OsString::from("/cache/other"));
        assert_ne!(first, digest_environment(&values).0);
        values.insert(OsString::from("RUSTUP_TOOLCHAIN"), OsString::from("stable"));
        let present = digest_environment(&values).0;
        values.remove(&OsString::from("RUSTUP_TOOLCHAIN"));
        assert_ne!(present, digest_environment(&values).0);
    }

    #[test]
    fn detected_credentials_cannot_qualify_an_environment() {
        let mut values = BTreeMap::new();
        values.insert(
            OsString::from("PROJECT_OPTION"),
            OsString::from("Authorization: Bearer synthetic-credential-value"),
        );
        assert!(!digest_environment(&values).1);
    }

    #[test]
    fn cargo_home_configuration_changes_invalidate_the_environment_fingerprint() {
        let home = tempfile::tempdir().unwrap();
        let values = BTreeMap::from([(
            OsString::from("CARGO_HOME"),
            home.path().as_os_str().to_owned(),
        )]);
        let absent = digest_environment(&values);
        std::fs::write(home.path().join("config.toml"), "[build]\njobs=2\n").unwrap();
        let first = digest_environment(&values);
        assert!(first.1);
        assert_ne!(absent.0, first.0);
        std::fs::write(home.path().join("config.toml"), "[build]\njobs=4\n").unwrap();
        assert_ne!(first.0, digest_environment(&values).0);
    }

    #[test]
    fn metadata_rejects_external_path_dependencies() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("Cargo.toml"), "[package]\n").unwrap();
        let candidate = Candidate {
            dir: root.path().to_path_buf(),
            manifest: BTreeMap::new(),
            candidate_sha256: Sha256Hex::of(b"candidate"),
            staged_paths: Vec::new(),
        };
        let metadata = serde_json::json!({
            "workspace_root": root.path().to_string_lossy(),
            "workspace_members": ["workspace"],
            "packages": [{"id":"workspace", "source":null, "manifest_path":outside.path().join("Cargo.toml").to_string_lossy()}]
        });
        assert!(metadata_is_supported(&metadata.to_string(), &candidate).is_err());
        let mut internal = metadata;
        internal["packages"][0]["manifest_path"] =
            serde_json::json!(root.path().join("Cargo.toml").to_string_lossy());
        assert!(metadata_is_supported(&internal.to_string(), &candidate).is_ok());
        assert!(metadata_is_supported("not metadata", &candidate).is_err());
    }

    fn cargo_workspace(root: &Path) {
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("leaf/src")).unwrap();
        std::fs::create_dir_all(root.join("assets")).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            concat!(
                "[workspace]\nmembers=[\"leaf\"]\nresolver=\"3\"\n",
                "[package]\nname=\"qualified-root\"\nversion=\"0.1.0\"\nedition=\"2024\"\n",
                "[dependencies]\nqualified-leaf={path=\"leaf\"}\n",
            ),
        )
        .unwrap();
        std::fs::write(
            root.join("leaf/Cargo.toml"),
            "[package]\nname=\"qualified-leaf\"\nversion=\"0.1.0\"\nedition=\"2024\"\n",
        )
        .unwrap();
        std::fs::write(root.join("leaf/src/lib.rs"), "pub fn value()->u8 { 7 }\n").unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            concat!(
                "pub const ASSET: &[u8] = include_bytes!(\"../assets/seed.bin\");\n",
                "#[test] fn passes(){assert_eq!(qualified_leaf::value(),ASSET[0]);}\n",
            ),
        )
        .unwrap();
        std::fs::write(root.join("Cargo.lock"), concat!(
            "version = 4\n\n[[package]]\nname = \"qualified-leaf\"\nversion = \"0.1.0\"\n\n",
            "[[package]]\nname = \"qualified-root\"\nversion = \"0.1.0\"\ndependencies = [\"qualified-leaf\"]\n",
        )).unwrap();
        std::fs::write(root.join("assets/seed.bin"), [7, 0, 255]).unwrap();
    }

    #[tokio::test]
    async fn qualified_cargo_executes_a_real_offline_workspace_and_rejects_missing_preparation() {
        use super::super::candidate::{capture_baseline, materialize_from_baseline};
        use super::super::cargo_policy::CargoValidationPolicyV1;
        use super::super::profile::{ProfileOrigin, detect_presets, resolve_cargo_profile};

        let root = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        cargo_workspace(root.path());
        let original_lock = std::fs::read(root.path().join("Cargo.lock")).unwrap();
        let policy = CargoValidationPolicyV1 {
            additional_inputs: vec!["assets/seed.bin".into()],
            ..CargoValidationPolicyV1::default()
        };
        let baseline = capture_baseline(
            root.path(),
            &data.path().join("baseline"),
            &policy.additional_inputs,
        )
        .unwrap();
        let candidate =
            materialize_from_baseline(&baseline, &[], &data.path().join("build")).unwrap();
        let cancellation = CancellationToken::new();
        let presets = detect_presets(root.path());
        let mut context = ReceiptContext {
            stage_execution_id: Uuid::generate(),
            parent_generation: 1,
            profile_snapshot_id: Uuid::generate(),
            extra_environment: vec![(
                "CARGO_TARGET_DIR".into(),
                data.path().join("target").to_string_lossy().into_owned(),
            )],
        };
        for profile in presets {
            let snapshot = resolve_cargo_profile(
                &profile,
                ProfileOrigin::Preset,
                &policy,
                root.path(),
                &cancellation,
            )
            .await
            .unwrap();
            assert!(
                snapshot.cargo_toolchain.is_some(),
                "{:?}",
                snapshot.cargo_unavailable_reason
            );
            for _ in 0..3 {
                let (receipt, details) = run_qualified_check(
                    &snapshot,
                    &policy,
                    &candidate,
                    &context,
                    &|_: &str| None,
                    &cancellation,
                )
                .await;
                receipt.check_execution_semantics().unwrap();
                assert_eq!(
                    receipt.status,
                    ReceiptStatus::Passed,
                    "{:?}; {:?}",
                    receipt.diagnostics,
                    details.limitations
                );
                assert!(details.qualifies());
                assert_eq!(
                    receipt.environment_sha256,
                    effective_environment_digest(&snapshot, &context)
                );
                if profile.kind == CheckKind::Test {
                    assert_eq!(details.tests_passed, Some(1));
                    assert_eq!(details.tests_failed, Some(0));
                }
                context.profile_snapshot_id = Uuid::generate();
            }
        }
        assert_eq!(
            std::fs::read(root.path().join("Cargo.lock")).unwrap(),
            original_lock
        );
        assert!(!root.path().join("target").exists());

        // A missing lock is a preparation limitation, not a started check or
        // an authoring defect that a model should try to repair.
        std::fs::remove_file(root.path().join("Cargo.lock")).unwrap();
        let incomplete = capture_baseline(
            root.path(),
            &data.path().join("incomplete-baseline"),
            &policy.additional_inputs,
        )
        .unwrap();
        let incomplete =
            materialize_from_baseline(&incomplete, &[], &data.path().join("incomplete-build"))
                .unwrap();
        let profile = detect_presets(root.path()).remove(0);
        let snapshot = resolve_cargo_profile(
            &profile,
            ProfileOrigin::Preset,
            &policy,
            root.path(),
            &cancellation,
        )
        .await
        .unwrap();
        let (receipt, details) = run_qualified_check(
            &snapshot,
            &policy,
            &incomplete,
            &context,
            &|_: &str| None,
            &cancellation,
        )
        .await;
        assert_eq!(receipt.status, ReceiptStatus::Unavailable);
        assert!(receipt.started_at.is_none());
        assert!(receipt.exit_code.is_none());
        assert!(!details.qualifies());
        assert_eq!(
            details.phase,
            super::super::evidence::ValidationPhase::Preparation
        );
        receipt.check_execution_semantics().unwrap();
    }

    #[tokio::test]
    async fn qualified_cargo_distinguishes_code_test_and_input_mutation_failures() {
        use super::super::candidate::{capture_baseline, materialize_from_baseline};
        use super::super::cargo_policy::CargoValidationPolicyV1;
        use super::super::evidence::{CheckResult, InterpretationStatus};
        use super::super::profile::{ProfileOrigin, detect_presets, resolve_cargo_profile};

        let root = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        cargo_workspace(root.path());
        let policy = CargoValidationPolicyV1 {
            additional_inputs: vec!["assets/seed.bin".into()],
            ..CargoValidationPolicyV1::default()
        };
        let cancellation = CancellationToken::new();
        for (index, kind, source, expected) in [
            (
                0,
                CheckKind::Compile,
                "pub fn broken()->u8 { \"wrong type\" }\n",
                ReceiptStatus::Failed,
            ),
            (
                1,
                CheckKind::Test,
                "#[test] fn fails(){panic!(\"expected failure\");}\n",
                ReceiptStatus::Failed,
            ),
            (
                2,
                CheckKind::Compile,
                "pub const ASSET:&[u8]=include_bytes!(\"../assets/seed.bin\");\n",
                ReceiptStatus::Error,
            ),
        ] {
            std::fs::write(root.path().join("src/lib.rs"), source).unwrap();
            if index == 2 {
                std::fs::write(
                    root.path().join("build.rs"),
                    "fn main(){std::fs::write(\"assets/seed.bin\",[9u8,0,255]).unwrap();}\n",
                )
                .unwrap();
            }
            let baseline = capture_baseline(
                root.path(),
                &data.path().join(format!("baseline-{index}")),
                &policy.additional_inputs,
            )
            .unwrap();
            let candidate = materialize_from_baseline(
                &baseline,
                &[],
                &data.path().join(format!("build-{index}")),
            )
            .unwrap();
            let profile = detect_presets(root.path())
                .into_iter()
                .find(|profile| profile.kind == kind)
                .unwrap();
            let snapshot = resolve_cargo_profile(
                &profile,
                ProfileOrigin::Preset,
                &policy,
                root.path(),
                &cancellation,
            )
            .await
            .unwrap();
            let context = ReceiptContext {
                stage_execution_id: Uuid::generate(),
                parent_generation: 1,
                profile_snapshot_id: Uuid::generate(),
                extra_environment: vec![(
                    "CARGO_TARGET_DIR".into(),
                    data.path().join("target").to_string_lossy().into_owned(),
                )],
            };
            let (receipt, details) = run_qualified_check(
                &snapshot,
                &policy,
                &candidate,
                &context,
                &|_: &str| None,
                &cancellation,
            )
            .await;
            assert_eq!(
                receipt.status, expected,
                "{:?}; {:?}",
                receipt.diagnostics, details.limitations
            );
            assert!(!details.qualifies());
            receipt.check_execution_semantics().unwrap();
            if index < 2 {
                assert_eq!(details.interpretation, InterpretationStatus::Supported);
                assert_eq!(details.result, CheckResult::Failed);
                assert!(details.candidate_integrity);
            } else {
                assert!(!details.candidate_integrity);
                assert_eq!(details.result, CheckResult::Unknown);
                assert_eq!(
                    std::fs::read(root.path().join("assets/seed.bin")).unwrap(),
                    [7, 0, 255]
                );
            }
        }
    }
}
