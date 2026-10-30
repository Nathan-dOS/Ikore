use super::*;
use crate::llm::AgentRole;
use crate::persistence_service::{CreateProjectRequest, PersistenceService};
use crate::sanitization::CreateSecretRedactionException;
use crate::terminals::{
    AgentExecutionMode, CliInvocationRequest, CreateTerminalRunRequest, ExecutionOrigin,
    TerminalRepository, TerminalRunStatus, ThinkingLevel, build_process_spec,
};
use std::fs;

struct Fixture {
    _data: tempfile::TempDir,
    workspace: tempfile::TempDir,
    db_path: PathBuf,
    run: TerminalRun,
}

impl Fixture {
    fn new() -> Self {
        let data = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let persistence = PersistenceService::initialize(data.path()).unwrap();
        let project = persistence
            .create_project(CreateProjectRequest {
                workspace_path: workspace.path().to_string_lossy().into_owned(),
                name: Some("Terminal host policy fixture".into()),
            })
            .unwrap()
            .project;
        let db_path = persistence.database_path().to_path_buf();
        let repository = TerminalRepository::new(&db_path);
        repository
            .ensure_project_configs(&project.id, true)
            .unwrap();
        let run = Self::create_run(&db_path, &project.id);
        let fixture = Self {
            _data: data,
            workspace,
            db_path,
            run,
        };
        fixture.record_current_input();
        fixture
    }

    fn create_run(db_path: &Path, project_id: &str) -> TerminalRun {
        TerminalRepository::new(db_path)
            .create_run(CreateTerminalRunRequest {
                project_id: project_id.into(),
                agent_role: AgentRole::BackendAgent,
                origin: ExecutionOrigin::Pipeline,
                execution_mode: AgentExecutionMode::CliProcess,
                backend: Some(CliBackend::Codex),
                model: Some("fixture".into()),
                pulsation_id: None,
                job_id: None,
                agent_run_id: None,
                llm_execution_id: None,
            })
            .unwrap()
    }

    fn store(&self) -> SanitizationStore {
        SanitizationStore::new(&self.db_path)
    }

    fn record_current_input(&self) {
        let store = self.store();
        let policy = store.policy_for_project(&self.run.project_id).unwrap();
        store
            .record_input(
                &self.run.project_id,
                None,
                &self.run.id,
                policy.identity(),
                0,
                &[],
                0,
                0,
            )
            .unwrap();
    }

    fn snapshot(&self) -> SnapshotPreparation {
        CoreTerminalHost
            .prepare_snapshot(
                &self.db_path,
                self.workspace.path(),
                &self.run,
                10_000,
                64 * 1024 * 1024,
            )
            .unwrap()
    }
}

#[test]
fn safe_cli_workspace_excludes_secrets_ignored_files_and_vendor_configuration() {
    let fixture = Fixture::new();
    let root = fixture.workspace.path();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join(".codex")).unwrap();
    fs::write(root.join("src/lib.rs"), "pub fn safe() {}\n").unwrap();
    fs::write(root.join(".env"), "TOKEN=do-not-copy\n").unwrap();
    fs::write(
        root.join(".codex/instructions.md"),
        "untrusted vendor instructions\n",
    )
    .unwrap();
    fs::write(root.join(".gitignore"), "ignored.txt\n").unwrap();
    fs::write(root.join("ignored.txt"), "ignored\n").unwrap();
    let snapshot = fixture.snapshot();
    let paths: Vec<_> = snapshot
        .files
        .iter()
        .map(|file| file.relative_path.clone())
        .collect();
    assert!(paths.contains(&PathBuf::from("src/lib.rs")));
    assert!(!paths.contains(&PathBuf::from(".env")));
    assert!(!paths.iter().any(|path| path.starts_with(".codex")));
    assert!(!paths.contains(&PathBuf::from("ignored.txt")));
}

#[test]
fn cli_sanitization_guards_stdin_for_every_backend_and_preserves_the_original() {
    let original = "Authorization: Bearer synthetic_cli_secret_value";
    for backend in CliBackend::ALL {
        let spec = build_process_spec(CliInvocationRequest {
            executable: std::env::current_exe().unwrap(),
            backend,
            model: None,
            model_profile: None,
            thinking_level: ThinkingLevel::Default,
            prompt: original.into(),
            schema_file: None,
            schema: None,
            custom_args: Vec::new(),
            resume_session_id: None,
        })
        .unwrap();
        let stdin = String::from_utf8(spec.stdin_payload).unwrap();
        assert!(!stdin.contains("synthetic_cli_secret_value"), "{backend:?}");
        assert!(stdin.contains("<REDACTED_SECRET:"), "{backend:?}");
        assert!(
            spec.args
                .iter()
                .all(|arg| !arg.contains("synthetic_cli_secret_value"))
        );
    }
    let fixture = Fixture::new();
    let path = fixture.workspace.path().join("config.rs");
    fs::write(&path, original).unwrap();
    let snapshot = fixture.snapshot();
    let view = snapshot
        .files
        .iter()
        .find(|file| file.relative_path == Path::new("config.rs"))
        .unwrap();
    assert!(
        !view
            .projected_content
            .contains("synthetic_cli_secret_value")
    );
    assert_eq!(fs::read_to_string(path).unwrap(), original);
}

#[test]
fn cli_sanitization_snapshot_exception_changes_resume_identity() {
    let mut fixture = Fixture::new();
    let original = "Authorization: Bearer synthetic_snapshot_fixture";
    fs::write(fixture.workspace.path().join("fixture.rs"), original).unwrap();
    let first = fixture.snapshot();
    assert!(
        !first.files[0]
            .projected_content
            .contains("synthetic_snapshot_fixture")
    );
    let store = fixture.store();
    let preview = store
        .preview_file(&fixture.run.project_id, "fixture.rs")
        .unwrap();
    let span = &preview.spans[0];
    store
        .create_exception(&CreateSecretRedactionException {
            project_id: fixture.run.project_id.clone(),
            relative_path: "fixture.rs".into(),
            detector_id: span.detector_id.clone(),
            detector_version: span.detector_version,
            original_file_sha256: preview.original_file_sha256,
            start_byte: span.start_byte,
            end_byte: span.end_byte,
            reason: "Synthetic fixture reviewed locally".into(),
        })
        .unwrap();
    // Receipts are immutable per call. A policy change needs a newly prepared run.
    TerminalRepository::new(&fixture.db_path)
        .set_run_status(
            &fixture.run.id,
            TerminalRunStatus::Cancelled,
            None,
            None,
            None,
        )
        .unwrap();
    fixture.run = Fixture::create_run(&fixture.db_path, &fixture.run.project_id);
    fixture.record_current_input();
    let second = fixture.snapshot();
    assert_eq!(second.files[0].projected_content, original);
    assert_ne!(first.policy_identity, second.policy_identity);
    assert_eq!(
        first.files[0].original_sha256,
        second.files[0].original_sha256
    );
}

#[test]
fn cli_sanitization_original_change_invalidates_identical_projected_snapshot() {
    let fixture = Fixture::new();
    let path = fixture.workspace.path().join("config.rs");
    fs::write(&path, "Authorization: Bearer synthetic_first_value").unwrap();
    let first = fixture.snapshot();
    fs::write(&path, "Authorization: Bearer synthetic_second_value").unwrap();
    let second = fixture.snapshot();
    assert_eq!(
        first.files[0].projected_content,
        second.files[0].projected_content
    );
    assert_ne!(
        first.files[0].original_sha256,
        second.files[0].original_sha256
    );
}

#[test]
fn cli_sanitization_rejects_policy_revoked_between_input_and_snapshot() {
    let fixture = Fixture::new();
    let before = fixture
        .store()
        .policy_for_project(&fixture.run.project_id)
        .unwrap();
    validate_cli_input_policy(&fixture.db_path, &fixture.run, &before).unwrap();
    let mut changed = before;
    changed.identity = "changed-after-input-preparation".into();
    assert!(validate_cli_input_policy(&fixture.db_path, &fixture.run, &changed).is_err());
    let db = Connection::open(&fixture.db_path).unwrap();
    db.execute(
        "UPDATE secret_sanitization_receipts SET policy_identity=?1 WHERE call_id=?2",
        params![SANITIZATION_POLICY_VERSION, fixture.run.id],
    )
    .unwrap();
    validate_cli_input_policy(&fixture.db_path, &fixture.run, &changed).unwrap();
    db.execute(
        "DELETE FROM secret_sanitization_receipts WHERE call_id=?1",
        [&fixture.run.id],
    )
    .unwrap();
    assert!(
        CoreTerminalHost
            .prepare_snapshot(
                &fixture.db_path,
                fixture.workspace.path(),
                &fixture.run,
                10,
                1024
            )
            .is_err()
    );
}

#[derive(Debug)]
struct ForgedProof;
impl InputProof for ForgedProof {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn system_prompt(&self) -> &str {
        panic!("untrusted proof getters must not be read")
    }
    fn user_prompt(&self) -> &str {
        panic!("untrusted proof getters must not be read")
    }
    fn policy_identity(&self) -> &str {
        panic!("untrusted proof getters must not be read")
    }
    fn redaction_count(&self) -> u64 {
        0
    }
    fn categories(&self) -> &[String] {
        &[]
    }
    fn original_bytes(&self) -> usize {
        0
    }
}

#[test]
fn terminal_host_rejects_a_forged_proof_without_consuming_its_getters() {
    assert!(matches!(
        CoreTerminalHost.prepare_input("system", "user", Some(Arc::new(ForgedProof))),
        Err(TerminalError::Redaction(_))
    ));
}

#[test]
fn terminal_host_rejects_a_proof_for_changed_prompts() {
    let proof = PreparedModelInput::new("system", "original input").unwrap();
    for (system, user) in [
        ("changed system", "original input"),
        ("system", "changed input"),
    ] {
        assert!(matches!(
            CoreTerminalHost.prepare_input(system, user, Some(Arc::new(proof.clone()))),
            Err(TerminalError::Redaction(_))
        ));
    }
}

#[derive(Debug)]
struct SpoofedGetters(PreparedModelInput);
impl InputProof for SpoofedGetters {
    fn as_any(&self) -> &dyn Any {
        &self.0
    }
    fn system_prompt(&self) -> &str {
        "forged system"
    }
    fn user_prompt(&self) -> &str {
        "forged user"
    }
    fn policy_identity(&self) -> &str {
        "forged policy"
    }
    fn redaction_count(&self) -> u64 {
        0
    }
    fn categories(&self) -> &[String] {
        &[]
    }
    fn original_bytes(&self) -> usize {
        0
    }
}

#[test]
fn terminal_host_returns_the_validated_concrete_proof_instead_of_candidate_getters() {
    let proof = PreparedModelInput::new("system", "user").unwrap();
    let prepared = CoreTerminalHost
        .prepare_input("system", "user", Some(Arc::new(SpoofedGetters(proof))))
        .unwrap();
    assert_eq!(prepared.system_prompt(), "system");
    assert_eq!(prepared.user_prompt(), "user");
    assert_eq!(prepared.policy_identity(), SANITIZATION_POLICY_VERSION);
    assert!(prepared.as_any().is::<PreparedModelInput>());
}

#[test]
fn terminal_host_preserves_only_the_authorized_occurrence_and_redacts_unprepared_input() {
    let secret = "ghp_synthetic_authorized_fixture";
    let original = format!("{secret}\n{secret}");
    let proof = PreparedModelInput::with_authorized_ranges(
        "",
        &original,
        "fixture-policy",
        &[],
        std::slice::from_ref(&(0..secret.len())),
    )
    .unwrap();
    let projected = proof.user_prompt().to_owned();
    let prepared = CoreTerminalHost
        .prepare_input("", &projected, Some(Arc::new(proof)))
        .unwrap();
    assert_eq!(prepared.user_prompt().matches(secret).count(), 1);
    assert!(prepared.user_prompt().contains("<REDACTED_SECRET:"));
    let default = CoreTerminalHost.prepare_input("", &original, None).unwrap();
    assert!(!default.user_prompt().contains(secret));
    assert_eq!(default.redaction_count(), 2);
    assert!(!format!("{default:?}").contains(secret));
}

#[test]
fn native_terminal_fixture_schema_matches_migrated_host_columns() {
    let fixture = Fixture::new();
    let migrated = Connection::open(&fixture.db_path).unwrap();
    let native = Connection::open_in_memory().unwrap();
    native
        .execute_batch(include_str!(
            "../../hvc-terminal/src/test_support/schema.sql"
        ))
        .unwrap();
    let tables: Vec<String> = native.prepare(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name"
    ).unwrap().query_map([], |row| row.get(0)).unwrap().collect::<Result<_, _>>().unwrap();
    for table in tables {
        let columns = |connection: &Connection| {
            connection.prepare(
                "SELECT name,type,\"notnull\",dflt_value,pk FROM pragma_table_info(?1) ORDER BY cid"
            ).unwrap().query_map([&table], |row| Ok((
                row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(3)?, row.get::<_, i64>(4)?
            ))).unwrap().collect::<Result<Vec<_>, _>>().unwrap()
        };
        assert_eq!(
            columns(&native),
            columns(&migrated),
            "Native fixture drifted from migrated {table}"
        );
    }
}

#[test]
fn terminal_host_reports_production_redaction_categories() {
    let projection = CoreTerminalHost
        .redact_secrets("Authorization: Bearer fixture-secret-token")
        .unwrap();
    assert_eq!(projection.count, 1);
    assert_eq!(projection.categories, ["bearer_token"]);
    assert!(!projection.text.contains("fixture-secret-token"));
}

/// ADR-037 M3: resuming a session after an edit tells the agent what
/// changed, with hunks of the sanitized content only: a secret added to the
/// changed file reaches the notice redacted.
#[tokio::test]
async fn a_resume_notice_carries_only_sanitized_hunks() {
    let data = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    fs::create_dir_all(workspace.path().join("src")).unwrap();
    fs::write(
        workspace.path().join("src/config.rs"),
        "pub const RETRIES: u32 = 3;\n",
    )
    .unwrap();
    let persistence = PersistenceService::initialize(data.path()).unwrap();
    let project = persistence
        .create_project(CreateProjectRequest {
            workspace_path: workspace.path().to_string_lossy().into_owned(),
            name: Some("Resume notice fixture".into()),
        })
        .unwrap()
        .project;
    let db_path = persistence.database_path().to_path_buf();
    let repository = TerminalRepository::new(&db_path);
    repository
        .ensure_project_configs(&project.id, true)
        .unwrap();
    let run = repository
        .create_run(CreateTerminalRunRequest {
            project_id: project.id.clone(),
            agent_role: AgentRole::BackendAgent,
            origin: ExecutionOrigin::Manual,
            execution_mode: AgentExecutionMode::CliProcess,
            backend: Some(CliBackend::Codex),
            model: Some("fixture".into()),
            pulsation_id: None,
            job_id: None,
            agent_run_id: None,
            llm_execution_id: None,
        })
        .unwrap();
    let store = SanitizationStore::new(&db_path);
    let policy = store.policy_for_project(&project.id).unwrap();
    store
        .record_input(&project.id, None, &run.id, policy.identity(), 0, &[], 0, 0)
        .unwrap();
    // The session's launch: its snapshot and manifest.
    CoreTerminalHost
        .prepare_workspace(&db_path, workspace.path(), &run)
        .unwrap();
    rusqlite::Connection::open(&db_path)
        .unwrap()
        .execute(
            "INSERT INTO terminal_run_resume_state(run_id,session_fingerprint_version) VALUES(?1,2)",
            [&run.id],
        )
        .unwrap();

    fs::write(
        workspace.path().join("src/config.rs"),
        "pub const RETRIES: u32 = 5;\npub const AUTH: &str = \"Authorization: Bearer fixture-secret-token\";\n",
    )
    .unwrap();
    let terminals = Arc::new(
        crate::terminals::TerminalService::new(
            &db_path,
            tokio::runtime::Handle::current(),
            Arc::new(crate::events::AppEventBus::default()),
        )
        .unwrap(),
    );
    let crate::terminals::ResumePreview::Changes(changes) = terminals
        .preview_resume(&project.id, &run.id)
        .await
        .unwrap()
    else {
        panic!("a session with a manifest previews its changes");
    };
    let notice = hvc_contracts::external_changes::render_notice(&changes).unwrap();
    assert!(
        notice.contains("- modified `src/config.rs` (+2 -1 lines)"),
        "{notice}"
    );
    assert!(notice.contains("+pub const RETRIES: u32 = 5;"));
    assert!(!notice.contains("fixture-secret-token"));
    assert!(notice.contains("REDACTED"), "{notice}");
}
