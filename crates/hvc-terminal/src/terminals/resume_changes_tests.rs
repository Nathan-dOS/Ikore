use std::sync::Mutex;

use hvc_contracts::AgentRole;
use hvc_contracts::external_changes::{ChangeKind, ExternalChangesDecision, render_notice};

use std::path::PathBuf;

use super::*;
use crate::host::{
    ContextRedaction, PreparedModelInput, RedactionResult, SnapshotFile, TerminalHost,
    TerminalUsageRecorder,
};
use crate::terminals::{
    AgentExecutionMode, CreateTerminalRunRequest, ExecutionOrigin, SafeCliWorkspace,
    TerminalRunStatus, materialize_safe_workspace, validate_resume_parent,
};
use crate::test_support::{TestDatabase, host as fixture_host};

/// A host whose workspace snapshot the test changes between calls.
struct ChangingHost {
    base: Arc<dyn TerminalHost>,
    snapshot: Mutex<SnapshotPreparation>,
}

impl std::fmt::Debug for ChangingHost {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChangingHost")
            .finish_non_exhaustive()
    }
}

impl TerminalHost for ChangingHost {
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
        Ok(self.snapshot.lock().unwrap().clone())
    }
    fn record_snapshot(
        &self,
        _: &Path,
        _: &TerminalRun,
        _: &SnapshotPreparation,
        _: u64,
    ) -> Result<(), TerminalError> {
        Ok(())
    }
}

/// Files as `(path, content)`; the projection is the content itself.
fn snapshot(files: &[(&str, &str)]) -> SnapshotPreparation {
    SnapshotPreparation {
        policy_identity: "policy-1".into(),
        files: files
            .iter()
            .map(|(path, content)| SnapshotFile {
                relative_path: PathBuf::from(path),
                original_sha256: Sha256::digest(content.as_bytes()).into(),
                projected_content: (*content).into(),
            })
            .collect(),
        redaction_count: 0,
        categories: Vec::new(),
        applied_exceptions: Vec::new(),
        original_bytes: files.iter().map(|(_, content)| content.len()).sum(),
    }
}

struct Fixture {
    directory: tempfile::TempDir,
    database: TestDatabase,
    repository: TerminalRepository,
    host: ChangingHost,
    project: String,
}

impl Fixture {
    fn new(files: &[(&str, &str)]) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let database = TestDatabase::initialize(directory.path()).unwrap();
        let project = database.insert_project(directory.path(), None).unwrap().id;
        let repository = TerminalRepository::new(database.database_path(), fixture_host());
        repository.ensure_project_configs(&project, false).unwrap();
        Self {
            directory,
            database,
            repository,
            host: ChangingHost {
                base: fixture_host(),
                snapshot: Mutex::new(snapshot(files)),
            },
            project,
        }
    }

    fn manual_run(&self) -> TerminalRun {
        self.repository
            .create_run(CreateTerminalRunRequest {
                project_id: self.project.clone(),
                agent_role: AgentRole::BackendAgent,
                origin: ExecutionOrigin::Manual,
                execution_mode: AgentExecutionMode::CliProcess,
                backend: Some(CliBackend::Codex),
                model: Some("fixture-model".into()),
                pulsation_id: None,
                job_id: None,
                agent_run_id: None,
                llm_execution_id: None,
            })
            .unwrap()
    }

    fn materialize(&self, run: &TerminalRun) -> SafeCliWorkspace {
        materialize_safe_workspace(
            &self.host,
            self.database.database_path(),
            self.directory.path(),
            run,
        )
        .unwrap()
    }

    fn set_snapshot(&self, files: &[(&str, &str)]) {
        *self.host.snapshot.lock().unwrap() = snapshot(files);
    }

    fn count(&self, sql: &str) -> u32 {
        Connection::open(self.database.database_path())
            .unwrap()
            .query_row(sql, [], |row| row.get(0))
            .unwrap()
    }
}

#[test]
fn a_manual_run_keeps_its_manifest_and_small_sanitized_blobs() {
    let big = "x".repeat(MAX_HUNK_FILE_BYTES + 1);
    let fixture = Fixture::new(&[("src/a.rs", "fn a() {}\n"), ("assets/big.txt", &big)]);
    let run = fixture.manual_run();
    let workspace = fixture.materialize(&run);
    let manifest = fixture.repository.snapshot_manifest(&run.id).unwrap();
    assert_eq!(manifest.len(), 2);
    assert_eq!(workspace.manifest.len(), 2);
    let small = manifest
        .iter()
        .find(|(entry, _)| entry.path == "src/a.rs")
        .unwrap();
    assert!(
        small.1.is_some(),
        "a small text file keeps its sanitized content"
    );
    let large = manifest
        .iter()
        .find(|(entry, _)| entry.path == "assets/big.txt")
        .unwrap();
    assert!(
        large.1.is_none(),
        "a file over 32 KiB keeps only its digest"
    );
    assert_eq!(
        fixture.count("SELECT COUNT(*) FROM terminal_snapshot_blobs"),
        1
    );
}

#[test]
fn changes_list_modified_renamed_created_and_deleted_with_hunks() {
    let fixture = Fixture::new(&[
        ("src/a.rs", "fn a() {}\nfn keep() {}\n"),
        ("docs/old.md", "moved text\n"),
        ("x.md", "gone\n"),
        ("keep.rs", "same\n"),
    ]);
    let parent = fixture.manual_run();
    fixture.materialize(&parent);
    let changed = snapshot(&[
        ("src/a.rs", "fn a() { 1 }\nfn keep() {}\n"),
        ("docs/new.md", "moved text\n"),
        ("c.rs", "new\n"),
        ("keep.rs", "same\n"),
    ]);
    let changes = external_changes(&fixture.repository, &parent, &changed).unwrap();
    assert_eq!(changes.decision, ExternalChangesDecision::Notice);
    let listed: Vec<(ChangeKind, &str, Option<&str>)> = changes
        .entries
        .iter()
        .map(|entry| {
            (
                entry.change,
                entry.path.as_str(),
                entry.from_path.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        listed,
        vec![
            (ChangeKind::Modified, "src/a.rs", None),
            (ChangeKind::Renamed, "docs/new.md", Some("docs/old.md")),
            (ChangeKind::Created, "c.rs", None),
            (ChangeKind::Deleted, "x.md", None),
        ]
    );
    let modified = &changes.entries[0];
    assert_eq!(
        (modified.added_lines, modified.removed_lines),
        (Some(1), Some(1))
    );
    let hunks = modified.hunks.as_deref().unwrap();
    assert!(hunks.contains("-fn a() {}") && hunks.contains("+fn a() { 1 }"));
    let notice = render_notice(&changes).unwrap();
    assert!(notice.starts_with("## External changes since your last turn in this session\n"));
    assert!(notice.contains("- modified `src/a.rs` (+1 -1 lines)"));
    assert!(notice.contains("- renamed `docs/old.md` -> `docs/new.md`"));
}

#[test]
fn a_resume_starts_only_on_the_snapshot_its_notice_described() {
    let fixture = Fixture::new(&[("src/a.rs", "fn a() {}\n")]);
    let parent = fixture.manual_run();
    fixture.materialize(&parent);
    let repository = &fixture.repository;
    repository
        .set_workspace_snapshot_hash(&parent.id, "workspace-a")
        .unwrap();
    repository
        .set_session_fingerprint(&parent.id, &session_fingerprint_v2(&parent))
        .unwrap();
    repository
        .set_session_fingerprint_version(&parent.id, 2)
        .unwrap();
    repository
        .set_native_session_id(&parent.id, "session-a")
        .unwrap();
    for status in [
        TerminalRunStatus::Starting,
        TerminalRunStatus::Running,
        TerminalRunStatus::Completed,
    ] {
        repository
            .set_run_status(&parent.id, status, Some(0), None, None)
            .unwrap();
    }
    let parent = repository.get_run(&parent.id).unwrap();

    // The developer edited the file; the notice describes that edit.
    fixture.set_snapshot(&[("src/a.rs", "fn a() { edited }\n")]);
    let preview = fixture.host.snapshot.lock().unwrap().clone();
    let changes = external_changes(repository, &parent, &preview).unwrap();
    assert_eq!(changes.entries.len(), 1);
    let resumed = fixture.manual_run();
    repository
        .set_external_changes_digest(&resumed.id, &changes_digest(&changes))
        .unwrap();
    let child = TerminalRun {
        parent_run_id: Some(parent.id.clone()),
        ..TerminalRun {
            id: resumed.id.clone(),
            ..parent.clone()
        }
    };
    let launch = fixture.materialize(&resumed);
    validate_resume_parent(repository, &child, &parent, &launch, Some("session-a")).unwrap();

    // Another edit between the preview and the launch: the notice is stale.
    fixture.set_snapshot(&[("src/a.rs", "fn a() { edited twice }\n"), ("b.rs", "b\n")]);
    let stale = fixture.materialize(&resumed);
    let error = validate_resume_parent(repository, &child, &parent, &stale, Some("session-a"))
        .unwrap_err()
        .to_string();
    assert!(error.contains("changed again"), "{error}");
}

#[test]
fn more_than_forty_changes_refuse_the_resume_with_the_count() {
    let fixture = Fixture::new(&[("src/a.rs", "fn a() {}\n")]);
    let parent = fixture.manual_run();
    fixture.materialize(&parent);
    let many: Vec<(String, String)> = (0..60)
        .map(|index| {
            (
                format!("src/new_{index:02}.rs"),
                format!("fn f{index}() {{}}\n"),
            )
        })
        .chain(std::iter::once((
            "src/a.rs".to_owned(),
            "fn a() {}\n".to_owned(),
        )))
        .collect();
    let files: Vec<(&str, &str)> = many
        .iter()
        .map(|(path, content)| (path.as_str(), content.as_str()))
        .collect();
    let changes = external_changes(&fixture.repository, &parent, &snapshot(&files)).unwrap();
    assert_eq!(changes.decision, ExternalChangesDecision::Refused);
    assert_eq!(
        hvc_contracts::external_changes::refusal_message(&changes),
        "60 files changed since the chosen session; start a new session"
    );
}

#[test]
fn blobs_stop_at_one_mebibyte_per_run_and_orphans_go_with_their_runs() {
    let contents: Vec<(String, String)> = (0..40)
        .map(|index| {
            (
                format!("data/{index:02}.txt"),
                format!("{index:02}").repeat(15 * 1024),
            )
        })
        .collect();
    let files: Vec<(&str, &str)> = contents
        .iter()
        .map(|(path, content)| (path.as_str(), content.as_str()))
        .collect();
    let fixture = Fixture::new(&files);
    let run = fixture.manual_run();
    fixture.materialize(&run);
    let kept: u64 = Connection::open(fixture.database.database_path())
        .unwrap()
        .query_row(
            "SELECT COALESCE(SUM(byte_len),0) FROM terminal_snapshot_blobs",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(kept <= MAX_RUN_BLOB_BYTES as u64, "{kept}");
    assert_eq!(
        fixture.count("SELECT COUNT(*) FROM terminal_snapshot_blobs"),
        34
    );
    assert_eq!(
        fixture.count("SELECT COUNT(*) FROM terminal_run_snapshot_files"),
        40
    );

    Connection::open(fixture.database.database_path())
        .unwrap()
        .execute("DELETE FROM terminal_runs WHERE id=?1", [&run.id])
        .unwrap();
    assert_eq!(
        fixture.count("SELECT COUNT(*) FROM terminal_run_snapshot_files"),
        0
    );
    assert_eq!(
        fixture
            .repository
            .delete_orphan_snapshot_blobs(&fixture.project)
            .unwrap(),
        34
    );
}

#[test]
fn only_manual_runs_keep_a_manifest() {
    let fixture = Fixture::new(&[("src/a.rs", "fn a() {}\n")]);
    let pipeline = fixture
        .repository
        .create_run(CreateTerminalRunRequest {
            project_id: fixture.project.clone(),
            agent_role: AgentRole::BackendAgent,
            origin: ExecutionOrigin::Pipeline,
            execution_mode: AgentExecutionMode::CliProcess,
            backend: Some(CliBackend::Codex),
            model: Some("fixture-model".into()),
            pulsation_id: None,
            job_id: None,
            agent_run_id: None,
            llm_execution_id: None,
        })
        .unwrap();
    fixture.materialize(&pipeline);
    assert_eq!(
        fixture.count("SELECT COUNT(*) FROM terminal_run_snapshot_files"),
        0
    );
}
