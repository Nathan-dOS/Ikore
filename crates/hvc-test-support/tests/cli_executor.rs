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
use hvc_core::llm::{AgentRole, LlmRequest, ResponseFormat};
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
            model: None,
            model_profile: None,
            model_source: CliModelSelectionSource::Default,
            thinking_level: ThinkingLevel::Default,
            custom_executable: Some(env!("CARGO_BIN_EXE_cli_fixture").into()),
            custom_args,
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
            model: None,
            model_profile: None,
            model_source: CliModelSelectionSource::Default,
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
        job_chain: "fixture".into(),
        agent_role: AgentRole::BackendAgent,
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
    }
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
async fn four_official_adapters_capture_and_resume_the_exact_native_session() {
    let _path_lock = PATH_TEST_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let binaries = tempfile::tempdir().unwrap();
    for backend in ["codex", "claude", "gemini", "opencode"] {
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
    ] {
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
                model: None,
                model_profile: None,
                model_source: CliModelSelectionSource::Default,
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
    assert_eq!(usage.totals.total_tokens, Some(200));
    assert_eq!(usage.totals.records, 8);
    assert_eq!(usage.totals.unavailable_records, 0);
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
                Instant::now() + Duration::from_millis(1_200),
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
async fn feature_discovery_uses_four_official_cli_adapters_with_isolated_context_and_own_identity()
{
    let _lock = PATH_TEST_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let binaries = tempfile::tempdir().unwrap();
    for name in ["codex", "claude", "gemini", "opencode"] {
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
    ] {
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
                model: None,
                model_profile: None,
                model_source: CliModelSelectionSource::Default,
                thinking_level: ThinkingLevel::Default,
                custom_executable: None,
                custom_args: vec![],
                custom_risk_ack: false,
                expected_version: config.version,
            })
            .unwrap();
        let mut r = request(&h.project_id, Instant::now() + Duration::from_secs(20));
        r.agent_role = AgentRole::FeatureAgent;
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
