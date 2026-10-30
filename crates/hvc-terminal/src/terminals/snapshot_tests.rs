use hvc_contracts::AgentRole;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use super::{
    AgentExecutionMode, CliBackend, CreateTerminalRunRequest, ExecutionOrigin, TerminalError,
    TerminalRepository, TerminalRun, materialize_safe_workspace,
};
use crate::host::{
    ContextRedaction, PreparedModelInput, RedactionResult, SnapshotFile, SnapshotPreparation,
    TerminalHost, TerminalUsageRecorder,
};
use crate::test_support::{TestDatabase, host as fixture_host};

struct SnapshotHost {
    base: Arc<dyn TerminalHost>,
    snapshot: SnapshotPreparation,
    recorded: AtomicUsize,
}

impl std::fmt::Debug for SnapshotHost {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SnapshotHost")
            .field("files", &self.snapshot.files.len())
            .finish_non_exhaustive()
    }
}

impl TerminalHost for SnapshotHost {
    fn redact_context(&self, text: &str) -> Result<ContextRedaction, String> {
        self.base.redact_context(text)
    }
    fn redact_secrets(&self, text: &str) -> Result<RedactionResult, String> {
        self.base.redact_secrets(text)
    }
    fn sanitize_model_text_counted(&self, text: &str) -> Result<RedactionResult, String> {
        self.base.sanitize_model_text_counted(text)
    }
    fn prepare_input(
        &self,
        system: &str,
        user: &str,
        candidate: Option<PreparedModelInput>,
    ) -> Result<PreparedModelInput, TerminalError> {
        self.base.prepare_input(system, user, candidate)
    }
    fn record_prepared_input(
        &self,
        db: &Path,
        project: &str,
        pulse: Option<&str>,
        call: &str,
        input: &PreparedModelInput,
    ) -> Result<(), TerminalError> {
        self.base
            .record_prepared_input(db, project, pulse, call, input)
    }
    fn create_usage_recorder(
        &self,
        db: &Path,
        context: hvc_contracts::UsageContext,
        backend: CliBackend,
    ) -> rusqlite::Result<Arc<dyn TerminalUsageRecorder>> {
        self.base.create_usage_recorder(db, context, backend)
    }
    fn prepare_snapshot(
        &self,
        _: &Path,
        _: &Path,
        _: &TerminalRun,
        _: usize,
        _: u64,
    ) -> Result<SnapshotPreparation, TerminalError> {
        Ok(self.snapshot.clone())
    }
    fn record_snapshot(
        &self,
        _: &Path,
        _: &TerminalRun,
        _: &SnapshotPreparation,
        _: u64,
    ) -> Result<(), TerminalError> {
        self.recorded.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

fn host_with_file(path: &str, original: &str, policy: &str) -> SnapshotHost {
    SnapshotHost {
        base: fixture_host(),
        snapshot: SnapshotPreparation {
            policy_identity: policy.into(),
            files: vec![SnapshotFile {
                relative_path: PathBuf::from(path),
                original_sha256: Sha256::digest(original.as_bytes()).into(),
                projected_content: "safe projection".into(),
            }],
            redaction_count: 1,
            categories: vec!["fixture".into()],
            applied_exceptions: Vec::new(),
            original_bytes: original.len(),
        },
        recorded: AtomicUsize::new(0),
    }
}

/// The fixture database inside a `run()` directory; a manual run records its
/// snapshot manifest there (ADR-037 M3).
fn database(directory: &tempfile::TempDir) -> PathBuf {
    directory.path().join("native-terminal-test.db")
}

fn run() -> Result<(tempfile::TempDir, TerminalRun), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let database = TestDatabase::initialize(directory.path())?;
    let project = database.insert_project(directory.path(), None)?;
    let repository = TerminalRepository::new(database.database_path(), fixture_host());
    repository.ensure_project_configs(&project.id, false)?;
    let run = repository.create_run(CreateTerminalRunRequest {
        project_id: project.id,
        agent_role: AgentRole::BackendAgent,
        origin: ExecutionOrigin::Manual,
        execution_mode: AgentExecutionMode::CliProcess,
        backend: Some(CliBackend::Codex),
        model: Some("fixture-model".into()),
        pulsation_id: None,
        job_id: None,
        agent_run_id: None,
        llm_execution_id: None,
    })?;
    Ok((directory, run))
}

#[test]
fn materializer_preserves_safe_content_manifest_readonly_and_receipt()
-> Result<(), Box<dyn std::error::Error>> {
    let (directory, run) = run()?;
    let host = host_with_file("src/main.rs", "private original", "policy-1");
    let safe = materialize_safe_workspace(&host, &database(&directory), directory.path(), &run)?;
    let source = safe.path().join("src/main.rs");
    assert_eq!(fs::read_to_string(&source)?, "safe projection");
    assert!(fs::metadata(source)?.permissions().readonly());
    let manifest: Value =
        serde_json::from_slice(&fs::read(safe.path().join(".hvc-cli-snapshot.json"))?)?;
    assert_eq!(manifest["manifestSha256"], safe.manifest_hash);
    assert_eq!(manifest["files"], serde_json::json!(["src/main.rs"]));
    assert_eq!(host.recorded.load(Ordering::SeqCst), 1);
    Ok(())
}

#[test]
fn original_and_policy_remain_bound_when_safe_content_is_identical()
-> Result<(), Box<dyn std::error::Error>> {
    let (directory, run) = run()?;
    let a = materialize_safe_workspace(
        &host_with_file("main.rs", "original A", "policy-1"),
        &database(&directory),
        directory.path(),
        &run,
    )?;
    let b = materialize_safe_workspace(
        &host_with_file("main.rs", "original B", "policy-1"),
        &database(&directory),
        directory.path(),
        &run,
    )?;
    let c = materialize_safe_workspace(
        &host_with_file("main.rs", "original A", "policy-2"),
        &database(&directory),
        directory.path(),
        &run,
    )?;
    assert_ne!(a.manifest_hash, b.manifest_hash);
    assert_ne!(a.manifest_hash, c.manifest_hash);
    Ok(())
}

#[test]
fn traversal_fails_before_receipt_or_outside_file_can_be_created()
-> Result<(), Box<dyn std::error::Error>> {
    let (directory, run) = run()?;
    let host = host_with_file("../outside.rs", "original", "policy-1");
    let result = materialize_safe_workspace(&host, &database(&directory), directory.path(), &run);
    assert!(matches!(result, Err(TerminalError::Security(_))));
    assert_eq!(host.recorded.load(Ordering::SeqCst), 0);
    Ok(())
}
