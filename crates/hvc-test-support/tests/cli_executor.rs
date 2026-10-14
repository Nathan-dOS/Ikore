use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hvc_core::chat_service::{
    ChatExecutionTargetRequest, ChatService, CreateChatConversationRequest,
    GetChatConversationRequest, NoopChatEventSink, SendChatMessageRequest,
};
use hvc_core::llm::credentials::CredentialStore;
use hvc_core::llm::error::LlmError;
use hvc_core::llm::{AgentExecutionTarget, AgentRole, LlmRequest, ResponseFormat};
use hvc_core::persistence_service::{CreateProjectRequest, PersistenceService};
use hvc_core::terminals::{
    AgentExecutionMode, CliAvailabilityState, CliBackend, CliModelCatalogState,
    CliModelSelectionSource, ResumeManualRunRequest, StartManualRunRequest, TerminalEventEnvelope,
    TerminalEventSink, TerminalRunStatus, TerminalService, ThinkingLevel,
    UpdateCliAgentConfigRequest,
};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct CollectSink(Mutex<Vec<TerminalEventEnvelope>>);

impl TerminalEventSink for CollectSink {
    fn emit(&self, envelope: &TerminalEventEnvelope) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(envelope.clone());
    }
}

struct Harness {
    _data: TempDir,
    _workspace: TempDir,
    project_id: String,
    database_path: PathBuf,
    terminals: Arc<TerminalService>,
    sink: Arc<CollectSink>,
}

fn harness(custom_args: Vec<String>) -> Harness {
    let data = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(workspace.path().join("src")).unwrap();
    std::fs::write(workspace.path().join("src/lib.rs"), "pub fn fixture() {}\n").unwrap();
    let persistence = PersistenceService::initialize(data.path()).unwrap();
    let project = persistence
        .create_project(CreateProjectRequest {
            workspace_path: workspace.path().to_string_lossy().into_owned(),
            name: Some("CLI executor fixture".into()),
        })
        .unwrap()
        .project;
    let sink = Arc::new(CollectSink::default());
    let terminals = Arc::new(
        TerminalService::new(
            persistence.database_path(),
            tokio::runtime::Handle::current(),
            sink.clone(),
        )
        .unwrap(),
    );
    terminals
        .repository()
        .ensure_project_configs(&project.id, false)
        .unwrap();
    let current = terminals
        .repository()
        .get_config(&project.id, AgentRole::BackendAgent)
        .unwrap();
    let custom = terminals
        .update_config(UpdateCliAgentConfigRequest {
            project_id: project.id.clone(),
            agent_role: AgentRole::BackendAgent,
            execution_mode: AgentExecutionMode::ApiDirect,
            backend: CliBackend::Custom,
            model: Some("haiku".into()),
            model_profile: None,
            model_source: CliModelSelectionSource::Manual,
            thinking_level: ThinkingLevel::Default,
            custom_executable: Some(env!("CARGO_BIN_EXE_cli_fixture").into()),
            custom_args: [vec!["--fixture-model".into(), "haiku".into()], custom_args].concat(),
            custom_risk_ack: true,
            expected_version: current.version,
        })
        .unwrap();
    terminals
        .update_config(UpdateCliAgentConfigRequest {
            project_id: project.id.clone(),
            agent_role: AgentRole::BackendAgent,
            execution_mode: AgentExecutionMode::CliProcess,
            backend: CliBackend::Custom,
            model: Some("haiku".into()),
            model_profile: None,
            model_source: CliModelSelectionSource::Manual,
            thinking_level: ThinkingLevel::Default,
            custom_executable: custom.custom_executable.clone(),
            custom_args: custom.custom_args.clone(),
            custom_risk_ack: true,
            expected_version: custom.version,
        })
        .unwrap();
    Harness {
        _data: data,
        _workspace: workspace,
        project_id: project.id,
        database_path: persistence.database_path().to_path_buf(),
        terminals,
        sink,
    }
}

#[derive(Default)]
struct NoHvcCredentials(AtomicUsize);

impl CredentialStore for NoHvcCredentials {
    fn get(&self, _credential_ref: &str) -> Result<Option<String>, LlmError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(None)
    }

    fn set(&self, _credential_ref: &str, _secret: &str) -> Result<(), LlmError> {
        panic!("CLI Companion must not store HVC credentials")
    }

    fn delete(&self, _credential_ref: &str) -> Result<bool, LlmError> {
        panic!("CLI Companion must not delete HVC credentials")
    }
}

fn request(project_id: &str, deadline: Instant) -> LlmRequest {
    LlmRequest {
        execution_id: "fixture-execution".to_string().into(),
        generation_id: "fixture-generation".to_string().into(),
        project_id: project_id.into(),
        pulsation_id: None,
        job_id: None,
        agent_run_id: None,
        job_chain: "fixture".into(),
        target: AgentExecutionTarget::canonical(AgentRole::BackendAgent),
        stage: "Code".into(),
        system_prompt: "Read-only fixture".into(),
        user_prompt: "payload-from-stdin".into(),
        response_format: ResponseFormat::Text,
        max_output_tokens: 100,
        temperature: 0.0,
        deadline,
        stream: true,
        legacy_provider: "fixture".into(),
        legacy_model: "fixture".into(),
        dispatch: hvc_core::llm::DispatchPolicy::default(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn progressive_cli_budget_outlives_api_deadline_without_an_active_viewer() {
    let harness = harness(vec!["--mode".into(), "paced".into()]);
    let initial = Instant::now() + Duration::from_millis(500);
    let mut request = request(&harness.project_id, initial);
    request.dispatch.cli_timeout = Some(hvc_core::agent_timeout::CliTimeoutBudget {
        policy: hvc_core::AgentTimeoutPolicy::new(500, 1_000, 5_000, 250),
        deadline: Instant::now() + Duration::from_secs(5),
    });
    let result = harness
        .terminals
        .execute_llm_request(&request, &CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert!(Instant::now() > initial);
    assert_eq!(
        request.deadline, initial,
        "API deadline must remain unchanged"
    );
    assert!(result.raw_output.contains("completed after initial budget"));
    let run = harness
        .terminals
        .repository()
        .get_run(&result.terminal_run_id)
        .unwrap();
    assert_eq!(run.status, TerminalRunStatus::Completed);
    let events = harness
        .terminals
        .repository()
        .list_events(&run.id, 0, 100)
        .unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.content.contains("max_queue_delay_ms="))
    );
}

/// ADR-025: the same silent CLI expires under the normal silence window and
/// completes inside the observation trial, which records how long the pipes
/// stayed quiet. The window is scaled down from 90 s to keep the test fast.
#[tokio::test(flavor = "multi_thread")]
async fn a_silent_cli_expires_normally_but_completes_inside_the_observation_trial() {
    let harness = harness(vec![
        "--mode".into(),
        "quiet".into(),
        "--quiet-ms".into(),
        "2500".into(),
    ]);
    let short_window = || hvc_core::agent_timeout::CliTimeoutBudget {
        policy: hvc_core::AgentTimeoutPolicy::new(300, 300, 10_000, 100),
        deadline: Instant::now() + Duration::from_secs(10),
    };
    let db = rusqlite::Connection::open(&harness.database_path).unwrap();
    db.execute(
        "INSERT INTO pulsations(id,project_id,feature_name,stage,status,created_at,updated_at) VALUES('trial-pulse',?1,'Quiet','Code','running',?2,?2)",
        rusqlite::params![harness.project_id, chrono::Utc::now().to_rfc3339()],
    )
    .unwrap();

    let mut outside = request(
        &harness.project_id,
        Instant::now() + Duration::from_secs(10),
    );
    outside.pulsation_id = Some("trial-pulse".into());
    outside.dispatch.cli_timeout = Some(short_window());
    let error = harness
        .terminals
        .execute_llm_request(&outside, &CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.code(), "cli_inactivity_timeout");

    let trial = harness
        .terminals
        .repository()
        .start_cli_observation_trial(&harness.project_id)
        .unwrap();
    let mut inside = request(
        &harness.project_id,
        Instant::now() + Duration::from_secs(10),
    );
    inside.pulsation_id = Some("trial-pulse".into());
    inside.dispatch.cli_timeout = Some(short_window());
    let result = harness
        .terminals
        .execute_llm_request(&inside, &CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert!(result.raw_output.contains("completed after silence"));
    let run = harness
        .terminals
        .repository()
        .get_run(&result.terminal_run_id)
        .unwrap();
    assert_eq!(run.status, TerminalRunStatus::Completed);
    assert_eq!(run.observation_trial_id.as_deref(), Some(trial.id.as_str()));
    assert!(run.deadline_at.is_none(), "a trial run carries no deadline");
    let silence: i64 = db
        .query_row(
            "SELECT max_transport_silence_ms FROM terminal_runs WHERE id=?1",
            [&run.id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(silence >= 1_300, "pipes were quiet ~1.5 s: {silence}");
    let report = harness
        .terminals
        .repository()
        .latest_cli_observation_report(&harness.project_id)
        .unwrap()
        .unwrap();
    assert_eq!((report.trial.admitted_runs, report.successful_runs), (1, 1));
    assert!(report.max_transport_silence_ms.unwrap() >= 1_300);
    assert!(report.runs[0].prep_ms.is_some() && report.runs[0].boot_ms.is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn explicit_short_cli_ceiling_is_not_extended_by_progress() {
    let harness = harness(vec!["--mode".into(), "paced".into()]);
    let mut request = request(&harness.project_id, Instant::now() + Duration::from_secs(5));
    request.dispatch.cli_timeout = Some(hvc_core::agent_timeout::CliTimeoutBudget {
        policy: hvc_core::AgentTimeoutPolicy::new(250, 1_000, 5_000, 250),
        deadline: Instant::now() + Duration::from_millis(700),
    });
    let error = harness
        .terminals
        .execute_llm_request(&request, &CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.code(), "cli_timeout");
}

async fn wait_for_status(
    harness: &Harness,
    run_id: &str,
    expected: TerminalRunStatus,
) -> hvc_core::terminals::TerminalRunStatus {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = harness
                .terminals
                .repository()
                .get_run(run_id)
                .unwrap()
                .status;
            if status == expected {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}

static PATH_TEST_LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();

struct PathRestore(Option<std::ffi::OsString>);

impl Drop for PathRestore {
    fn drop(&mut self) {
        // SAFETY: this integration test serializes its PATH mutation with PATH_TEST_LOCK and
        // restores the exact prior value before releasing the guard.
        unsafe {
            if let Some(path) = self.0.take() {
                std::env::set_var("PATH", path);
            } else {
                std::env::remove_var("PATH");
            }
        }
    }
}

#[tokio::test]
async fn official_adapters_capture_and_resume_the_exact_native_session() {
    let _path_lock = PATH_TEST_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let binaries = tempfile::tempdir().unwrap();
    for backend in ["codex", "claude", "gemini", "opencode", "agy"] {
        std::fs::copy(
            env!("CARGO_BIN_EXE_cli_fixture"),
            binaries.path().join(format!("{backend}.exe")),
        )
        .unwrap();
    }
    let original_path = std::env::var_os("PATH");
    let _restore = PathRestore(original_path.clone());
    let mut paths = vec![binaries.path().to_path_buf()];
    if let Some(original_path) = original_path {
        paths.extend(std::env::split_paths(&original_path));
    }
    let joined = std::env::join_paths(paths).unwrap();
    // SAFETY: guarded and restored by PATH_TEST_LOCK/PathRestore for the duration of this test.
    unsafe { std::env::set_var("PATH", joined) };

    let harness = harness(vec!["--mode".into(), "success".into()]);
    let discovery = harness.terminals.refresh_discovery().await;
    for backend in [
        CliBackend::Codex,
        CliBackend::Claude,
        CliBackend::Gemini,
        CliBackend::Opencode,
        CliBackend::Antigravity,
    ] {
        let model = match backend {
            CliBackend::Codex => "gpt-5.6-luna",
            CliBackend::Claude => "haiku",
            CliBackend::Gemini => "gemini-2.5-flash",
            CliBackend::Opencode => "fixture-opencode-model",
            CliBackend::Antigravity => "gemini-3.8-flash",
            CliBackend::Custom => unreachable!(),
        };
        assert!(discovery.iter().any(|item| {
            item.backend == backend && item.state == CliAvailabilityState::Available
        }));
        let current = harness
            .terminals
            .repository()
            .get_config(&harness.project_id, AgentRole::BackendAgent)
            .unwrap();
        harness
            .terminals
            .update_config(UpdateCliAgentConfigRequest {
                project_id: harness.project_id.clone(),
                agent_role: AgentRole::BackendAgent,
                execution_mode: AgentExecutionMode::CliProcess,
                backend,
                model: Some(model.into()),
                model_profile: None,
                model_source: CliModelSelectionSource::Manual,
                thinking_level: ThinkingLevel::Default,
                custom_executable: None,
                custom_args: Vec::new(),
                custom_risk_ack: false,
                expected_version: current.version,
            })
            .unwrap();
        let initial = harness
            .terminals
            .start_manual(StartManualRunRequest {
                project_id: harness.project_id.clone(),
                agent_role: AgentRole::BackendAgent,
                prompt: "primeiro turno".into(),
            })
            .unwrap();
        wait_for_status(&harness, &initial.id, TerminalRunStatus::Completed).await;
        let initial = harness.terminals.repository().get_run(&initial.id).unwrap();
        assert_eq!(
            initial.native_session_id.as_deref(),
            Some("fixture-native-session")
        );
        let resumed = harness
            .terminals
            .resume_manual(ResumeManualRunRequest {
                project_id: harness.project_id.clone(),
                source_run_id: initial.id.clone(),
                prompt: "segundo turno".into(),
            })
            .unwrap();
        wait_for_status(&harness, &resumed.id, TerminalRunStatus::Completed).await;
        let resumed = harness.terminals.repository().get_run(&resumed.id).unwrap();
        assert_eq!(resumed.parent_run_id.as_deref(), Some(initial.id.as_str()));
        assert_eq!(
            resumed.continuation_reason.as_deref(),
            Some("explicit_resume")
        );
        assert_eq!(resumed.native_session_id, initial.native_session_id);
    }
    let usage = hvc_core::usage::query_usage(&harness.database_path, &Default::default()).unwrap();
    assert_eq!(usage.totals.total_tokens, Some(250));
    assert_eq!(usage.totals.records, 10);
    assert_eq!(usage.totals.unavailable_records, 0);
}

#[tokio::test]
async fn claude_sonnet_generation_rejects_haiku_output_and_terminates_the_process_tree() {
    let _path_lock = PATH_TEST_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let binaries = tempfile::tempdir().unwrap();
    let claude = binaries.path().join("claude.exe");
    std::fs::copy(env!("CARGO_BIN_EXE_cli_fixture"), &claude).unwrap();
    std::fs::write(binaries.path().join("claude.fixture-state"), "sonnet_mismatch").unwrap();
    let marker = binaries.path().join("claude.fixture-after-mismatch");
    let original_path = std::env::var_os("PATH");
    let _restore = PathRestore(original_path.clone());
    let mut paths = vec![binaries.path().to_path_buf()];
    if let Some(original_path) = original_path {
        paths.extend(std::env::split_paths(&original_path));
    }
    // SAFETY: PATH_TEST_LOCK serializes this mutation and PathRestore restores the variable.
    unsafe { std::env::set_var("PATH", std::env::join_paths(paths).unwrap()) };

    let h = harness(Vec::new());
    let current = h
        .terminals
        .repository()
        .get_config(&h.project_id, AgentRole::BackendAgent)
        .unwrap();
    h.terminals
        .update_config(UpdateCliAgentConfigRequest {
            project_id: h.project_id.clone(),
            agent_role: AgentRole::BackendAgent,
            execution_mode: AgentExecutionMode::CliProcess,
            backend: CliBackend::Claude,
            model: Some("haiku".into()),
            model_profile: None,
            model_source: CliModelSelectionSource::Manual,
            thinking_level: ThinkingLevel::Default,
            custom_executable: None,
            custom_args: Vec::new(),
            custom_risk_ack: false,
            expected_version: current.version,
        })
        .unwrap();

    let error = h
        .terminals
        .execute_llm_request(
            &request(&h.project_id, Instant::now() + Duration::from_secs(15)),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), "model_mismatch");

    let db = rusqlite::Connection::open(&h.database_path).unwrap();
    let (run_id, status, error_code, observed_model): (String, String, Option<String>, Option<String>) = db
        .query_row(
            "SELECT id,status,error_code,observed_model FROM terminal_runs ORDER BY created_at DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
        )
        .unwrap();
    assert_eq!(status, "failed");
    assert_eq!(error_code.as_deref(), Some("model_mismatch"));
    assert_eq!(observed_model.as_deref(), Some("claude-sonnet-5"));
    let generation = db
        .query_row(
            "SELECT model_id,message_id,is_generation FROM terminal_model_observations WHERE run_id=?1 AND is_generation=1 ORDER BY ordinal LIMIT 1",
            [&run_id],
            |row| Ok((row.get::<_,Option<String>>(0)?,row.get::<_,Option<String>>(1)?,row.get::<_,bool>(2)?)),
        )
        .unwrap();
    assert_eq!(generation.0.as_deref(), Some("claude-sonnet-5"));
    assert_eq!(generation.1.as_deref(), Some("claude-message-1"));
    assert!(generation.2);
    assert!(!marker.exists(), "the host must kill the local process tree immediately");
}

/// "Haiku CLI teste 1" (2026-09-26): a Claude run spent 44,354 output tokens
/// against a 16,000-token reservation and nothing stopped it. The ceiling is
/// twice the reservation (floor 24,000); the fixture reports 40,000.
#[tokio::test]
async fn claude_run_past_its_output_ceiling_is_killed_and_its_partial_usage_recorded() {
    let _path_lock = PATH_TEST_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let binaries = tempfile::tempdir().unwrap();
    let claude = binaries.path().join("claude.exe");
    std::fs::copy(env!("CARGO_BIN_EXE_cli_fixture"), &claude).unwrap();
    std::fs::write(binaries.path().join("claude.fixture-state"), "output_runaway").unwrap();
    let marker = binaries.path().join("claude.fixture-after-mismatch");
    let original_path = std::env::var_os("PATH");
    let _restore = PathRestore(original_path.clone());
    let mut paths = vec![binaries.path().to_path_buf()];
    if let Some(original_path) = original_path {
        paths.extend(std::env::split_paths(&original_path));
    }
    // SAFETY: PATH_TEST_LOCK serializes this mutation and PathRestore restores the variable.
    unsafe { std::env::set_var("PATH", std::env::join_paths(paths).unwrap()) };

    let h = harness(Vec::new());
    let current = h
        .terminals
        .repository()
        .get_config(&h.project_id, AgentRole::BackendAgent)
        .unwrap();
    h.terminals
        .update_config(UpdateCliAgentConfigRequest {
            project_id: h.project_id.clone(),
            agent_role: AgentRole::BackendAgent,
            execution_mode: AgentExecutionMode::CliProcess,
            backend: CliBackend::Claude,
            model: Some("haiku".into()),
            model_profile: None,
            model_source: CliModelSelectionSource::Manual,
            thinking_level: ThinkingLevel::Default,
            custom_executable: None,
            custom_args: Vec::new(),
            custom_risk_ack: false,
            expected_version: current.version,
        })
        .unwrap();

    let mut call = request(&h.project_id, Instant::now() + Duration::from_secs(15));
    call.max_output_tokens = 16_000;
    let error = h
        .terminals
        .execute_llm_request(&call, &CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.code(), "cli_output_budget_exceeded");

    let db = rusqlite::Connection::open(&h.database_path).unwrap();
    let (run_id, status, error_code, reason): (String, String, Option<String>, Option<String>) = db
        .query_row(
            "SELECT id,status,error_code,termination_reason FROM terminal_runs ORDER BY created_at DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(status, "failed");
    assert_eq!(error_code.as_deref(), Some("cli_output_budget_exceeded"));
    assert_eq!(reason.as_deref(), Some("output_limit"));
    let (output, quality): (i64, String) = db
        .query_row(
            "SELECT output_tokens,quality FROM token_usage_records WHERE source_kind='terminal_run' AND source_id=?1",
            [&run_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((output, quality.as_str()), (40_000, "partial"));
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert!(!marker.exists(), "the host must kill the process before it finishes");
}

/// ADR-022/025 live qualification: one synthetic request through the real
/// authenticated Claude CLI. It is explicit and ignored in normal CI.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "Requires local Claude authentication and network; sends one synthetic Haiku identity probe"]
async fn authenticated_claude_model_identity_smoke() {
    let harness = harness(Vec::new());
    let current = harness
        .terminals
        .repository()
        .get_config(&harness.project_id, AgentRole::BackendAgent)
        .unwrap();
    harness
        .terminals
        .update_config(UpdateCliAgentConfigRequest {
            project_id: harness.project_id.clone(),
            agent_role: AgentRole::BackendAgent,
            execution_mode: AgentExecutionMode::CliProcess,
            backend: CliBackend::Claude,
            model: Some("haiku".into()),
            model_profile: None,
            model_source: CliModelSelectionSource::Manual,
            thinking_level: ThinkingLevel::Default,
            custom_executable: None,
            custom_args: Vec::new(),
            custom_risk_ack: false,
            expected_version: current.version,
        })
        .unwrap();

    let mut probe = request(&harness.project_id, Instant::now() + Duration::from_secs(120));
    probe.user_prompt = "Reply with exactly HVC_MODEL_IDENTITY_OK and no tools.".into();
    probe.max_output_tokens = 32;
    let result = harness
        .terminals
        .execute_llm_request(&probe, &CancellationToken::new())
        .await;

    let db = rusqlite::Connection::open(&harness.database_path).unwrap();
    let (run_id, status, error_code, selected, observed): (
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = db
        .query_row(
            "SELECT id,status,error_code,model,observed_model FROM terminal_runs WHERE project_id=?1 ORDER BY created_at DESC LIMIT 1",
            [&harness.project_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(selected.as_deref(), Some("haiku"));
    let generations: Vec<Option<String>> = db
        .prepare(
            "SELECT model_id FROM terminal_model_observations WHERE run_id=?1 AND is_generation=1 ORDER BY ordinal",
        )
        .unwrap()
        .query_map([&run_id], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    if generations.is_empty() {
        let stderr: String = db
            .query_row(
                "SELECT COALESCE(group_concat(content, char(10)), '') FROM terminal_events WHERE run_id=?1 AND stream='stderr'",
                [&run_id],
                |r| r.get(0),
            )
            .unwrap();
        panic!(
            "live Claude produced no auditable generation; status={status}, code={error_code:?}, observed={observed:?}, stderr={stderr:?}, returned_error={:?}",
            result.as_ref().err().map(ToString::to_string)
        );
    }
    match result {
        Ok(_) => {
            assert_eq!(status, "completed");
            assert!(generations.iter().all(|model| model
                .as_deref()
                .is_some_and(|model| model.to_ascii_lowercase().contains("haiku"))));
        }
        Err(error) => {
            assert!(matches!(error.code(), "model_mismatch" | "model_unverified"), "{error}");
            assert_eq!(status, "failed");
            assert_eq!(error_code.as_deref(), Some(error.code()));
        }
    }
    if status == "completed" {
        assert!(observed.as_deref().is_some_and(|model| model.to_ascii_lowercase().contains("haiku")));
    }
}

/// ADR-010 live qualification for Antigravity's CLI: discovery with quota, the
/// live catalog, a stage run through the hardened launch path, and a write that
/// the private `--gemini_dir` must refuse. Uses the cheapest Gemini effort.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "Requires local Antigravity (agy) authentication and network; sends two synthetic prompts"]
async fn authenticated_antigravity_pipeline_smoke() {
    let harness = harness(Vec::new());
    let discovery = harness.terminals.refresh_discovery().await;
    let antigravity = discovery
        .iter()
        .find(|item| item.backend == CliBackend::Antigravity)
        .unwrap();
    assert_eq!(
        antigravity.state,
        CliAvailabilityState::Available,
        "{antigravity:?}"
    );
    assert!(
        antigravity
            .executable_path
            .as_ref()
            .is_some_and(|path| path.ends_with("agy.exe"))
    );
    assert!(
        !antigravity.quota.is_empty(),
        "the auth probe reports quota"
    );

    let catalog = harness
        .terminals
        .model_catalog(&harness.project_id, CliBackend::Antigravity, true)
        .await
        .unwrap();
    assert_eq!(
        catalog.state,
        CliModelCatalogState::Ready,
        "{:?}",
        catalog.diagnostic
    );
    let flash = catalog
        .models
        .iter()
        .find(|model| model.model_id.as_deref() == Some("gemini-3.8-flash"))
        .expect("the account lists Gemini 3.8 Flash");
    assert!(flash.profiles.iter().any(|profile| profile == "low"));

    let current = harness
        .terminals
        .repository()
        .get_config(&harness.project_id, AgentRole::BackendAgent)
        .unwrap();
    harness
        .terminals
        .update_config(UpdateCliAgentConfigRequest {
            project_id: harness.project_id.clone(),
            agent_role: AgentRole::BackendAgent,
            execution_mode: AgentExecutionMode::CliProcess,
            backend: CliBackend::Antigravity,
            model: Some("gemini-3.8-flash".into()),
            model_profile: Some("low".into()),
            model_source: CliModelSelectionSource::Catalog,
            thinking_level: ThinkingLevel::Default,
            custom_executable: None,
            custom_args: Vec::new(),
            custom_risk_ack: false,
            expected_version: current.version,
        })
        .unwrap();

    let mut probe = request(
        &harness.project_id,
        Instant::now() + Duration::from_secs(180),
    );
    probe.user_prompt =
        "Reply with exactly HVC_ANTIGRAVITY_OK and nothing else. Do not use tools.".into();
    let completion = harness
        .terminals
        .execute_llm_request(&probe, &CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert!(
        completion.raw_output.contains("HVC_ANTIGRAVITY_OK"),
        "{}",
        completion.raw_output
    );
    let db = rusqlite::Connection::open(&harness.database_path).unwrap();
    let (observed, source): (String, String) = db
        .query_row(
            "SELECT o.model_id,o.evidence_source FROM terminal_model_observations o
             WHERE o.run_id=?1 AND o.is_generation=1 ORDER BY o.ordinal LIMIT 1",
            [&completion.terminal_run_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (observed.as_str(), source.as_str()),
        ("gemini-3.8-flash", "cli_init")
    );

    let mut write = request(
        &harness.project_id,
        Instant::now() + Duration::from_secs(180),
    );
    write.user_prompt =
        "Use your file writing tool to create hvc-probe.txt containing the word blocked. \
        Then answer in one sentence whether the write succeeded or was denied."
            .into();
    let completion = harness
        .terminals
        .execute_llm_request(&write, &CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let answer = completion.raw_output.to_ascii_lowercase();
    assert!(
        answer.contains("denied") || answer.contains("deny") || answer.contains("permission"),
        "the deny rule must stop the write: {}",
        completion.raw_output
    );
}

/// The Companion over Antigravity: one synthetic round in a read-only, tool-less directory.
#[tokio::test]
#[ignore = "Requires local Antigravity (agy) authentication and network; sends one synthetic message"]
async fn authenticated_antigravity_companion_smoke() {
    let harness = harness(Vec::new());
    harness.terminals.refresh_discovery().await;
    harness
        .terminals
        .model_catalog(&harness.project_id, CliBackend::Antigravity, true)
        .await
        .unwrap();
    let credentials = Arc::new(NoHvcCredentials::default());
    let chat = Arc::new(ChatService::new(
        &harness.database_path,
        credentials.clone(),
        Arc::new(NoopChatEventSink),
    ));
    chat.attach_terminal_service(harness.terminals.clone());
    let conversation = chat
        .create_conversation(CreateChatConversationRequest {
            project_id: harness.project_id.clone(),
            target: ChatExecutionTargetRequest::Cli {
                backend: CliBackend::Antigravity,
                model: Some("gemini-3.8-flash".into()),
                model_profile: Some("low".into()),
            },
        })
        .unwrap()
        .conversation;
    chat.start_turn(
        SendChatMessageRequest {
            conversation_id: conversation.id.clone(),
            content: "Reply with exactly Ready. Do not call any tools.".into(),
            expected_target: None,
        },
        harness._workspace.path(),
    )
    .unwrap();
    assert!(chat.wait_for_idle(Duration::from_secs(180)).await);
    let loaded = chat
        .get_conversation(GetChatConversationRequest {
            conversation_id: conversation.id.clone(),
        })
        .unwrap();
    let assistant = loaded
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "assistant")
        .unwrap();
    assert_eq!(assistant.status, "completed", "{:?}", assistant.error_message);
    assert!(assistant.content.contains("Ready"), "{}", assistant.content);
    assert_eq!(credentials.0.load(Ordering::SeqCst), 0);
}

/// ADR-010/022 live qualification for Codex's model-less public item events.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "Requires local Codex authentication and network; sends one synthetic model identity probe"]
async fn authenticated_codex_turn_context_model_smoke() {
    let harness = harness(Vec::new());
    harness.terminals.refresh_discovery().await;
    let current = harness
        .terminals
        .repository()
        .get_config(&harness.project_id, AgentRole::BackendAgent)
        .unwrap();
    harness
        .terminals
        .update_config(UpdateCliAgentConfigRequest {
            project_id: harness.project_id.clone(),
            agent_role: AgentRole::BackendAgent,
            execution_mode: AgentExecutionMode::CliProcess,
            backend: CliBackend::Codex,
            model: Some("gpt-5.6-luna".into()),
            model_profile: None,
            model_source: CliModelSelectionSource::Manual,
            thinking_level: ThinkingLevel::Default,
            custom_executable: None,
            custom_args: Vec::new(),
            custom_risk_ack: false,
            expected_version: current.version,
        })
        .unwrap();
    let mut probe = request(&harness.project_id, Instant::now() + Duration::from_secs(180));
    probe.user_prompt = "Reply with exactly HVC_CODEX_MODEL_OK and do not call tools.".into();
    probe.max_output_tokens = 32;
    let result = tokio::time::timeout(
        Duration::from_secs(180),
        harness
            .terminals
            .execute_llm_request(&probe, &CancellationToken::new()),
    )
    .await
    .expect("Codex identity smoke exceeded 180 seconds");
    if let Err(error) = &result {
        panic!("Codex identity smoke failed before audit: {error}");
    }
    let db = rusqlite::Connection::open(&harness.database_path).unwrap();
    let (run_id, selected, observed): (String, Option<String>, Option<String>) = db
        .query_row(
            "SELECT id,model,observed_model FROM terminal_runs WHERE project_id=?1 ORDER BY created_at DESC LIMIT 1",
            [&harness.project_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(selected.as_deref(), Some("gpt-5.6-luna"));
    assert_eq!(observed.as_deref(), Some("gpt-5.6-luna"));
    let evidence: (Option<String>, String) = db
        .query_row(
            "SELECT model_id,evidence_source FROM terminal_model_observations WHERE run_id=?1 AND is_generation=1 ORDER BY ordinal LIMIT 1",
            [&run_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(evidence.0.as_deref(), Some("gpt-5.6-luna"));
    assert_eq!(evidence.1, "codex_turn_context");
    assert!(result.unwrap().unwrap().raw_output.contains("HVC_CODEX_MODEL_OK"));
}

#[tokio::test]
async fn opencode_catalog_keeps_the_last_valid_snapshot_after_refresh_failure() {
    let _path_lock = PATH_TEST_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let binaries = tempfile::tempdir().unwrap();
    let fixture_executable = binaries.path().join("opencode.exe");
    std::fs::copy(env!("CARGO_BIN_EXE_cli_fixture"), &fixture_executable).unwrap();
    let original_path = std::env::var_os("PATH");
    let _path_restore = PathRestore(original_path.clone());
    let mut paths = vec![binaries.path().to_path_buf()];
    if let Some(original_path) = original_path {
        paths.extend(std::env::split_paths(&original_path));
    }
    // SAFETY: PATH_TEST_LOCK serializes the mutation and PathRestore restores the variable.
    unsafe { std::env::set_var("PATH", std::env::join_paths(paths).unwrap()) };

    let harness = harness(Vec::new());
    harness.terminals.refresh_discovery().await;
    let ready = harness
        .terminals
        .model_catalog(&harness.project_id, CliBackend::Opencode, true)
        .await
        .unwrap();
    assert_eq!(ready.state, CliModelCatalogState::Ready);
    assert_eq!(ready.models.len(), 4, "three models plus CLI default");

    std::fs::remove_file(fixture_executable).unwrap();
    let stale = harness
        .terminals
        .model_catalog(&harness.project_id, CliBackend::Opencode, true)
        .await
        .unwrap();
    assert_eq!(stale.state, CliModelCatalogState::Stale);
    assert_eq!(stale.models, ready.models);
    assert!(stale.diagnostic.is_some());
}

#[tokio::test]
async fn companion_cli_uses_existing_cli_auth_without_hvc_credentials_and_persists_only_text() {
    let _path_lock = PATH_TEST_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let binaries = tempfile::tempdir().unwrap();
    std::fs::copy(
        env!("CARGO_BIN_EXE_cli_fixture"),
        binaries.path().join("claude.exe"),
    )
    .unwrap();
    std::fs::copy(
        env!("CARGO_BIN_EXE_cli_fixture"),
        binaries.path().join("codex.exe"),
    )
    .unwrap();
    let original_path = std::env::var_os("PATH");
    let _restore = PathRestore(original_path.clone());
    let mut paths = vec![binaries.path().to_path_buf()];
    if let Some(original_path) = original_path {
        paths.extend(std::env::split_paths(&original_path));
    }
    // SAFETY: PATH_TEST_LOCK serializes this mutation and PathRestore restores it.
    unsafe { std::env::set_var("PATH", std::env::join_paths(paths).unwrap()) };

    for backend in [CliBackend::Codex, CliBackend::Claude] {
        let harness = harness(Vec::new());
        let credentials = Arc::new(NoHvcCredentials::default());
        let chat = Arc::new(ChatService::new(
            &harness.database_path,
            credentials.clone(),
            Arc::new(NoopChatEventSink),
        ));
        chat.attach_terminal_service(harness.terminals.clone());
        let conversation = chat
            .create_conversation(CreateChatConversationRequest {
                project_id: harness.project_id.clone(),
                target: ChatExecutionTargetRequest::Cli {
                    backend,
                    model: None,
                    model_profile: None,
                },
            })
            .unwrap()
            .conversation;
        chat.start_turn(
            SendChatMessageRequest {
                conversation_id: conversation.id.clone(),
                content: "Recommend a model using only local evidence.".into(),
                expected_target: None,
            },
            harness._workspace.path(),
        )
        .unwrap();
        assert!(chat.wait_for_idle(Duration::from_secs(30)).await);
        let loaded = chat
            .get_conversation(GetChatConversationRequest {
                conversation_id: conversation.id.clone(),
            })
            .unwrap();
        let assistant = loaded
            .messages
            .iter()
            .find(|message| message.role == "assistant")
            .unwrap();
        assert_eq!(assistant.status, "completed");
        assert_eq!(assistant.content, "Companion fixture response");
        assert_eq!(assistant.input_tokens, Some(20));
        assert_eq!(assistant.output_tokens, Some(5));
        let usage =
            hvc_core::usage::query_usage(&harness.database_path, &Default::default()).unwrap();
        assert_eq!(usage.totals.total_tokens, Some(25));
        assert_eq!(usage.totals.records, 1);
        assert!(!assistant.content.contains("control_envelope"));
        assert!(!assistant.content.contains("item.completed"));
        assert!(loaded.tool_calls.is_empty());
        assert_eq!(credentials.0.load(Ordering::SeqCst), 0);
        if backend == CliBackend::Codex {
            chat.start_turn(
                SendChatMessageRequest {
                    conversation_id: conversation.id.clone(),
                    content: "companion-error-fixture".into(),
                    expected_target: None,
                },
                harness._workspace.path(),
            )
            .unwrap();
            assert!(chat.wait_for_idle(Duration::from_secs(30)).await);
            let failed = chat
                .get_conversation(GetChatConversationRequest {
                    conversation_id: conversation.id,
                })
                .unwrap();
            let assistant = failed
                .messages
                .iter()
                .rev()
                .find(|m| m.role == "assistant")
                .unwrap();
            assert_eq!(assistant.status, "failed");
            let error = assistant.error_message.as_deref().unwrap();
            assert!(error.contains("not supported"));
            assert!(error.contains("create a new conversation"));
            assert!(!error.contains("turn.failed"));
            assert!(!error.contains("Some(1)"));
        }
    }
}

#[tokio::test]
#[ignore = "Requires local Codex authentication and network; sends only synthetic messages"]
async fn authenticated_codex_companion_smoke() {
    let harness = harness(Vec::new());
    let credentials = Arc::new(NoHvcCredentials::default());
    let chat = Arc::new(ChatService::new(
        &harness.database_path,
        credentials.clone(),
        Arc::new(NoopChatEventSink),
    ));
    chat.attach_terminal_service(harness.terminals.clone());
    let conversation = chat
        .create_conversation(CreateChatConversationRequest {
            project_id: harness.project_id.clone(),
            target: ChatExecutionTargetRequest::Cli {
                backend: CliBackend::Codex,
                model: None,
                model_profile: None,
            },
        })
        .unwrap()
        .conversation;
    for content in [
        "Reply with exactly Ready. Do not call any tools.",
        "Use the HVC list_repository_tree tool to list src, then tell me which file it contains. Do not use native tools.",
    ] {
        chat.start_turn(
            SendChatMessageRequest {
                conversation_id: conversation.id.clone(),
                content: content.into(),
                expected_target: None,
            },
            harness._workspace.path(),
        )
        .unwrap();
        assert!(chat.wait_for_idle(Duration::from_secs(150)).await);
        let loaded = chat
            .get_conversation(GetChatConversationRequest {
                conversation_id: conversation.id.clone(),
            })
            .unwrap();
        let assistant = loaded
            .messages
            .iter()
            .rev()
            .find(|m| m.role == "assistant")
            .unwrap();
        assert_eq!(
            assistant.status, "completed",
            "{:?}",
            assistant.error_message
        );
        assert!(!assistant.content.is_empty());
        assert!(!assistant.content.contains("control_envelope"));
        if content.starts_with("Use") {
            assert!(
                loaded
                    .tool_calls
                    .iter()
                    .any(|call| call.tool_name == "list_repository_tree")
            );
            assert!(assistant.content.contains("lib.rs"));
        }
    }
    assert_eq!(credentials.0.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn real_executor_streams_fragmented_utf8_and_persists_terminal_lifecycle() {
    let harness = harness(vec![
        "--mode".into(),
        "stream".into(),
        "--expect-stdin".into(),
        "payload-from-stdin".into(),
    ]);
    let completion = harness
        .terminals
        .execute_llm_request(
            &request(&harness.project_id, Instant::now() + Duration::from_secs(5)),
            &CancellationToken::new(),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(completion.raw_output.contains("final action"));
    let run = harness
        .terminals
        .repository()
        .get_run(&completion.terminal_run_id)
        .unwrap();
    assert_eq!(run.status, TerminalRunStatus::Completed);
    assert!(run.workspace_snapshot_hash.is_some());
    assert!(run.launch_identity_sha256.is_some());
    assert!(run.event_count >= 4);
    let envelopes = harness
        .sink
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert!(
        envelopes
            .windows(2)
            .all(|pair| pair[0].sequence < pair[1].sequence)
    );
    assert!(envelopes.iter().all(|event| event.protocol_version == 2));
    assert_eq!(
        envelopes.last().unwrap().run.status,
        TerminalRunStatus::Completed
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn manual_cancel_before_spawn_finishes_as_cancelled_without_fallback() {
    let harness = harness(vec!["--mode".into(), "hang".into()]);
    let run = harness
        .terminals
        .start_manual(StartManualRunRequest {
            project_id: harness.project_id.clone(),
            agent_role: AgentRole::BackendAgent,
            prompt: "cancel fixture".into(),
        })
        .unwrap();
    assert!(harness.terminals.cancel_run(&run.id));
    wait_for_status(&harness, &run.id, TerminalRunStatus::Cancelled).await;
    let persisted = harness.terminals.repository().get_run(&run.id).unwrap();
    assert_eq!(
        persisted.termination_reason.as_deref(),
        Some("user_cancelled")
    );
}

#[cfg(windows)]
#[tokio::test(flavor = "multi_thread")]
async fn deadline_kills_the_process_tree_and_stops_descendant_heartbeat() {
    let heartbeat_dir = tempfile::tempdir().unwrap();
    let heartbeat = heartbeat_dir.path().join("heartbeat.log");
    let harness = harness(vec![
        "--mode".into(),
        "descendant".into(),
        "--heartbeat".into(),
        heartbeat.to_string_lossy().into_owned(),
    ]);
    let error = harness
        .terminals
        .execute_llm_request(
            &request(
                &harness.project_id,
                Instant::now() + Duration::from_secs(5),
            ),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), "cli_timeout");
    tokio::time::sleep(Duration::from_millis(200)).await;
    let before = std::fs::metadata(&heartbeat).unwrap().len();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let after = std::fs::metadata(&heartbeat).unwrap().len();
    assert_eq!(
        before, after,
        "descendant kept running after Job Object termination"
    );
    let run = harness
        .terminals
        .repository()
        .list_runs(&harness.project_id, AgentRole::BackendAgent, 1)
        .unwrap()
        .remove(0);
    assert_eq!(run.status, TerminalRunStatus::Failed);
    assert_eq!(run.error_code.as_deref(), Some("cli_timeout"));
    assert_eq!(run.termination_reason.as_deref(), Some("timeout"));
}

#[tokio::test]
async fn feature_discovery_uses_the_official_cli_adapters_with_isolated_context_and_own_identity()
{
    let _lock = PATH_TEST_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let binaries = tempfile::tempdir().unwrap();
    for name in ["codex", "claude", "gemini", "opencode", "agy"] {
        std::fs::copy(
            env!("CARGO_BIN_EXE_cli_fixture"),
            binaries.path().join(format!("{name}.exe")),
        )
        .unwrap();
    }
    let original = std::env::var_os("PATH");
    let _restore = PathRestore(original.clone());
    let mut paths = vec![binaries.path().to_path_buf()];
    if let Some(original) = original {
        paths.extend(std::env::split_paths(&original));
    }
    // SAFETY: the PATH test lock serializes fixture discovery and restores the original value.
    unsafe {
        std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
    }
    let h = harness(vec![]);
    for backend in [
        CliBackend::Codex,
        CliBackend::Claude,
        CliBackend::Gemini,
        CliBackend::Opencode,
        CliBackend::Antigravity,
    ] {
        let model = match backend {
            CliBackend::Codex => "gpt-5.6-luna",
            CliBackend::Claude => "haiku",
            CliBackend::Gemini => "gemini-2.5-flash",
            CliBackend::Opencode => "fixture-opencode-model",
            CliBackend::Antigravity => "gemini-3.8-flash",
            CliBackend::Custom => unreachable!(),
        };
        let config = h
            .terminals
            .repository()
            .get_config(&h.project_id, AgentRole::FeatureAgent)
            .unwrap();
        h.terminals
            .update_config(UpdateCliAgentConfigRequest {
                project_id: h.project_id.clone(),
                agent_role: AgentRole::FeatureAgent,
                execution_mode: AgentExecutionMode::CliProcess,
                backend,
                model: Some(model.into()),
                model_profile: None,
                model_source: CliModelSelectionSource::Manual,
                thinking_level: ThinkingLevel::Default,
                custom_executable: None,
                custom_args: vec![],
                custom_risk_ack: false,
                expected_version: config.version,
            })
            .unwrap();
        let mut r = request(&h.project_id, Instant::now() + Duration::from_secs(20));
        r.target = AgentExecutionTarget::canonical(AgentRole::FeatureAgent);
        r.stage = "Feature".into();
        r.job_chain = "feature:fixture".into();
        r.user_prompt = "feature-discovery-fixture".into();
        let completion = h
            .terminals
            .execute_llm_request(&r, &CancellationToken::new())
            .await
            .unwrap()
            .unwrap();
        let run = h
            .terminals
            .repository()
            .get_run(&completion.terminal_run_id)
            .unwrap();
        assert_eq!(run.origin, hvc_core::terminals::ExecutionOrigin::Feature);
        assert_eq!(run.agent_role, Some(AgentRole::FeatureAgent));
        assert!(run.pulsation_id.is_none());
        assert_eq!(run.status, TerminalRunStatus::Completed);
    }
}
