use crate::test_support::{fixture_proof, host};

fn test_repository(path: impl Into<PathBuf>) -> TerminalRepository {
    TerminalRepository::new(path, host())
}
pub(super) fn test_service(
    path: impl Into<PathBuf>,
    runtime: Handle,
    sink: Arc<dyn TerminalEventSink>,
) -> Result<TerminalService, TerminalError> {
    TerminalService::new(path, runtime, sink, host())
}

struct FixtureEventBus {
    sender: tokio::sync::broadcast::Sender<TerminalEventEnvelope>,
}
impl FixtureEventBus {
    fn new(capacity: usize) -> Self {
        Self {
            sender: tokio::sync::broadcast::channel(capacity).0,
        }
    }
    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<TerminalEventEnvelope> {
        self.sender.subscribe()
    }
}
impl TerminalEventSink for FixtureEventBus {
    fn emit(&self, event: &TerminalEventEnvelope) {
        let _ = self.sender.send(event.clone());
    }
}

use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

/// A project whose `notes_scribe` route was never created, with the
/// Feature Agent on the given backend.
fn scribe_route_fixture(
    feature_backend: &str,
) -> (
    tempfile::TempDir,
    tempfile::TempDir,
    TerminalRepository,
    String,
) {
    use crate::test_support::TestDatabase;

    let data = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let persistence = TestDatabase::initialize(data.path()).unwrap();
    let project = persistence
        .insert_project(
            workspace.path().to_string_lossy().into_owned(),
            Some("Scribe route".into()),
        )
        .unwrap();
    let repository = test_repository(persistence.database_path());
    repository
        .ensure_project_configs(&project.id, false)
        .unwrap();
    let db = repository.connection().unwrap();
    db.execute(
        "DELETE FROM cli_agent_configs WHERE project_id=?1 AND agent_role='notes_scribe'",
        [&project.id],
    )
    .unwrap();
    db.execute(
        "UPDATE cli_agent_configs SET execution_mode='cli_process',backend=?2,model='sonnet',
                thinking_level='high',model_source='manual'
         WHERE project_id=?1 AND agent_role='feature_agent'",
        params![project.id, feature_backend],
    )
    .unwrap();
    (data, workspace, repository, project.id)
}

#[test]
fn notes_scribe_route_starts_from_the_feature_agent_route() {
    let (_data, _workspace, repository, project) = scribe_route_fixture("claude");
    repository.ensure_project_configs(&project, false).unwrap();
    let scribe = repository
        .get_config(&project, AgentRole::NotesScribe)
        .unwrap();
    assert_eq!(scribe.execution_mode, AgentExecutionMode::CliProcess);
    assert_eq!(scribe.backend, CliBackend::Claude);
    assert_eq!(scribe.model.as_deref(), Some("sonnet"));
    assert_eq!(scribe.thinking_level, ThinkingLevel::High);
    assert_eq!(scribe.model_source, CliModelSelectionSource::Manual);
    assert!(scribe.custom_executable.is_none() && !scribe.custom_risk_ack);
}

#[test]
fn notes_scribe_route_does_not_inherit_a_custom_feature_agent_route() {
    let (_data, _workspace, repository, project) = scribe_route_fixture("custom");
    repository.ensure_project_configs(&project, false).unwrap();
    let scribe = repository
        .get_config(&project, AgentRole::NotesScribe)
        .unwrap();
    assert_eq!(scribe.execution_mode, AgentExecutionMode::ApiDirect);
    assert_eq!(scribe.backend, CliBackend::Codex);
    assert!(scribe.model.is_none());
}

/// ADR-034 M5: the Documentation Reconciler starts from the Feature Agent's
/// route, as the Notes Scribe does, and never inherits a Custom one.
#[test]
fn the_documentation_reconciler_route_follows_the_feature_agent_but_never_custom() {
    for (backend, inherited) in [("claude", true), ("custom", false)] {
        let (_data, _workspace, repository, project) = scribe_route_fixture(backend);
        repository
            .connection()
            .unwrap()
            .execute(
                "DELETE FROM cli_agent_configs WHERE project_id=?1 AND agent_role='documentation_reconciler'",
                [&project],
            )
            .unwrap();
        repository.ensure_project_configs(&project, false).unwrap();
        let route = repository
            .get_config(&project, AgentRole::DocumentationReconciler)
            .unwrap();
        if inherited {
            assert_eq!(route.execution_mode, AgentExecutionMode::CliProcess);
            assert_eq!(route.backend, CliBackend::Claude);
            assert_eq!(route.model.as_deref(), Some("sonnet"));
        } else {
            assert_eq!(route.execution_mode, AgentExecutionMode::ApiDirect);
            assert_eq!(route.backend, CliBackend::Codex);
            assert!(route.model.is_none());
        }
        assert!(route.custom_executable.is_none() && !route.custom_risk_ack);
    }
}

#[test]
fn schema_suffix_keeps_artifacts_english_except_for_the_notes_scribe() {
    for role in AgentRole::ALL {
        let suffix = schema_prompt_suffix(role);
        assert!(suffix.contains("Return only valid JSON"));
        assert_eq!(
            suffix.contains("must be English"),
            role != AgentRole::NotesScribe,
            "{role:?}"
        );
    }
}

#[tokio::test]
async fn only_gemini_gates_an_unvalidated_newer_version_and_only_until_validated() {
    use crate::test_support::TestDatabase;

    let data = tempfile::tempdir().unwrap();
    let persistence = TestDatabase::initialize(data.path()).unwrap();
    let sink: Arc<dyn TerminalEventSink> = Arc::new(CollectTerminalEvents::default());
    let service = test_service(
        persistence.database_path(),
        tokio::runtime::Handle::current(),
        sink,
    )
    .unwrap();
    let seeded = |backend: CliBackend, version: &str| {
        let (install_url, install_command) = install_guidance(backend);
        CliAvailability {
            backend,
            state: CliAvailabilityState::Available,
            executable_path: Some(std::path::PathBuf::from("fixture.exe")),
            version: Some(version.to_string()),
            diagnostic: None,
            install_url,
            install_command,
            capabilities: adapters::capabilities(backend, Some(version)),
            quota: Vec::new(),
        }
    };
    *service
        .discovery_cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(CliDiscoveryCache {
        discovered_at: Instant::now(),
        items: vec![
            seeded(CliBackend::Gemini, "99.0.0"),
            seeded(CliBackend::Codex, "99.0.0"),
        ],
    });

    let gate = service
        .unvalidated_newer_version(CliBackend::Gemini)
        .await
        .unwrap()
        .expect("an unvalidated newer Gemini gates");
    assert_eq!(gate.version, "99.0.0");
    assert!(gate.diagnostic().contains("99.0.0"));
    assert!(gate.diagnostic().contains(&gate.minimum));
    // Codex is newer too but does not gate: it runs with the advisory only.
    assert!(
        service
            .unvalidated_newer_version(CliBackend::Codex)
            .await
            .unwrap()
            .is_none()
    );
    // Validating another version does not cover this one; validating this one does.
    service
        .repository()
        .record_validated_cli_version(CliBackend::Gemini, "98.0.0")
        .unwrap();
    assert!(
        service
            .unvalidated_newer_version(CliBackend::Gemini)
            .await
            .unwrap()
            .is_some()
    );
    service
        .repository()
        .record_validated_cli_version(CliBackend::Gemini, "99.0.0")
        .unwrap();
    assert!(
        service
            .unvalidated_newer_version(CliBackend::Gemini)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        service
            .repository()
            .validated_cli_version(CliBackend::Gemini)
            .unwrap()
            .as_deref(),
        Some("99.0.0")
    );
}

#[test]
fn newer_cli_versions_stay_available_with_an_advisory_and_older_ones_are_blocked() {
    fn probed(backend: CliBackend, version: &str) -> CliAvailability {
        let (install_url, install_command) = install_guidance(backend);
        CliAvailability {
            backend,
            state: CliAvailabilityState::Available,
            executable_path: Some(std::path::PathBuf::from("gemini.cmd")),
            version: Some(version.to_string()),
            diagnostic: None,
            install_url,
            install_command,
            capabilities: adapters::capabilities(backend, None),
            quota: Vec::new(),
        }
    }

    // Above the validated ceiling: usable, full contract, advisory note.
    let mut newer = probed(CliBackend::Gemini, "0.60.0");
    assert!(apply_version_verdict(&mut newer));
    assert_eq!(newer.state, CliAvailabilityState::Available);
    assert!(newer.capabilities.native_resume);
    let note = newer.diagnostic.expect("advisory note");
    assert!(note.contains("0.60.0") && note.contains("0.55.0"), "{note}");

    // Inside the validated range: usable, no note.
    let mut current = probed(CliBackend::Gemini, "0.59.5");
    assert!(apply_version_verdict(&mut current));
    assert_eq!(current.state, CliAvailabilityState::Available);
    assert!(current.diagnostic.is_none());

    // Below the minimum: blocked.
    let mut older = probed(CliBackend::Gemini, "0.54.9");
    assert!(!apply_version_verdict(&mut older));
    assert_eq!(older.state, CliAvailabilityState::UnsupportedVersion);
    assert!(!older.capabilities.native_resume);
}

#[derive(Default)]
struct CollectTerminalEvents {
    envelopes: Mutex<Vec<TerminalEventEnvelope>>,
}

impl TerminalEventSink for CollectTerminalEvents {
    fn emit(&self, envelope: &TerminalEventEnvelope) {
        self.envelopes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(envelope.clone());
    }
}

#[test]
fn canonical_catalog_covers_every_role_once() {
    let roles = CANONICAL_AGENTS
        .iter()
        .map(|item| item.role)
        .collect::<HashSet<_>>();
    assert_eq!(roles.len(), AgentRole::ALL.len());
    assert!(AgentRole::ALL.into_iter().all(|role| roles.contains(&role)));
}

#[test]
fn codex_process_is_non_interactive_read_only_and_uses_stdin() {
    let spec = build_process_spec(
        host().as_ref(),
        CliInvocationRequest {
            executable: PathBuf::from(r"C:\Tools\codex.exe"),
            backend: CliBackend::Codex,
            model: Some("gpt-test".into()),
            model_profile: None,
            thinking_level: ThinkingLevel::High,
            prompt: "segredo do prompt".into(),
            schema_file: Some(PathBuf::from(r"C:\Temp\schema.json")),
            schema: None,
            custom_args: vec![],
            resume_session_id: None,
        },
    )
    .unwrap();
    assert_eq!(spec.executable, PathBuf::from(r"C:\Tools\codex.exe"));
    assert!(
        spec.args
            .windows(2)
            .any(|pair| pair == ["--sandbox", "read-only"])
    );
    assert!(spec.args.windows(2).any(|pair| pair == ["-a", "never"]));
    assert!(
        !spec
            .args
            .iter()
            .any(|arg| arg.contains("segredo do prompt"))
    );
    assert_eq!(spec.stdin_payload, b"segredo do prompt");
}

#[test]
fn gemini_and_opencode_processes_enforce_read_only_modes() {
    let gemini = build_process_spec(
        host().as_ref(),
        CliInvocationRequest {
            executable: PathBuf::from(r"C:\Tools\gemini.exe"),
            backend: CliBackend::Gemini,
            model: None,
            model_profile: None,
            thinking_level: ThinkingLevel::Default,
            prompt: "inspect".into(),
            schema_file: None,
            schema: None,
            custom_args: vec![],
            resume_session_id: None,
        },
    )
    .unwrap();
    assert!(
        gemini
            .args
            .windows(2)
            .any(|pair| pair == ["--approval-mode", "plan"])
    );

    let opencode = build_process_spec(
        host().as_ref(),
        CliInvocationRequest {
            executable: PathBuf::from(r"C:\Tools\opencode.exe"),
            backend: CliBackend::Opencode,
            model: None,
            model_profile: None,
            thinking_level: ThinkingLevel::Default,
            prompt: "inspect".into(),
            schema_file: None,
            schema: None,
            custom_args: vec![],
            resume_session_id: None,
        },
    )
    .unwrap();
    assert!(opencode.args.iter().any(|arg| arg == "--pure"));
    let permissions = opencode
        .environment
        .iter()
        .find(|(key, _)| key == "OPENCODE_PERMISSION")
        .map(|(_, value)| value)
        .unwrap();
    assert!(!permissions.contains(r#""edit""#));
    assert!(permissions.contains(r#""*":"deny""#));
    assert!(permissions.contains(r#""read":"allow""#));
}

#[test]
fn codex_companion_reads_nested_stdout_errors_without_exposing_other_events() {
    let error = serde_json::json!({"type":"error","status":400,"error":{
        "type":"invalid_request_error",
        "message":"The 'gpt-5.2-codex' model is not supported when using Codex with a ChatGPT account."
    }});
    let output = format!(
        "{}\n{}\n{}\n",
        serde_json::json!({"type":"item.completed","item":{"type":"reasoning","text":"private trace"}}),
        serde_json::json!({"type":"turn.failed","error":{"message":error.to_string()}}),
        serde_json::json!({"type":"item.completed","item":{"type":"agent_message","text":"internal control stream"}}),
    );
    let diagnostic = codex_companion_error(&output).unwrap();
    assert!(diagnostic.contains("gpt-5.2-codex"));
    assert!(diagnostic.contains("create a new conversation"));
    assert!(!diagnostic.contains("private trace"));
    assert!(!diagnostic.contains("internal control stream"));
    assert!(!diagnostic.contains("invalid_request_error"));
    assert!(
        codex_companion_error(
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"not an error"}}"#
        )
        .is_none()
    );
    // Claude: the API error lives in the final `result` event; stderr is empty.
    let claude = concat!(
        r#"{"type":"system","subtype":"init","model":"claude-haiku-4-5"}"#,
        "\n",
        r#"{"type":"result","subtype":"success","is_error":true,"api_error_status":400,"result":"API Error: 400 tools.0.custom.input_schema.type: Field required"}"#,
        "\n"
    );
    assert_eq!(
        claude_companion_error(claude).as_deref(),
        Some("API Error: 400 tools.0.custom.input_schema.type: Field required")
    );
    assert!(
        claude_companion_error(r#"{"type":"result","is_error":false,"result":"ok"}"#).is_none()
    );
    let wrapped = serde_json::json!({"type":"item.completed","item":{"type":"agent_message","text":
        serde_json::json!({"response":{"type":"final","text":"Ready"}}).to_string()
    }})
    .to_string();
    assert_eq!(
        serde_json::from_str::<Value>(&extract_companion_envelope_json(&wrapped).unwrap()).unwrap(),
        serde_json::json!({"type":"final","text":"Ready"})
    );
}

#[test]
fn companion_extracts_canvas_offer_control_without_treating_it_as_authority() {
    let answer = serde_json::json!({
        "type": "final_with_canvas_offer",
        "text": "The UI calls the domain service.",
        "offer": {
            "profile": "architecture",
            "language": "en",
            "title": "System architecture",
            "request_text": "May I illustrate this explanation on Canvas?"
        }
    });
    for wrapped in [
        answer.to_string(),
        serde_json::json!({"response": answer.clone()}).to_string(),
        serde_json::json!({"type":"result", "result":answer.to_string()}).to_string(),
    ] {
        let extracted = extract_companion_envelope_json(&wrapped).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&extracted).unwrap(), answer);
    }
    assert!(
        extract_companion_envelope_json(r#"{"type":"approve_canvas","approved":true}"#).is_none()
    );
    assert!(
        extract_companion_envelope_json(r#"{"type":"canvas_finish","drawing_id":"arbitrary"}"#)
            .is_none()
    );
}

/// Mirrors a real `claude -p --include-partial-messages --json-schema` run
/// (2026-09-26): a long answer streamed as ~750 delta frames holding about
/// 2,400 braces before the whole `assistant` and `result` events. The
/// bounded brace scan gave up long before them.
#[test]
fn companion_envelope_survives_a_long_claude_partial_message_stream() {
    let text = "Line with an object { \"key\": 1 } in the answer.\n".repeat(120);
    let envelope = serde_json::json!({"response":{"type":"final","text":text}});
    let mut stream = vec![
        serde_json::json!({"type":"system","subtype":"init","tools":["Read","StructuredOutput"]})
            .to_string(),
    ];
    let json = envelope.to_string();
    let chars = json.chars().collect::<Vec<_>>();
    for chunk in chars.chunks(12) {
        stream.push(
            serde_json::json!({"type":"stream_event","event":{"type":"content_block_delta",
                "index":1,"delta":{"type":"input_json_delta",
                "partial_json":chunk.iter().collect::<String>()}},"parent_tool_use_id":null})
            .to_string(),
        );
    }
    stream.push(
        serde_json::json!({"type":"assistant","message":{"content":[{"type":"tool_use",
            "name":"StructuredOutput","input":envelope.clone()}]}})
        .to_string(),
    );
    stream.push(
        serde_json::json!({"type":"result","subtype":"success","is_error":false,
            "result":json,"structured_output":envelope})
        .to_string(),
    );
    let stdout = stream.join("\n");
    assert!(stdout.matches('{').count() > 1_000);
    let found = extract_companion_envelope_json(&stdout).expect("the final envelope");
    assert_eq!(
        serde_json::from_str::<Value>(&found).unwrap(),
        serde_json::json!({"type":"final","text":text})
    );
    // Deltas alone never make an envelope, however many there are.
    let deltas_only = stream[..stream.len() - 2].join("\n");
    assert!(extract_companion_envelope_json(&deltas_only).is_none());
}

#[test]
fn companion_recovers_only_complete_structured_output_streams() {
    let envelope = serde_json::json!({"response":{"type":"final","text":"Ready"}});
    let start = |name: &str| {
        serde_json::json!({"type":"stream_event","event":{
            "type":"content_block_start","index":0,
            "content_block":{"type":"tool_use","name":name,"input":{}}
        }})
        .to_string()
    };
    let frames =
        |name: &str, input: &str| {
            let mut lines = vec![start(name)];
            for chunk in input.as_bytes().chunks(7) {
                lines.push(serde_json::json!({"type":"stream_event","event":{
                "type":"content_block_delta","index":0,"delta":{
                    "type":"input_json_delta","partial_json":std::str::from_utf8(chunk).unwrap()
                }
            }}).to_string());
            }
            lines
        };
    let lines = frames("StructuredOutput", &envelope.to_string());
    let recovered = extract_companion_envelope_json(&lines.join("\n")).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&recovered).unwrap(),
        serde_json::json!({"type":"final","text":"Ready"})
    );
    assert!(
        extract_companion_envelope_json(&frames("Read", &envelope.to_string()).join("\n"))
            .is_none()
    );
    assert!(
        extract_companion_envelope_json(&frames("StructuredOutput", "{\"response\":").join("\n"))
            .is_none()
    );
    assert!(
        extract_companion_envelope_json(
            &frames(
                "StructuredOutput",
                "{\"response\":{\"type\":\"final\",\"text\":\"Ready\"}"
            )
            .join("\n")
        )
        .is_none()
    );
    let malformed_single_delta = serde_json::json!({"type":"stream_event","event":{
        "type":"content_block_delta","index":0,"delta":{"type":"input_json_delta",
        "partial_json":"{\"response\":{\"type\":\"final\",\"text\":\"Ready\"}"
    }}})
    .to_string();
    assert!(extract_companion_envelope_json(&malformed_single_delta).is_none());
    assert_eq!(
        companion_envelope_miss(CliBackend::Claude, &lines.join("\n")),
        "no_control_envelope"
    );
}

/// The rounds that failed on 2026-09-26: the model called the host's
/// operations as native tools and never reached `StructuredOutput`.
#[test]
fn a_missing_envelope_names_why_it_is_missing() {
    let stream = |events: &[Value]| {
        events
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    };
    let unavailable = stream(&[
        serde_json::json!({"type":"assistant","message":{"content":[{"type":"tool_use","name":"read_repository_file","input":{"path":"index.html"}}]}}),
        serde_json::json!({"type":"user","message":{"content":[{"type":"tool_result","content":"<tool_use_error>Error: No such tool available: read_repository_file</tool_use_error>"}]}}),
        serde_json::json!({"type":"result","subtype":"success","result":""}),
    ]);
    assert!(extract_companion_envelope_json(&unavailable).is_none());
    assert_eq!(
        companion_envelope_miss(CliBackend::Claude, &unavailable),
        "unavailable_tool"
    );
    let rejected = stream(&[
        serde_json::json!({"type":"user","message":{"content":[{"type":"tool_result","content":"InputValidationError: StructuredOutput was called with input that could not be parsed as JSON."}]}}),
    ]);
    assert_eq!(
        companion_envelope_miss(CliBackend::Claude, &rejected),
        "schema_rejected"
    );
    let prose = stream(&[
        serde_json::json!({"type":"assistant","message":{"content":[{"type":"text","text":"Everything is ready."}]}}),
    ]);
    assert_eq!(
        companion_envelope_miss(CliBackend::Claude, &prose),
        "text_only"
    );
    assert_eq!(companion_envelope_miss(CliBackend::Claude, ""), "no_output");
    assert_eq!(
        companion_envelope_miss(CliBackend::Claude, r#"{"type":"result","result":""}"#),
        "no_control_envelope"
    );
    assert_eq!(
        companion_envelope_miss(CliBackend::Codex, &prose),
        "no_envelope"
    );
}

fn spec_for(backend: CliBackend) -> CliProcessSpec {
    let executable = match backend {
        CliBackend::Codex => r"C:\Tools\codex.exe",
        CliBackend::Claude => r"C:\Tools\claude.exe",
        CliBackend::Gemini => r"C:\Tools\gemini.exe",
        CliBackend::Opencode => r"C:\Tools\opencode.exe",
        CliBackend::Antigravity => r"C:\Tools\agy.exe",
        CliBackend::Custom => r"C:\Tools\custom.exe",
    };
    build_process_spec(
        host().as_ref(),
        CliInvocationRequest {
            executable: PathBuf::from(executable),
            backend,
            model: None,
            model_profile: None,
            thinking_level: ThinkingLevel::Default,
            prompt: "Return one stage output".into(),
            schema_file: (backend == CliBackend::Codex)
                .then(|| PathBuf::from(r"C:\Temp\schema.json")),
            schema: (backend == CliBackend::Claude).then(|| serde_json::json!({})),
            custom_args: vec![],
            resume_session_id: None,
        },
    )
    .unwrap()
}

/// A stage agent must keep its read tools - it legitimately inspects the
/// workspace - while the developer's own configuration is still isolated.
#[test]
fn pipeline_hardening_isolates_user_configuration_but_keeps_read_tools() {
    let mut claude = spec_for(CliBackend::Claude);
    harden_spec(CliBackend::Claude, Hardening::Pipeline, &mut claude).unwrap();
    let tools = claude.args.iter().position(|arg| arg == "--tools").unwrap();
    assert_eq!(
        claude.args[tools + 1],
        "Read,Glob,Grep",
        "Pipeline must not blank the read tools the way Companion does"
    );
    assert!(claude.args.iter().any(|arg| arg == "--strict-mcp-config"));
    let sources = claude
        .args
        .iter()
        .position(|arg| arg == "--setting-sources")
        .unwrap();
    assert_eq!(claude.args[sources + 1], "");
    assert!(
        claude
            .environment
            .contains(&("CLAUDE_CODE_DISABLE_AUTO_MEMORY".into(), "1".into()))
    );

    let mut codex = spec_for(CliBackend::Codex);
    harden_spec(CliBackend::Codex, Hardening::Pipeline, &mut codex).unwrap();
    let exec = codex.args.iter().position(|arg| arg == "exec").unwrap();
    assert_eq!(codex.args[exec + 1], "--ignore-user-config");
    assert_eq!(codex.args[exec + 2], "--ignore-rules");
    assert!(
        codex.args.iter().any(|arg| arg == "--sandbox"),
        "the adapter's read-only sandbox must survive hardening"
    );
    assert!(
        !codex.args.iter().any(|arg| arg == "--ephemeral"),
        "--ephemeral is a Companion-only concern"
    );

    // OpenCode keeps the adapter's read-only permission set, not the
    // Companion's blanket deny.
    let mut opencode = spec_for(CliBackend::Opencode);
    harden_spec(CliBackend::Opencode, Hardening::Pipeline, &mut opencode).unwrap();
    let (_, permissions) = opencode
        .environment
        .iter()
        .find(|(name, _)| name == "OPENCODE_PERMISSION")
        .unwrap();
    assert!(permissions.contains(r#""read":"allow""#));
    assert_ne!(permissions, r#"{"*":"deny"}"#);
}

#[test]
fn gemini_pipeline_keeps_core_tools_while_companion_removes_them() {
    for (mode, expects_empty_core) in [
        (Hardening::Companion, true),
        (Hardening::HostContextOnly, true),
        (Hardening::Pipeline, false),
    ] {
        let mut spec = spec_for(CliBackend::Gemini);
        let guard = harden_spec(CliBackend::Gemini, mode, &mut spec).unwrap();
        let (_, path) = spec
            .environment
            .iter()
            .find(|(name, _)| name == "GEMINI_CLI_SYSTEM_SETTINGS_PATH")
            .unwrap();
        let settings: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(
            settings["tools"]["core"].is_array(),
            expects_empty_core,
            "{mode:?} disagreed about core tools"
        );
        assert!(settings["mcp"]["allowed"].as_array().unwrap().is_empty());
        assert_eq!(settings["skills"]["enabled"], false);
        drop(guard);
    }
}

#[test]
fn custom_is_marked_read_only_for_pipelines_and_refused_for_the_companion() {
    let mut spec = spec_for(CliBackend::Custom);
    harden_spec(CliBackend::Custom, Hardening::Pipeline, &mut spec).unwrap();
    assert!(
        spec.environment
            .iter()
            .any(|(name, value)| name == "HVC_STAGE_READ_ONLY" && value == "1")
    );

    for mode in [Hardening::Companion, Hardening::HostContextOnly] {
        let mut spec = spec_for(CliBackend::Custom);
        assert!(harden_spec(CliBackend::Custom, mode, &mut spec).is_err());
    }
}

#[test]
fn companion_and_host_context_hardening_disable_native_tools_and_user_configuration() {
    let cases = [
        (CliBackend::Codex, r"C:\Tools\codex.exe"),
        (CliBackend::Claude, r"C:\Tools\claude.exe"),
        (CliBackend::Gemini, r"C:\Tools\gemini.exe"),
        (CliBackend::Opencode, r"C:\Tools\opencode.exe"),
        (CliBackend::Antigravity, r"C:\Tools\agy.exe"),
    ];
    for (backend, executable, mode) in cases.into_iter().flat_map(|(backend, executable)| {
        [Hardening::Companion, Hardening::HostContextOnly]
            .into_iter()
            .map(move |mode| (backend, executable, mode))
    }) {
        let schema_file =
            (backend == CliBackend::Codex).then(|| PathBuf::from(r"C:\Temp\schema.json"));
        let schema = (backend == CliBackend::Claude).then(|| serde_json::json!({}));
        let mut spec = build_process_spec(
            host().as_ref(),
            CliInvocationRequest {
                executable: PathBuf::from(executable),
                backend,
                model: None,
                model_profile: None,
                thinking_level: ThinkingLevel::Default,
                prompt: "Return one control envelope".into(),
                schema_file,
                schema,
                custom_args: vec![],
                resume_session_id: None,
            },
        )
        .unwrap();
        let guard = harden_spec(backend, mode, &mut spec).unwrap();
        match backend {
            CliBackend::Codex => {
                let exec = spec.args.iter().position(|arg| arg == "exec").unwrap();
                assert_eq!(spec.args[exec + 1], "--ignore-user-config");
                assert_eq!(spec.args[exec + 2], "--ignore-rules");
                assert_eq!(
                    spec.args.iter().any(|argument| argument == "--ephemeral"),
                    mode == Hardening::Companion,
                    "Only actual Companion rounds may omit the rollout needed to verify automatic generations"
                );
            }
            CliBackend::Claude => {
                let tools = spec.args.iter().position(|arg| arg == "--tools").unwrap();
                assert_eq!(spec.args[tools + 1], "");
                assert!(spec.args.iter().any(|arg| arg == "--strict-mcp-config"));
                assert!(
                    spec.args
                        .iter()
                        .any(|arg| arg == "--no-session-persistence")
                );
                assert!(
                    spec.args
                        .iter()
                        .any(|arg| arg == "--disable-slash-commands")
                );
                assert!(
                    spec.environment
                        .contains(&("CLAUDE_CODE_DISABLE_AUTO_MEMORY".into(), "1".into()))
                );
            }
            CliBackend::Gemini => {
                let settings_path = spec
                    .environment
                    .iter()
                    .find(|(name, _)| name == "GEMINI_CLI_SYSTEM_SETTINGS_PATH")
                    .map(|(_, value)| value)
                    .unwrap();
                let settings: Value =
                    serde_json::from_str(&fs::read_to_string(settings_path).unwrap()).unwrap();
                assert_eq!(
                    settings.pointer("/tools/core"),
                    Some(&serde_json::json!([]))
                );
                assert_eq!(
                    settings.pointer("/mcp/allowed"),
                    Some(&serde_json::json!([]))
                );
                assert_eq!(
                    settings.pointer("/hooksConfig/enabled"),
                    Some(&Value::Bool(false))
                );
                assert!(
                    spec.args
                        .windows(2)
                        .any(|pair| pair == ["--extensions", "none"])
                );
                assert!(guard.is_some());
            }
            CliBackend::Opencode => {
                let permissions = spec
                    .environment
                    .iter()
                    .find(|(name, _)| name == "OPENCODE_PERMISSION")
                    .map(|(_, value)| value)
                    .unwrap();
                assert_eq!(permissions, r#"{"*":"deny"}"#);
            }
            CliBackend::Antigravity => {
                let deny = antigravity_deny_rules(&spec);
                for rule in [
                    "write_file(*)",
                    "command(*)",
                    "read_file(*)",
                    "search_web(*)",
                ] {
                    assert!(deny.iter().any(|value| value == rule), "{rule}");
                }
                assert!(
                    spec.args
                        .iter()
                        .any(|arg| arg == "--disable-slash-commands")
                );
                assert!(guard.is_some());
            }
            CliBackend::Custom => unreachable!(),
        }
    }
}

/// The deny rules `harden_spec` wrote into the private `--gemini_dir`.
fn antigravity_deny_rules(spec: &CliProcessSpec) -> Vec<String> {
    let directory = spec
        .args
        .iter()
        .find_map(|arg| arg.strip_prefix("--gemini_dir="))
        .expect("hardening must point agy at a private config directory");
    let settings: Value = serde_json::from_str(
        &fs::read_to_string(Path::new(directory).join("antigravity-cli/settings.json")).unwrap(),
    )
    .unwrap();
    settings["permissions"]["deny"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

#[test]
fn antigravity_pipeline_keeps_reads_but_denies_writes_shell_and_urls() {
    let mut spec = spec_for(CliBackend::Antigravity);
    let guard = harden_spec(CliBackend::Antigravity, Hardening::Pipeline, &mut spec).unwrap();
    let deny = antigravity_deny_rules(&spec);
    for rule in [
        "write_file(*)",
        "command(*)",
        "unsandboxed(*)",
        "execute_url(*)",
        "search_web(*)",
    ] {
        assert!(deny.iter().any(|value| value == rule), "{rule}");
    }
    assert!(!deny.iter().any(|value| value == "read_file(*)"));
    // The directory is private to this launch and goes away with the guard.
    let directory = spec.args[0]
        .strip_prefix("--gemini_dir=")
        .unwrap()
        .to_owned();
    assert!(Path::new(&directory).is_dir());
    drop(guard);
    assert!(!Path::new(&directory).exists());
}

#[test]
fn antigravity_launches_never_self_update_and_skip_the_npm_shim_path() {
    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join("agy.exe");
    fs::write(&executable, b"MZ fixture").unwrap();
    let spec = CliProcessSpec {
        executable,
        args: Vec::new(),
        environment: Vec::new(),
        stdin_payload: Vec::new(),
        resume_session_id: None,
    };
    let secured = secure_launch_spec(spec, CliBackend::Antigravity).unwrap();
    assert!(
        secured
            .spec
            .environment
            .contains(&("AGY_CLI_DISABLE_AUTO_UPDATE".into(), "true".into()))
    );
    assert!(secured.spec.executable.ends_with("agy.exe"));
}

#[test]
fn companion_launch_binds_model_and_profile_and_reports_model_evidence() {
    for (backend, model, profile) in [
        (CliBackend::Codex, "gpt-5.6-luna", "low"),
        (CliBackend::Claude, "haiku", "low"),
        (CliBackend::Opencode, "openai/gpt-5.6-luna", "low"),
    ] {
        let mut spec = build_process_spec(
            host().as_ref(),
            CliInvocationRequest {
                executable: PathBuf::from(r"C:\Tools\companion-cli.exe"),
                backend,
                model: Some(model.into()),
                model_profile: Some(profile.into()),
                thinking_level: ThinkingLevel::Default,
                prompt: "Return one control envelope".into(),
                schema_file: (backend == CliBackend::Codex)
                    .then(|| PathBuf::from(r"C:\Temp\schema.json")),
                schema: (backend == CliBackend::Claude).then(|| serde_json::json!({})),
                custom_args: vec![],
                resume_session_id: None,
            },
        )
        .unwrap();
        let _guard = harden_spec(backend, Hardening::Companion, &mut spec).unwrap();
        validate_companion_launch_selection(&spec, backend, Some(model), Some(profile)).unwrap();
        assert!(
            validate_companion_launch_selection(&spec, backend, Some("other"), Some(profile))
                .is_err()
        );
        assert!(
            validate_companion_launch_selection(&spec, backend, Some(model), Some("high")).is_err()
        );
    }

    let frame = serde_json::json!({"type":"stream_event","session_id":"s-1",
        "event":{"type":"message_start",
            "message":{"id":"m-1","model":"claude-haiku-4-5-20251001"}}})
    .to_string();
    let (observed, verification) =
        companion_model_evidence(host().as_ref(), CliBackend::Claude, Some("haiku"), &frame)
            .unwrap();
    assert_eq!(observed.as_deref(), Some("claude-haiku-4-5-20251001"));
    assert_eq!(verification, "family_verified");
    assert!(matches!(
        companion_model_evidence(host().as_ref(), CliBackend::Claude, Some("sonnet"), &frame),
        Err(TerminalError::ModelMismatch { .. })
    ));
    assert_eq!(
        companion_model_evidence(host().as_ref(), CliBackend::Codex, Some("gpt-5.6-luna"), "")
            .unwrap(),
        (None, "unverified")
    );
}

#[test]
fn model_identifiers_and_manual_profiles_fail_closed() {
    assert!(normalize_model_id(Some("provider/model:v1@fast".into())).is_ok());
    assert!(normalize_model_id(Some("provider/model with spaces".into())).is_err());
    assert!(
        validate_model_selection(
            CliBackend::Claude,
            Some("unverified-model"),
            Some("high"),
            CliModelSelectionSource::Manual,
        )
        .is_err()
    );
}

#[test]
fn every_official_adapter_receives_the_requested_model_and_supported_profile() {
    for (backend, model, profile, profile_flag, profile_value) in [
        (
            CliBackend::Codex,
            "gpt-5.4",
            Some("xhigh"),
            "-c",
            "model_reasoning_effort=\"xhigh\"",
        ),
        (
            CliBackend::Claude,
            "sonnet",
            Some("high"),
            "--effort",
            "high",
        ),
        (CliBackend::Gemini, "pro", None, "", ""),
        (
            CliBackend::Opencode,
            "openai/gpt-5.4",
            Some("deep"),
            "--variant",
            "deep",
        ),
    ] {
        let spec = build_process_spec(
            host().as_ref(),
            CliInvocationRequest {
                executable: PathBuf::from(format!(r"C:\Tools\{}.exe", backend.as_str())),
                backend,
                model: Some(model.into()),
                model_profile: profile.map(str::to_string),
                thinking_level: ThinkingLevel::Default,
                prompt: "Inspect the project".into(),
                schema_file: None,
                schema: None,
                custom_args: vec![],
                resume_session_id: None,
            },
        )
        .unwrap();
        assert!(
            spec.args.windows(2).any(|pair| pair == ["--model", model]),
            "model argument missing for {backend:?}"
        );
        if let Some(profile) = profile {
            assert!(
                spec.args
                    .windows(2)
                    .any(|pair| pair == [profile_flag, profile_value]),
                "profile {profile} missing for {backend:?}"
            );
        }
    }
}

#[test]
fn automatic_cli_launch_requires_one_explicit_selected_model() {
    let mut spec = build_process_spec(
        host().as_ref(),
        CliInvocationRequest {
            executable: PathBuf::from(r"C:\Tools\claude.exe"),
            backend: CliBackend::Claude,
            model: Some("haiku".into()),
            model_profile: None,
            thinking_level: ThinkingLevel::Default,
            prompt: "Inspect the project".into(),
            schema_file: None,
            schema: None,
            custom_args: vec![],
            resume_session_id: None,
        },
    )
    .unwrap();
    harden_spec(CliBackend::Claude, Hardening::Pipeline, &mut spec).unwrap();
    assert!(validate_cli_launch_model(CliBackend::Claude, Some("haiku"), &spec).is_ok());
    let settings_arg = spec
        .args
        .windows(2)
        .find(|pair| pair[0] == "--settings")
        .map(|pair| pair[1].as_str())
        .unwrap();
    let settings: Value = serde_json::from_str(settings_arg).unwrap();
    assert_eq!(settings["availableModels"], serde_json::json!(["haiku"]));
    assert_eq!(settings["enforceAvailableModels"], true);
    assert_eq!(settings["fallbackModel"], serde_json::json!([]));
    assert_eq!(
        validate_cli_launch_model(CliBackend::Claude, None, &spec)
            .unwrap_err()
            .code(),
        "model_unverified"
    );
    for dynamic_model in ["default", "inherit", "best", "opusplan"] {
        assert_eq!(
            validate_cli_launch_model(CliBackend::Claude, Some(dynamic_model), &spec)
                .unwrap_err()
                .code(),
            "model_unverified"
        );
    }
    let mut custom = build_process_spec(
        host().as_ref(),
        CliInvocationRequest {
            executable: PathBuf::from(r"C:\Tools\custom.exe"),
            backend: CliBackend::Custom,
            model: Some("haiku".into()),
            model_profile: None,
            thinking_level: ThinkingLevel::Default,
            prompt: "Inspect the project".into(),
            schema_file: None,
            schema: None,
            custom_args: vec![],
            resume_session_id: None,
        },
    )
    .unwrap();
    assert!(validate_cli_launch_model(CliBackend::Custom, Some("haiku"), &custom).is_ok());
    custom
        .environment
        .push(("HVC_SELECTED_MODEL".into(), "sonnet".into()));
    assert_eq!(
        validate_cli_launch_model(CliBackend::Custom, Some("haiku"), &custom)
            .unwrap_err()
            .code(),
        "model_mismatch"
    );
    assert_eq!(
        validate_cli_launch_model(CliBackend::Claude, Some("sonnet"), &spec)
            .unwrap_err()
            .code(),
        "model_mismatch"
    );
    spec.args.extend(["--model".into(), "sonnet".into()]);
    assert_eq!(
        validate_cli_launch_model(CliBackend::Claude, Some("haiku"), &spec)
            .unwrap_err()
            .code(),
        "model_mismatch"
    );
}

#[test]
fn custom_rejects_shell_scripts() {
    let error = build_process_spec(
        host().as_ref(),
        CliInvocationRequest {
            executable: PathBuf::from(r"C:\Temp\agent.cmd"),
            backend: CliBackend::Custom,
            model: None,
            model_profile: None,
            thinking_level: ThinkingLevel::Default,
            prompt: String::new(),
            schema_file: None,
            schema: None,
            custom_args: vec![],
            resume_session_id: None,
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("shell"));
}

#[cfg(windows)]
#[test]
fn trusted_npm_shim_resolves_to_node_and_tampering_is_rejected() {
    let installation = tempfile::tempdir().unwrap();
    let node = installation.path().join("node.exe");
    fs::copy(std::env::current_exe().unwrap(), &node).unwrap();
    let entrypoint = installation
        .path()
        .join("node_modules/@openai/codex/bin/codex.js");
    fs::create_dir_all(entrypoint.parent().unwrap()).unwrap();
    fs::write(&entrypoint, "console.log('fixture');\n").unwrap();
    let shim = installation.path().join("codex.cmd");
    fs::write(
        &shim,
        "@\"%~dp0\\node.exe\" \"%~dp0\\node_modules\\@openai\\codex\\bin\\codex.js\" %*\r\n",
    )
    .unwrap();

    let original = CliProcessSpec {
        executable: shim.clone(),
        args: vec!["exec".into()],
        environment: Vec::new(),
        stdin_payload: Vec::new(),
        resume_session_id: None,
    };
    let secured = secure_launch_spec(original.clone(), CliBackend::Codex).unwrap();
    assert_eq!(secured.spec.executable, node.canonicalize().unwrap());
    assert_eq!(
        secured.spec.args.first(),
        Some(&process_argument_path(&entrypoint.canonicalize().unwrap()))
    );
    assert_eq!(secured.spec.args.get(1).map(String::as_str), Some("exec"));
    assert_eq!(secured.identity_sha256.len(), 64);

    fs::write(
        &shim,
        "@\"%~dp0\\node.exe\" \"%~dp0\\node_modules\\untrusted\\index.js\" %*\r\n",
    )
    .unwrap();
    let error = secure_launch_spec(original.clone(), CliBackend::Codex).unwrap_err();
    assert!(error.to_string().contains("expected package"));

    fs::write(
        &shim,
        "@\"%~dp0\\node.exe\" \"%~dp0\\node_modules\\@openai\\codex\\bin\\codex.js\"\r\n",
    )
    .unwrap();
    let error = secure_launch_spec(original, CliBackend::Codex).unwrap_err();
    assert!(error.to_string().contains("expected format"));
}

#[test]
fn launch_identity_is_reused_while_metadata_holds_and_recomputed_when_it_changes() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let directory = tempfile::tempdir().unwrap();
    let binary = directory.path().join("cli.exe");
    fs::write(&binary, b"version one").unwrap();
    let reads = AtomicUsize::new(0);
    let identity = || {
        cached_launch_identity(&[&binary], || {
            reads.fetch_add(1, Ordering::SeqCst);
            hash_launch_files_uncached(&[&binary])
        })
        .unwrap()
    };
    let first = identity();
    assert_eq!(identity(), first);
    assert_eq!(
        reads.load(Ordering::SeqCst),
        1,
        "unchanged file is not re-read"
    );
    assert_eq!(first, hash_launch_files_uncached(&[&binary]).unwrap());

    // Same length, new content: the modification time alone invalidates.
    fs::write(&binary, b"version two").unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&binary)
        .unwrap()
        .set_modified(std::time::SystemTime::now() + Duration::from_secs(60))
        .unwrap();
    let second = identity();
    assert_ne!(second, first);
    assert_eq!(reads.load(Ordering::SeqCst), 2);
    assert_eq!(second, hash_launch_files_uncached(&[&binary]).unwrap());

    // A missing file is never served from the cache.
    fs::remove_file(&binary).unwrap();
    assert!(hash_launch_files(&[&binary]).is_err());
}

#[cfg(windows)]
#[test]
fn windows_discovery_prefers_cmd_over_posix_and_powershell_npm_shims() {
    let installation = tempfile::tempdir().unwrap();
    let extensions = windows_executable_extensions(Some(".PS1;.CMD;.EXE"));
    assert!(!extensions.iter().any(String::is_empty));
    assert!(!extensions.iter().any(|extension| extension == ".ps1"));

    for backend in ["gemini", "opencode"] {
        fs::write(installation.path().join(backend), "#!/bin/sh\nexit 0\n").unwrap();
        fs::write(
            installation.path().join(format!("{backend}.ps1")),
            "exit 0\n",
        )
        .unwrap();
        let command_shim = installation.path().join(format!("{backend}.cmd"));
        fs::write(&command_shim, "@ECHO off\r\nEXIT /b 0\r\n").unwrap();

        assert_eq!(
            find_executable(backend, &[installation.path().to_path_buf()], &extensions),
            Some(command_shim),
            "{backend} discovery must not select a POSIX or PowerShell shim"
        );
    }
}

#[cfg(windows)]
#[test]
fn windows_discovery_recovers_default_npm_path_from_gui_environment() {
    let inherited = tempfile::tempdir().unwrap();
    let app_data = tempfile::tempdir().unwrap();
    let joined_path = env::join_paths([inherited.path()]).unwrap();
    let environment = HashMap::from([
        ("PATH", joined_path),
        ("APPDATA", app_data.path().as_os_str().to_owned()),
    ]);

    let paths = cli_search_paths_from_environment(|name| environment.get(name).cloned());

    assert_eq!(paths.first(), Some(&inherited.path().to_path_buf()));
    assert!(paths.contains(&app_data.path().join("npm")));
}

#[cfg(windows)]
#[test]
fn antigravity_is_discovered_as_agy_never_as_the_ide_launcher() {
    // The IDE installs `antigravity.cmd` on PATH; launching it opens a window.
    let ide = tempfile::tempdir().unwrap();
    fs::write(ide.path().join("antigravity.cmd"), "@ECHO off\r\n").unwrap();
    let cli = tempfile::tempdir().unwrap();
    let agy = cli.path().join("agy.exe");
    fs::write(&agy, b"MZ fixture").unwrap();
    let paths = vec![ide.path().to_path_buf(), cli.path().to_path_buf()];
    assert_eq!(
        find_executable(
            CliBackend::Antigravity.executable_name(),
            &paths,
            &windows_executable_extensions(None)
        ),
        Some(agy)
    );

    let local_app_data = tempfile::tempdir().unwrap();
    let environment =
        HashMap::from([("LOCALAPPDATA", local_app_data.path().as_os_str().to_owned())]);
    let paths = cli_search_paths_from_environment(|name| environment.get(name).cloned());
    assert!(paths.contains(&local_app_data.path().join("agy/bin")));
}

#[cfg(windows)]
#[test]
fn gemini_and_opencode_npm_shims_resolve_current_entrypoints() {
    for (backend, command, relative_entrypoint) in [
        (
            CliBackend::Gemini,
            "gemini",
            "node_modules/@google/gemini-cli/bundle/gemini.js",
        ),
        (
            CliBackend::Opencode,
            "opencode",
            "node_modules/opencode-ai/bin/opencode",
        ),
    ] {
        let installation = tempfile::tempdir().unwrap();
        let node = installation.path().join("node.exe");
        fs::copy(std::env::current_exe().unwrap(), &node).unwrap();
        let entrypoint = installation.path().join(relative_entrypoint);
        fs::create_dir_all(entrypoint.parent().unwrap()).unwrap();
        fs::write(&entrypoint, "console.log('fixture');\n").unwrap();
        let shim = installation.path().join(format!("{command}.cmd"));
        fs::write(
            &shim,
            format!(
                "@ECHO off\r\n\"%~dp0\\node.exe\" \"%~dp0\\{}\" %*\r\n",
                relative_entrypoint.replace('/', "\\")
            ),
        )
        .unwrap();

        let secured = secure_launch_spec(
            CliProcessSpec {
                executable: shim,
                args: vec!["--version".into()],
                environment: Vec::new(),
                stdin_payload: Vec::new(),
                resume_session_id: None,
            },
            backend,
        )
        .unwrap();

        assert_eq!(secured.spec.executable, node.canonicalize().unwrap());
        assert!(!secured.spec.args[0].starts_with(r"\\?\"));
        assert_eq!(
            secured.spec.args,
            vec![
                process_argument_path(&entrypoint.canonicalize().unwrap()),
                "--version".to_string(),
            ]
        );
    }
}

#[cfg(windows)]
#[test]
fn opencode_native_executable_npm_shim_resolves_directly() {
    let installation = tempfile::tempdir().unwrap();
    let entrypoint = installation
        .path()
        .join("node_modules/opencode-ai/bin/opencode.exe");
    fs::create_dir_all(entrypoint.parent().unwrap()).unwrap();
    fs::write(&entrypoint, "binary-fixture\n").unwrap();
    let shim = installation.path().join("opencode.cmd");
    fs::write(
        &shim,
        "@ECHO off\r\n\"%~dp0\\node_modules\\opencode-ai\\bin\\opencode.exe\" %*\r\n",
    )
    .unwrap();

    let secured = secure_launch_spec(
        CliProcessSpec {
            executable: shim.clone(),
            args: vec!["--version".into()],
            environment: Vec::new(),
            stdin_payload: Vec::new(),
            resume_session_id: None,
        },
        CliBackend::Opencode,
    )
    .unwrap();

    assert_eq!(secured.spec.executable, entrypoint.canonicalize().unwrap());
    assert_eq!(secured.spec.args, vec!["--version".to_string()]);
    assert_eq!(secured.identity_sha256.len(), 64);
}

#[test]
fn sanitizer_removes_ansi_osc_and_control_characters() {
    let value = sanitize_terminal_text(b"ok\x1b[31m red\x1b[0m\x1b]0;title\x07\0done");
    assert_eq!(value, "ok reddone");
}

#[test]
fn cli_sanitization_rejects_changed_preparation_instead_of_silently_repreparing() {
    let prepared = fixture_proof("system", "original input").unwrap();
    assert!(
        verified_cli_input(host().as_ref(), "system", "original input", Some(&prepared)).is_ok()
    );
    for (system, user) in [
        ("changed system", "original input"),
        ("system", "changed input"),
    ] {
        assert!(matches!(
            verified_cli_input(host().as_ref(), system, user, Some(&prepared)),
            Err(TerminalError::Redaction(_))
        ));
    }
    assert!(verified_cli_input(host().as_ref(), "system", "unprepared input", None).is_ok());
}

#[tokio::test]
async fn cli_sanitization_preserves_operational_edit_literals_and_masks_only_observers() {
    let (_data, repository, run) = audit_test_run();
    let run_id = run.id.clone();
    let collected = Arc::new(CollectTerminalEvents::default());
    let literal = "const TOKEN: &str = \"ghp_synthetic_operational_fixture\";\r\n";
    let original_response = serde_json::json!({
        "outputLanguage": "en", "edits": [{"content": literal}]
    })
    .to_string();
    let (sender, receiver) = mpsc::channel(2);
    sender
        .send(ProcessStreamItem::Frame {
            stream: "stdout",
            received_at: Instant::now(),
            bytes: serde_json::json!({"type":"item.completed", "model":"fixture",
            "item":{"id":"operational-fixture","type":"agent_message","text":original_response}})
            .to_string()
            .into_bytes(),
        })
        .await
        .unwrap();
    drop(sender);
    let summary = coordinate_process_events(
        repository.clone(),
        collected.clone(),
        run,
        CliBackend::Codex,
        receiver,
        CancellationToken::new(),
        Arc::new(Mutex::new(None)),
        ProgressWatchdog::new(AgentTimeoutPolicy::default()),
        None,
    )
    .await
    .unwrap();
    assert_eq!(summary.raw_output, original_response);
    let parsed: Value = serde_json::from_str(&summary.raw_output).unwrap();
    assert_eq!(
        parsed["edits"][0]["content"].as_str().unwrap().as_bytes(),
        literal.as_bytes()
    );
    let events = repository.list_events(&run_id, 0, 100).unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.content.contains("<REDACTED_SECRET:"))
    );
    assert!(
        events
            .iter()
            .all(|event| !event.content.contains("ghp_synthetic_operational_fixture"))
    );
    assert!(collected.envelopes.lock().unwrap().iter().all(|envelope| {
        !envelope
            .event
            .content
            .contains("ghp_synthetic_operational_fixture")
    }));
    assert!(!format!("{summary:?}").contains("ghp_synthetic_operational_fixture"));
}

#[test]
fn cli_sanitization_redacts_continuations_and_isolates_streams() {
    let parts = [
        ("stdout", "Authorization: Bearer synthetic_"),
        ("stderr", "ordinary warning"),
        ("stdout", "split_token_value"),
    ];
    let result = redact_continuous_segments(host().as_ref(), &parts).unwrap();
    assert!(result[0].text.contains("<REDACTED_SECRET:"));
    assert!(result[2].text.is_empty());
    assert_eq!(result[1].text, "ordinary warning");
    assert_eq!(result.iter().map(|r| r.count).sum::<u64>(), 1);
}

#[test]
fn cli_sanitization_history_rescans_before_pagination_and_export() {
    let (_data, repository, run) = audit_test_run();
    let db = repository.connection().unwrap();
    for (sequence, content) in [
        (1, "Authorization: Bearer synthetic_"),
        (2, "legacy_split_value"),
    ] {
        db.execute("INSERT INTO terminal_events(id,run_id,sequence,kind,stream,content,created_at,redaction_count,redaction_categories_json) VALUES(?1,?2,?3,'message','stdout',?4,'2026-10-22T00:00:00Z',0,'[]')",
            params![format!("legacy-{sequence}"), run.id, sequence, content]).unwrap();
    }
    let first = repository.list_events(&run.id, 0, 1).unwrap();
    let second = repository.list_events(&run.id, 1, 1).unwrap();
    assert!(first[0].content.contains("<REDACTED_SECRET:"));
    assert!(second[0].content.is_empty());
    // The view is safe without rewriting legacy evidence in place.
    let raw: String = db
        .query_row(
            "SELECT content FROM terminal_events WHERE run_id=?1 AND sequence=2",
            [&run.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(raw, "legacy_split_value");
}

#[test]
fn cli_sanitization_legacy_export_hashes_original_and_safe_envelopes_separately() {
    let (_data, repository, mut run) = audit_test_run();
    let workspace = tempfile::tempdir().unwrap();
    let db = repository.connection().unwrap();
    db.execute(
        "UPDATE projects SET workspace_path=?1 WHERE id=?2",
        params![workspace.path().to_string_lossy(), run.project_id],
    )
    .unwrap();
    let diagnostic = "Authorization: Bearer synthetic_legacy_diagnostic";
    run.error_message = Some(diagnostic.into());
    db.execute(
        "UPDATE terminal_runs SET error_message=?1 WHERE id=?2",
        params![diagnostic, run.id],
    )
    .unwrap();
    let fragments = [
        "before\nAuthorization: Bearer synthetic_",
        "legacy_export_value\nvisible tail",
    ];
    for (index, content) in fragments.iter().enumerate() {
        let sequence = index + 1;
        db.execute("INSERT INTO terminal_events(id,run_id,sequence,kind,stream,content,created_at,redaction_count,redaction_categories_json) VALUES(?1,?2,?3,'message','stdout',?4,'2026-10-22T00:00:00Z',0,'[]')",
            params![format!("legacy-export-{sequence}"), run.id, sequence, content]).unwrap();
    }
    let original_events = read_run_history_events(&db, &run.id).unwrap();
    let mut original_envelope = serde_json::json!({
        "schemaVersion": 1, "redacted": true,
        "run": run, "events": original_events,
    });
    canonicalize_terminal_export(&mut original_envelope);
    let original_hash = lower_hex(&Sha256::digest(
        serde_json::to_vec(&original_envelope).unwrap(),
    ));
    let path = repository
        .export_run_history(&run.project_id, &run.id)
        .unwrap();
    let exported = fs::read_to_string(path).unwrap();
    let mut envelope: Value = serde_json::from_str(&exported).unwrap();
    let metadata = envelope
        .as_object_mut()
        .unwrap()
        .remove("sanitization")
        .unwrap();
    assert_eq!(envelope["schemaVersion"], 1);
    assert_eq!(envelope["redacted"], true);
    assert_eq!(metadata["policy"], SANITIZATION_POLICY_VERSION);
    assert_eq!(metadata["originalEvidenceSha256"], original_hash);
    canonicalize_terminal_export(&mut envelope);
    let sanitized_hash = lower_hex(&Sha256::digest(serde_json::to_vec(&envelope).unwrap()));
    assert_eq!(metadata["sanitizedContentSha256"], sanitized_hash);
    assert_ne!(
        metadata["originalEvidenceSha256"],
        metadata["sanitizedContentSha256"]
    );
    assert!(
        metadata["hashFormat"]
            .as_str()
            .unwrap()
            .contains("no trailing newline")
    );
    assert_eq!(envelope["events"].as_array().unwrap().len(), 2);
    assert!(
        envelope["events"][0]["content"]
            .as_str()
            .unwrap()
            .contains("<REDACTED_SECRET:")
    );
    assert_eq!(envelope["events"][1]["content"], "\nvisible tail");
    assert!(!exported.contains("synthetic_"));
    assert!(!exported.contains("legacy_export_value"));
    assert!(!exported.contains("legacy_diagnostic"));
    // Export only derives a view; the exact original evidence remains in SQLite.
    let after = read_run_history_events(&db, &run.id).unwrap();
    assert_eq!(
        after
            .iter()
            .map(|event| event.content.as_str())
            .collect::<Vec<_>>(),
        fragments
    );
    let raw_diagnostic: String = db
        .query_row(
            "SELECT error_message FROM terminal_runs WHERE id=?1",
            [&run.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(raw_diagnostic, diagnostic);
}

#[test]
fn cli_sanitization_export_bounds_encoded_bytes_with_typed_failure() {
    let value = serde_json::json!({"text": "\0\n\"".repeat(80)});
    for pretty in [false, true] {
        let expected = if pretty {
            serde_json::to_vec_pretty(&value).unwrap()
        } else {
            serde_json::to_vec(&value).unwrap()
        };
        assert!(matches!(
            terminal_export_bytes(&value, pretty, expected.len() - 1),
            Err(TerminalError::OutputLimit)
        ));
        assert_eq!(
            terminal_export_bytes(&value, pretty, expected.len()).unwrap(),
            expected
        );
    }
}

#[test]
fn cli_sanitization_legacy_unicode_json_stream_decodes_before_history_and_export() {
    let (_data, repository, run) = audit_test_run();
    let workspace = tempfile::tempdir().unwrap();
    let db = repository.connection().unwrap();
    db.execute(
        "UPDATE projects SET workspace_path=?1 WHERE id=?2",
        params![workspace.path().to_string_lossy(), run.project_id],
    )
    .unwrap();
    let original = r#"{"\u0070assword":"\u0061rbitrary phrase","nested":"{\"\\u0074oken\":\"\\u006eested phrase\"}","public":"ok"}"#;
    let split = original.len() / 2;
    let fragments = [&original[..split], &original[split..]];
    for (index, content) in fragments.iter().enumerate() {
        let sequence = index + 1;
        db.execute("INSERT INTO terminal_events(id,run_id,sequence,kind,stream,content,created_at,redaction_count,redaction_categories_json) VALUES(?1,?2,?3,'message','stdout',?4,'2026-10-22T00:00:00Z',0,'[]')",
            params![format!("legacy-json-{sequence}"), run.id, sequence, content]).unwrap();
    }
    let events = repository.list_events(&run.id, 0, 100).unwrap();
    assert_eq!(events.len(), 2);
    assert!(events[1].content.is_empty());
    assert_eq!(events[1].redaction_count, 0);
    assert_eq!(events[0].redaction_count, 2);
    let projected: Value = serde_json::from_str(&events[0].content).unwrap();
    assert!(
        projected["password"]
            .as_str()
            .unwrap()
            .starts_with("<REDACTED_SECRET:")
    );
    let nested: Value = serde_json::from_str(projected["nested"].as_str().unwrap()).unwrap();
    assert!(
        nested["token"]
            .as_str()
            .unwrap()
            .starts_with("<REDACTED_SECRET:")
    );
    assert_eq!(projected["public"], "ok");
    let export = repository
        .export_run_history(&run.project_id, &run.id)
        .unwrap();
    let exported = fs::read_to_string(export).unwrap();
    assert!(!exported.contains("arbitrary phrase"));
    assert!(!exported.contains("nested phrase"));
    assert!(!exported.contains("rbitrary phrase"));
    assert!(!exported.contains("ested phrase"));
    let envelope: Value = serde_json::from_str(&exported).unwrap();
    assert_eq!(envelope["events"][0]["content"], events[0].content);
    assert_eq!(envelope["events"][1]["content"], "");
    let originals = read_run_history_events(&db, &run.id).unwrap();
    assert_eq!(
        originals
            .iter()
            .map(|event| event.content.as_str())
            .collect::<String>(),
        original
    );
}

#[test]
fn cli_sanitization_multiline_regions_preserve_crlf_across_every_segment_boundary() {
    let original = "before\r\n-----BEGIN PRIVATE KEY-----\r\nprivate-synthetic-body\r\n-----END PRIVATE KEY-----\r\nafter";
    let expected = host().redact_context(original).unwrap().text;
    for split in 1..original.len() {
        let projected = redact_continuous_segments(
            host().as_ref(),
            &[
                ("stdout", &original[..split]),
                ("stdout", &original[split..]),
            ],
        )
        .unwrap();
        assert_eq!(
            projected
                .iter()
                .map(|part| part.text.as_str())
                .collect::<String>(),
            expected
        );
        assert_eq!(projected.iter().map(|part| part.count).sum::<u64>(), 1);
    }
}

#[test]
fn cli_sanitization_json_envelope_preserves_yaml_render_linebreaks_and_masks_keys() {
    let yaml = "name: visible\nitems:\n  - first\n  - second\n";
    let split = yaml.find("items:").unwrap();
    let projected = redact_continuous_segments(
        host().as_ref(),
        &[("stdout", &yaml[..split]), ("stdout", &yaml[split..])],
    )
    .unwrap();
    assert_eq!(projected[0].text, &yaml[..split]);
    assert_eq!(projected[1].text, &yaml[split..]);
    assert_eq!(projected.iter().map(|part| part.count).sum::<u64>(), 0);
    let key = "ghp_syntheticabcdefghijklmnop";
    let mut envelope = serde_json::json!({"render": yaml, key: "public"});
    assert_eq!(
        redact_json_strings_count(host().as_ref(), &mut envelope).unwrap(),
        1
    );
    assert_eq!(envelope["render"], yaml);
    assert!(!envelope.as_object().unwrap().contains_key(key));
    assert!(
        envelope
            .as_object()
            .unwrap()
            .keys()
            .any(|key| key.starts_with("<REDACTED_SECRET:"))
    );
}

#[test]
fn cli_sanitization_omits_unrecoverable_legacy_fragments_after_old_markers() {
    let (_data, repository, run) = audit_test_run();
    let db = repository.connection().unwrap();
    for (sequence, content) in [(1, "Bearer [REDACTED]"), (2, "unrecoverable-tail")] {
        db.execute("INSERT INTO terminal_events(id,run_id,sequence,kind,stream,content,created_at,redaction_count,redaction_categories_json) VALUES(?1,?2,?3,'message','stdout',?4,'2026-10-22T00:00:00Z',0,'[]')",
            params![format!("legacy-old-{sequence}"), run.id, sequence, content]).unwrap();
    }
    db.execute(
        "UPDATE terminal_runs SET error_message=?1 WHERE id=?2",
        params!["Authorization: Bearer synthetic_legacy_error_value", run.id],
    )
    .unwrap();
    assert!(
        !repository
            .get_run(&run.id)
            .unwrap()
            .error_message
            .unwrap()
            .contains("synthetic_legacy_error_value")
    );
    let events = repository.list_events(&run.id, 0, 100).unwrap();
    assert!(
        events
            .iter()
            .all(|event| event.content.contains("Legacy stream omitted"))
    );
    assert!(
        events
            .iter()
            .all(|event| !event.content.contains("unrecoverable-tail"))
    );
}

#[tokio::test]
async fn cli_sanitization_discards_incomplete_output_without_emitting_fragments() {
    let (_data, repository, run) = audit_test_run();
    let id = run.id.clone();
    let collected = Arc::new(CollectTerminalEvents::default());
    let (sender, receiver) = mpsc::channel(4);
    sender.send(ProcessStreamItem::Frame {
        stream: "stdout", received_at: Instant::now(),
        bytes: br#"{"type":"item.completed","model":"fixture","item":{"type":"agent_message","text":"-----BEGIN PRIVATE KEY-----\nsynthetic-partial"}}"#.to_vec(),
    }).await.unwrap();
    drop(sender);
    let summary = coordinate_process_events_with_completion(
        repository.clone(),
        collected.clone(),
        run,
        CliBackend::Codex,
        receiver,
        CancellationToken::new(),
        Arc::new(Mutex::new(None)),
        ProgressWatchdog::new(AgentTimeoutPolicy::default()),
        None,
        Some(Arc::new(Mutex::new(Some(false)))),
    )
    .await
    .unwrap();
    assert!(summary.raw_output.is_empty());
    assert!(repository.list_events(&id, 0, 100).unwrap().is_empty());
    assert!(collected.envelopes.lock().unwrap().is_empty());
}

#[test]
fn normalizes_structured_tool_events() {
    let (kind, text) = normalize_cli_frame(
        CliBackend::Gemini,
        "stdout",
        br#"{"type":"tool_result","result":"arquivo lido"}"#,
    );
    assert_eq!(kind, TerminalEventKind::ToolResult);
    assert_eq!(text, "arquivo lido");
}

#[test]
fn companion_stream_failure_preserves_database_error_classification() {
    let error = companion_stream_output(
        Ok(Err(std::io::Error::other(rusqlite::Error::InvalidQuery))),
        "stdout",
    )
    .unwrap_err();
    assert_eq!(error.code(), "cli_persistence_failed");
    let error = companion_stream_output(
        Ok(Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "output limit",
        ))),
        "stderr",
    )
    .unwrap_err();
    assert_eq!(error.code(), "cli_output_limit");
}

#[test]
fn pulsation_role_model_is_frozen_across_terminal_runs() {
    use crate::test_support::TestDatabase;

    let data = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let persistence = TestDatabase::initialize(data.path()).unwrap();
    let project = persistence
        .insert_project(
            workspace.path().to_string_lossy().into_owned(),
            Some("Frozen CLI role".into()),
        )
        .unwrap();
    let repository = test_repository(persistence.database_path());
    repository
        .ensure_project_configs(&project.id, true)
        .unwrap();
    let pulse = persistence
        .insert_pulsation(project.id.clone(), "Frozen CLI role".into())
        .unwrap();
    let create = |role, backend, model: &str| CreateTerminalRunRequest {
        project_id: project.id.clone(),
        agent_role: role,
        origin: ExecutionOrigin::Pipeline,
        execution_mode: AgentExecutionMode::CliProcess,
        backend: Some(backend),
        model: Some(model.into()),
        pulsation_id: Some(pulse.id.clone()),
        job_id: None,
        agent_run_id: None,
        llm_execution_id: None,
    };
    let first = repository
        .create_run(create(AgentRole::IntakeRouter, CliBackend::Claude, "haiku"))
        .unwrap();
    assert!(
        repository
            .has_cli_role_run(&pulse.id, AgentRole::IntakeRouter)
            .unwrap()
    );
    assert!(
        !repository
            .has_cli_role_run(&pulse.id, AgentRole::BackendAgent)
            .unwrap()
    );
    repository
        .set_run_status(
            &first.id,
            TerminalRunStatus::Failed,
            None,
            Some("fixture_done"),
            None,
        )
        .unwrap();
    let second = repository
        .create_run(create(AgentRole::IntakeRouter, CliBackend::Claude, "haiku"))
        .unwrap();
    repository
        .set_run_status(
            &second.id,
            TerminalRunStatus::Failed,
            None,
            Some("fixture_done"),
            None,
        )
        .unwrap();
    assert!(matches!(
        repository.clear_history(&project.id, Some(AgentRole::IntakeRouter)),
        Err(TerminalError::Validation(_))
    ));
    assert_eq!(
        repository
            .create_run(create(
                AgentRole::IntakeRouter,
                CliBackend::Claude,
                "sonnet"
            ))
            .unwrap_err()
            .code(),
        "model_mismatch"
    );
    assert_eq!(
        repository
            .create_run(create(AgentRole::IntakeRouter, CliBackend::Codex, "haiku"))
            .unwrap_err()
            .code(),
        "model_mismatch"
    );
    repository
        .create_run(create(
            AgentRole::BackendAgent,
            CliBackend::Codex,
            "gpt-5.6-luna",
        ))
        .unwrap();
}

#[test]
fn repository_round_trips_project_configs_runs_and_events() {
    use crate::test_support::TestDatabase;

    let data = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let persistence = TestDatabase::initialize(data.path()).unwrap();
    let project = persistence
        .insert_project(
            workspace.path().to_string_lossy().into_owned(),
            Some("Agents".into()),
        )
        .unwrap();
    let repository = test_repository(persistence.database_path());

    let configs = repository
        .ensure_project_configs(&project.id, true)
        .unwrap();
    assert_eq!(configs.len(), AgentRole::ALL.len());
    assert!(configs.iter().all(
        |config| config.execution_mode == AgentExecutionMode::CliProcess
            && config.backend == CliBackend::Codex
    ));

    let original = repository
        .get_config(&project.id, AgentRole::BackendAgent)
        .unwrap();
    let updated = repository
        .update_config(UpdateCliAgentConfigRequest {
            project_id: project.id.clone(),
            agent_role: AgentRole::BackendAgent,
            execution_mode: AgentExecutionMode::ApiDirect,
            backend: CliBackend::Claude,
            model: Some(" test-model ".into()),
            model_profile: None,
            model_source: CliModelSelectionSource::Manual,
            thinking_level: ThinkingLevel::Medium,
            custom_executable: None,
            custom_args: vec![],
            custom_risk_ack: false,
            expected_version: original.version,
        })
        .unwrap();
    assert_eq!(updated.version, original.version + 1);
    assert_eq!(updated.model.as_deref(), Some("test-model"));
    assert_eq!(updated.model_profile, None);
    assert!(matches!(
        repository.update_config(UpdateCliAgentConfigRequest {
            expected_version: original.version,
            project_id: project.id.clone(),
            agent_role: AgentRole::BackendAgent,
            execution_mode: AgentExecutionMode::ApiDirect,
            backend: CliBackend::Claude,
            model: None,
            model_profile: None,
            model_source: CliModelSelectionSource::Default,
            thinking_level: ThinkingLevel::Default,
            custom_executable: None,
            custom_args: vec![],
            custom_risk_ack: false
        }),
        Err(TerminalError::Conflict)
    ));

    let run = repository
        .create_run(CreateTerminalRunRequest {
            project_id: project.id.clone(),
            agent_role: AgentRole::BackendAgent,
            origin: ExecutionOrigin::Manual,
            execution_mode: AgentExecutionMode::CliProcess,
            backend: Some(CliBackend::Codex),
            model: None,
            pulsation_id: None,
            job_id: None,
            agent_run_id: None,
            llm_execution_id: None,
        })
        .unwrap();
    repository
        .set_run_status(&run.id, TerminalRunStatus::Starting, None, None, None)
        .unwrap();
    repository
        .set_run_status(&run.id, TerminalRunStatus::Running, None, None, None)
        .unwrap();
    repository
        .append_event(
            &run.id,
            1,
            TerminalEventKind::Message,
            Some("stdout"),
            "\u{1b}[31msaída\u{1b}[0m",
        )
        .unwrap();
    assert_eq!(
        repository.list_events(&run.id, 0, 20).unwrap()[0].content,
        "saída"
    );
    repository
        .append_event(
            &run.id,
            2,
            TerminalEventKind::Stderr,
            Some("stderr"),
            "Authorization: Bearer fixture-secret-token",
        )
        .unwrap();
    let redacted = &repository.list_events(&run.id, 1, 20).unwrap()[0];
    assert!(
        redacted
            .content
            .starts_with("Authorization: Bearer <REDACTED_SECRET:")
    );
    assert_eq!(redacted.redaction_count, 1);
    // Categories are supplied by the host; the native fixture uses its own category.
    assert_eq!(redacted.redaction_categories, ["fixture"]);
    assert!(matches!(
        repository.clear_history(&project.id, Some(AgentRole::BackendAgent)),
        Err(TerminalError::Validation(_))
    ));
    assert_eq!(repository.recover_interrupted_runs().unwrap(), 1);
    assert_eq!(
        repository.get_run(&run.id).unwrap().status,
        TerminalRunStatus::Interrupted
    );
    assert!(matches!(
        repository.set_run_status(&run.id, TerminalRunStatus::Running, None, None, None),
        Err(TerminalError::InvalidTransition { .. })
    ));
    assert_eq!(
        repository
            .clear_history(&project.id, Some(AgentRole::BackendAgent))
            .unwrap(),
        1
    );
}

#[test]
fn backend_override_status_reports_the_current_stage_election_and_reason() {
    use crate::test_support::TestDatabase;

    let data = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let persistence = TestDatabase::initialize(data.path()).unwrap();
    let project = persistence
        .insert_project(
            workspace.path().to_string_lossy().into_owned(),
            Some("Override status".into()),
        )
        .unwrap();
    let pulse = persistence
        .insert_pulsation(project.id.clone(), "Override status".into())
        .unwrap();
    let repository = test_repository(persistence.database_path());
    let role = AgentRole::for_stage(pulse.stage.as_db_value());

    let connection = Connection::open(persistence.database_path()).unwrap();
    connection
        .execute(
            "INSERT INTO pulse_execution_overrides(pulsation_id,agent_role,execution_mode,backend,source_checkpoint_id,created_at) \
             VALUES(?1,?2,'cli_process',?3,NULL,?4)",
            params![pulse.id, role.as_str(), CliBackend::Codex.as_str(), "2026-09-10T00:00:00Z"],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO pulse_events(id,pulsation_id,event_type,from_status,to_status,from_stage,to_stage,message,created_at) \
             VALUES('evt-election',?1,'cli_backend_elected','idle','idle',?2,?2,?3,?4)",
            params![
                pulse.id,
                pulse.stage.as_db_value(),
                "OpenCode falhou (cli_not_found); a etapa será repetida em Codex.",
                "2026-09-10T00:00:01Z"
            ],
        )
        .unwrap();

    let status = repository
        .backend_override_status(&pulse.id)
        .unwrap()
        .expect("an active override should be reported");
    assert_eq!(status.backend, CliBackend::Codex);
    assert_eq!(status.agent_role, role);
    assert_eq!(
        status.reason.as_deref(),
        Some("OpenCode falhou (cli_not_found); a etapa será repetida em Codex.")
    );
    // create_pulsation enqueues the first stage's job and leaves it pending.
    assert_eq!(status.job_status.as_deref(), Some("pending"));
}

#[test]
fn backend_override_status_is_none_once_the_stage_moved_on() {
    use crate::test_support::TestDatabase;

    let data = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let persistence = TestDatabase::initialize(data.path()).unwrap();
    let project = persistence
        .insert_project(
            workspace.path().to_string_lossy().into_owned(),
            Some("Override stale".into()),
        )
        .unwrap();
    let pulse = persistence
        .insert_pulsation(project.id.clone(), "Override stale".into())
        .unwrap();
    let repository = test_repository(persistence.database_path());
    let stale_role = AgentRole::for_stage(pulse.stage.as_db_value());

    let connection = Connection::open(persistence.database_path()).unwrap();
    connection
        .execute(
            "INSERT INTO pulse_execution_overrides(pulsation_id,agent_role,execution_mode,backend,source_checkpoint_id,created_at) \
             VALUES(?1,?2,'cli_process',?3,NULL,?4)",
            params![pulse.id, stale_role.as_str(), CliBackend::Codex.as_str(), "2026-09-10T00:00:00Z"],
        )
        .unwrap();
    // The pulsation moved past the stage that override belonged to; a
    // fresh probe of the *current* stage must not report stale data.
    connection
        .execute(
            "UPDATE pulsations SET stage='Contract' WHERE id=?1",
            [&pulse.id],
        )
        .unwrap();

    assert!(
        repository
            .backend_override_status(&pulse.id)
            .unwrap()
            .is_none(),
        "an override left behind by a stage the pulsation already left must not surface"
    );
}

#[test]
fn custom_identity_change_requires_a_fresh_explicit_acknowledgement() {
    use crate::test_support::TestDatabase;

    let data = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let persistence = TestDatabase::initialize(data.path()).unwrap();
    let project = persistence
        .insert_project(
            workspace.path().to_string_lossy().into_owned(),
            Some("Custom ack".into()),
        )
        .unwrap();
    let repository = test_repository(persistence.database_path());
    repository
        .ensure_project_configs(&project.id, false)
        .unwrap();
    let original = repository
        .get_config(&project.id, AgentRole::BackendAgent)
        .unwrap();
    let executable = std::env::current_exe()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let error = repository
        .update_config(UpdateCliAgentConfigRequest {
            project_id: project.id.clone(),
            agent_role: AgentRole::BackendAgent,
            execution_mode: AgentExecutionMode::CliProcess,
            backend: CliBackend::Custom,
            model: None,
            model_profile: None,
            model_source: CliModelSelectionSource::Default,
            thinking_level: ThinkingLevel::Default,
            custom_executable: Some(executable.clone()),
            custom_args: Vec::new(),
            custom_risk_ack: true,
            expected_version: original.version,
        })
        .unwrap_err();
    assert!(error.to_string().contains("saving in API Mode"));
    let staged = repository
        .update_config(UpdateCliAgentConfigRequest {
            project_id: project.id.clone(),
            agent_role: AgentRole::BackendAgent,
            execution_mode: AgentExecutionMode::ApiDirect,
            backend: CliBackend::Custom,
            model: None,
            model_profile: None,
            model_source: CliModelSelectionSource::Default,
            thinking_level: ThinkingLevel::Default,
            custom_executable: Some(executable.clone()),
            custom_args: Vec::new(),
            custom_risk_ack: true,
            expected_version: original.version,
        })
        .unwrap();
    assert!(!staged.custom_risk_ack);
    let acknowledged = repository
        .update_config(UpdateCliAgentConfigRequest {
            project_id: project.id,
            agent_role: AgentRole::BackendAgent,
            execution_mode: AgentExecutionMode::CliProcess,
            backend: CliBackend::Custom,
            model: None,
            model_profile: None,
            model_source: CliModelSelectionSource::Default,
            thinking_level: ThinkingLevel::Default,
            custom_executable: Some(executable),
            custom_args: Vec::new(),
            custom_risk_ack: true,
            expected_version: staged.version,
        })
        .unwrap();
    assert!(acknowledged.custom_risk_ack);
}

#[test]
fn terminal_slots_backfill_and_free_form_crud_enforce_the_project_limit() {
    use crate::test_support::TestDatabase;

    let data = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let persistence = TestDatabase::initialize(data.path()).unwrap();
    let project = persistence
        .insert_project(
            workspace.path().to_string_lossy().into_owned(),
            Some("Slots".into()),
        )
        .unwrap();
    let repository = test_repository(persistence.database_path());
    repository
        .ensure_project_configs(&project.id, true)
        .unwrap();
    let canonical_count: u32 = rusqlite::Connection::open(persistence.database_path())
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM terminal_slots WHERE project_id=?1 AND kind='canonical'",
            [&project.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(canonical_count as usize, AgentRole::ALL.len());

    let mut slots = Vec::new();
    for index in 0..8 {
        slots.push(
            repository
                .create_free_form_slot(CreateFreeFormSlotRequest {
                    project_id: project.id.clone(),
                    label: format!("Livre {index}"),
                })
                .unwrap(),
        );
    }
    assert!(matches!(
        repository.create_free_form_slot(CreateFreeFormSlotRequest {
            project_id: project.id.clone(),
            label: "Excedente".into(),
        }),
        Err(TerminalError::Validation(_))
    ));
    let mut order = slots.iter().map(|slot| slot.id.clone()).collect::<Vec<_>>();
    order.reverse();
    repository
        .reorder_free_form_slots(&project.id, &order)
        .unwrap();
    assert_eq!(
        repository
            .list_free_form_slots(&project.id, false)
            .unwrap()
            .iter()
            .map(|slot| slot.id.clone())
            .collect::<Vec<_>>(),
        order
    );
    let first = repository
        .get_slot(
            &project.id,
            &SessionSlot::FreeForm {
                id: slots[0].id.clone(),
            },
        )
        .unwrap();
    let archived = repository
        .update_free_form_slot(UpdateFreeFormSlotRequest {
            id: first.id.clone(),
            project_id: first.project_id.clone(),
            label: first.label.clone(),
            execution_mode: first.execution_mode,
            backend: first.backend,
            model: first.model.clone(),
            model_profile: first.model_profile.clone(),
            model_source: first.model_source,
            thinking_level: first.thinking_level,
            session_policy: first.session_policy,
            custom_executable: first.custom_executable.clone(),
            custom_args: first.custom_args.clone(),
            custom_risk_ack: first.custom_risk_ack,
            position: first.position,
            archived: true,
            expected_version: first.version,
        })
        .unwrap();
    assert_eq!(
        repository
            .list_free_form_slots(&project.id, false)
            .unwrap()
            .len(),
        7
    );
    assert_eq!(
        repository
            .list_free_form_slots(&project.id, true)
            .unwrap()
            .iter()
            .filter(|slot| slot.archived)
            .count(),
        1
    );
    let restored = repository
        .update_free_form_slot(UpdateFreeFormSlotRequest {
            id: archived.id.clone(),
            project_id: archived.project_id.clone(),
            label: archived.label.clone(),
            execution_mode: archived.execution_mode,
            backend: archived.backend,
            model: archived.model.clone(),
            model_profile: archived.model_profile.clone(),
            model_source: archived.model_source,
            thinking_level: archived.thinking_level,
            session_policy: archived.session_policy,
            custom_executable: archived.custom_executable.clone(),
            custom_args: archived.custom_args.clone(),
            custom_risk_ack: archived.custom_risk_ack,
            position: archived.position,
            archived: false,
            expected_version: archived.version,
        })
        .unwrap();
    let run = repository
        .create_session_run(CreateSessionRunRequest {
            project_id: project.id.clone(),
            slot: SessionSlot::FreeForm {
                id: restored.id.clone(),
            },
            origin: ExecutionOrigin::Manual,
            execution_mode: restored.execution_mode,
            backend: Some(restored.backend),
            model: restored.model.clone(),
            pulsation_id: None,
            job_id: None,
            agent_run_id: None,
            llm_execution_id: None,
            parent_run_id: None,
            continuation_reason: None,
            observation_trial_id: None,
        })
        .unwrap();
    assert!(matches!(
        repository.clear_slot_history(&project.id, &restored.id),
        Err(TerminalError::Validation(_))
    ));
    repository
        .set_run_status(&run.id, TerminalRunStatus::Starting, None, None, None)
        .unwrap();
    repository
        .set_run_status(&run.id, TerminalRunStatus::Running, None, None, None)
        .unwrap();
    repository
        .set_run_status(&run.id, TerminalRunStatus::Completed, Some(0), None, None)
        .unwrap();
    for sequence in 1..=501 {
        repository
            .append_event(
                &run.id,
                sequence,
                TerminalEventKind::Stdout,
                Some("stdout"),
                "linha",
            )
            .unwrap();
    }
    let export = repository.export_run_history(&project.id, &run.id).unwrap();
    let payload: Value = serde_json::from_slice(&fs::read(&export).unwrap()).unwrap();
    assert_eq!(payload["events"].as_array().unwrap().len(), 501);
    let second_export = repository.export_run_history(&project.id, &run.id).unwrap();
    assert_eq!(second_export, export);
    assert_eq!(
        repository
            .clear_slot_history(&project.id, &restored.id)
            .unwrap(),
        1
    );
    repository
        .delete_free_form_slot(&project.id, &restored.id, restored.version)
        .unwrap();
    assert_eq!(
        repository
            .list_free_form_slots(&project.id, false)
            .unwrap()
            .len(),
        7
    );
}

#[test]
fn ephemeral_slots_are_isolated_from_free_form_and_each_other() {
    use crate::test_support::TestDatabase;

    let data = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let persistence = TestDatabase::initialize(data.path()).unwrap();
    let project = persistence
        .insert_project(
            workspace.path().to_string_lossy().into_owned(),
            Some("Execution slots".into()),
        )
        .unwrap();
    let repository = test_repository(persistence.database_path());
    repository
        .ensure_project_configs(&project.id, true)
        .unwrap();
    let role = AgentRole::BackendAgent;
    let slots = [
        "ephemeral:execution-a:unit-a",
        "ephemeral:execution-a:unit-b",
    ];
    for id in &slots {
        repository
            .ensure_ephemeral_slot(&project.id, id, role)
            .unwrap();
    }
    let create = |id: &str| CreateSessionRunRequest {
        project_id: project.id.clone(),
        slot: SessionSlot::Ephemeral {
            id: id.into(),
            agent_role: role,
        },
        origin: ExecutionOrigin::Pipeline,
        execution_mode: AgentExecutionMode::CliProcess,
        backend: Some(CliBackend::Codex),
        model: None,
        pulsation_id: None,
        job_id: None,
        agent_run_id: None,
        llm_execution_id: None,
        parent_run_id: None,
        continuation_reason: None,
        observation_trial_id: None,
    };
    let first = repository.create_session_run(create(slots[0])).unwrap();
    let second = repository.create_session_run(create(slots[1])).unwrap();
    assert_ne!(first.slot_id, second.slot_id);
    assert!(matches!(first.slot, SessionSlot::Ephemeral { .. }));
    assert!(
        repository
            .list_free_form_slots(&project.id, false)
            .unwrap()
            .is_empty()
    );
    let mut manual = create(slots[0]);
    manual.origin = ExecutionOrigin::Manual;
    assert!(matches!(
        repository.create_session_run(manual),
        Err(TerminalError::Validation(_))
    ));
}

#[test]
fn observation_trial_reserves_exactly_twenty_automatic_pulsation_slots() {
    use crate::test_support::TestDatabase;

    let data = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let persistence = TestDatabase::initialize(data.path()).unwrap();
    let project = persistence
        .insert_project(
            workspace.path().to_string_lossy().into_owned(),
            Some("Observation trial".into()),
        )
        .unwrap();
    let repository = test_repository(persistence.database_path());
    let trial = repository.start_cli_observation_trial(&project.id).unwrap();

    // Companion/manual, discovery and non-Pulsation work never consume a slot.
    assert!(
        repository
            .reserve_cli_observation_for_pulsation(&project.id, None, ExecutionOrigin::Pipeline)
            .unwrap()
            .is_none()
    );
    assert!(
        repository
            .reserve_cli_observation_for_pulsation(
                &project.id,
                Some("pulse"),
                ExecutionOrigin::Manual
            )
            .unwrap()
            .is_none()
    );
    assert!(
        repository
            .reserve_cli_observation_for_pulsation(
                &project.id,
                Some("pulse"),
                ExecutionOrigin::Feature
            )
            .unwrap()
            .is_none()
    );

    for _ in 0..20 {
        assert_eq!(
            repository
                .reserve_cli_observation_for_pulsation(
                    &project.id,
                    Some("pulse"),
                    ExecutionOrigin::Pipeline
                )
                .unwrap()
                .as_ref()
                .map(|reserved| reserved.id.as_str()),
            Some(trial.id.as_str())
        );
    }
    let closing = repository.get_cli_observation_trial(&trial.id).unwrap();
    assert_eq!(closing.admitted_runs, 20);
    assert_eq!(closing.state, "closing");
    assert!(
        repository
            .reserve_cli_observation_for_pulsation(
                &project.id,
                Some("pulse"),
                ExecutionOrigin::Learning
            )
            .unwrap()
            .is_none()
    );
}

/// The report explains a run's duration (tokens over throughput, boot,
/// preparation, admission) and a Pulsation's wall time (CLI, host, human
/// waits), and keeps a rejected output out of the accepted mean.
#[test]
fn observation_report_explains_durations_and_the_pulsation_critical_path() {
    use crate::test_support::TestDatabase;

    let data = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let persistence = TestDatabase::initialize(data.path()).unwrap();
    let project = persistence
        .insert_project(
            workspace.path().to_string_lossy().into_owned(),
            Some("Observation report".into()),
        )
        .unwrap();
    let repository = test_repository(persistence.database_path());
    let trial = repository.start_cli_observation_trial(&project.id).unwrap();
    let at = |seconds: i64| {
        (chrono::DateTime::parse_from_rfc3339("2026-09-22T18:00:00+00:00").unwrap()
            + chrono::Duration::seconds(seconds))
        .to_rfc3339()
    };
    let db = repository.connection().unwrap();
    db.execute(
        "INSERT INTO pulsations(id,project_id,feature_name,stage,status,created_at,updated_at) VALUES('p1',?1,'Todo','Code','failed',?2,?3)",
        params![project.id, at(0), at(600)],
    )
    .unwrap();
    for (id, kind, class, started, finished) in [
        ("j-spec", "run_stage::SPECIFICATION", "main", 1, 300),
        ("j-learn", "learning_report", "background", 310, 400),
    ] {
        db.execute(
            "INSERT INTO jobs(id,pulsation_id,project_id,kind,status,job_class,created_at,updated_at,started_at,finished_at) VALUES(?1,'p1',?2,?3,'done',?4,?5,?6,?5,?6)",
            params![id, project.id, kind, class, at(started), at(finished)],
        )
        .unwrap();
    }
    db.execute(
        "INSERT INTO learning_runs(job_id,pulsation_id,integration_epoch,state,created_at,updated_at) VALUES('j-learn','p1',1,'published',?1,?1)",
        [at(400)],
    )
    .unwrap();
    db.execute(
        "INSERT INTO agent_runs(id,job_id,agent_name,status,created_at,updated_at) VALUES('ar-code','j-spec','backend_agent','failed',?1,?1)",
        [at(241)],
    )
    .unwrap();
    db.execute(
        "INSERT INTO checkpoints(id,pulsation_id,stage,status,created_at,updated_at) VALUES('c1','p1','SPECIFICATION','approved',?1,?2)",
        params![at(100), at(160)],
    )
    .unwrap();
    for (id, role, origin, backend, model, job, agent_run, created, started, finished, silence) in [
        (
            "r-spec",
            "task_specifier",
            "pipeline",
            "opencode",
            "openrouter/nex-agi/nex-n2.5-pro:free",
            "j-spec",
            None,
            11,
            14,
            214,
            Some(190_000_i64),
        ),
        (
            "r-code",
            "backend_agent",
            "pipeline",
            "codex",
            "gpt-5.6-luna",
            "j-spec",
            Some("ar-code"),
            220,
            225,
            241,
            None,
        ),
        (
            "r-learn",
            "learning_agent",
            "learning",
            "claude",
            "haiku",
            "j-learn",
            None,
            310,
            313,
            393,
            None,
        ),
    ] {
        db.execute(
            "INSERT INTO terminal_runs(id,project_id,agent_role,origin,execution_mode,backend,model,pulsation_id,job_id,agent_run_id,status,created_at,started_at,finished_at,updated_at,observation_trial_id,max_transport_silence_ms)
             VALUES(?1,?2,?3,?4,'cli_process',?5,?6,'p1',?7,?8,'completed',?9,?10,?11,?11,?12,?13)",
            params![id, project.id, role, origin, backend, model, job, agent_run, at(created), at(started), at(finished), trial.id, silence],
        )
        .unwrap();
    }
    db.execute(
        "INSERT INTO terminal_events(run_id,sequence,kind,stream,content,created_at) VALUES('r-spec',1,'system','stdout','step_start',?1)",
        [at(18)],
    )
    .unwrap();
    for (run, unit, requested, observed, input, output, total, reasoning) in [
        (
            "r-spec",
            "u1",
            "openrouter/nex-agi/nex-n2.5-pro:free",
            None,
            12_402,
            1_862,
            17_293,
            Some(3_029),
        ),
        (
            "r-learn",
            "u2",
            "haiku",
            Some("claude-haiku-4-5-20251001"),
            7_018,
            14,
            7_032,
            None,
        ),
        (
            "r-learn",
            "u3",
            "haiku",
            Some("claude-sonnet-5"),
            22_547,
            12_826,
            35_373,
            None,
        ),
    ] {
        db.execute(
            "INSERT INTO token_usage_records(source_kind,source_id,unit_key,transport,provider_id,requested_model,observed_model,input_tokens,output_tokens,total_tokens,reasoning_tokens,quality,started_at,updated_at)
             VALUES('terminal_run',?1,?2,'cli','x',?3,?4,?5,?6,?7,?8,'complete',?9,?9)",
            params![run, unit, requested, observed, input, output, total, reasoning, at(0)],
        )
        .unwrap();
    }
    for (run_id, ordinal, model_id) in [
        ("r-spec", 1_i64, "nex-agi/nex-n2.5-pro"),
        ("r-learn", 1_i64, "claude-haiku-4-5-20251001"),
        ("r-learn", 2_i64, "claude-sonnet-5"),
    ] {
        db.execute(
            "INSERT INTO terminal_model_observations(run_id,ordinal,event_sequence,model_id,message_id,is_generation,created_at)
             VALUES(?1,?2,?2,?3,?4,1,?5)",
            params![run_id, ordinal, model_id, format!("message-{ordinal}"), at(214)],
        )
        .unwrap();
    }

    let report = repository
        .latest_cli_observation_report(&project.id)
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            report.successful_runs,
            report.stage_rejected_runs,
            report.failed_runs
        ),
        (1, 2, 0),
        "exit 0 with a rejected output is not a success"
    );
    assert_eq!(report.max_success_duration_ms, Some(200_000));
    assert_eq!(report.max_transport_silence_ms, Some(190_000));
    assert_eq!(
        report.build_profile,
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );
    let spec = report
        .runs
        .iter()
        .find(|run| run.run_id == "r-spec")
        .unwrap();
    assert_eq!(
        (spec.prep_ms, spec.boot_ms, spec.admission_ms),
        (Some(3_000), Some(4_000), Some(10_000))
    );
    assert_eq!(spec.generated_tokens, Some(4_891));
    let rate = spec.tokens_per_second.unwrap();
    assert!((rate - 24.95).abs() < 0.01, "{rate}");
    let code = report
        .runs
        .iter()
        .find(|run| run.run_id == "r-code")
        .unwrap();
    assert_eq!(code.outcome, ObservationOutcome::StageRejected);
    assert_eq!(
        code.admission_ms,
        Some(6_000),
        "measured from the previous run of the job"
    );
    let learning = report
        .runs
        .iter()
        .find(|run| run.run_id == "r-learn")
        .unwrap();
    assert_eq!(learning.outcome, ObservationOutcome::StageRejected);
    assert_eq!(learning.observed_model.as_deref(), Some("claude-sonnet-5"));
    assert!(
        learning.model_mismatch,
        "haiku was requested, sonnet did the work"
    );
    assert!(!learning.model_verified);
    assert_eq!(report.groups.len(), 3);
    let [pulsation] = report.pulsations.as_slice() else {
        panic!("one pulsation: {:?}", report.pulsations);
    };
    assert_eq!(
        (
            pulsation.total_ms,
            pulsation.cli_ms,
            pulsation.prep_ms,
            pulsation.admission_ms,
            pulsation.human_wait_ms,
            pulsation.other_ms
        ),
        (300_000, 216_000, 8_000, 16_000, 60_000, 0),
        "Learning runs in the background and is not on the critical path"
    );
}

#[test]
fn explicit_resume_fencing_rejects_workspace_backend_model_and_session_changes() {
    use crate::test_support::TestDatabase;

    let data = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let persistence = TestDatabase::initialize(data.path()).unwrap();
    let project = persistence
        .insert_project(
            workspace.path().to_string_lossy().into_owned(),
            Some("Resume".into()),
        )
        .unwrap();
    let repository = test_repository(persistence.database_path());
    repository
        .ensure_project_configs(&project.id, true)
        .unwrap();
    let parent = repository
        .create_run(CreateTerminalRunRequest {
            project_id: project.id.clone(),
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
        .unwrap();
    let fingerprint = session_fingerprint(&parent, "workspace-a");
    repository
        .set_workspace_snapshot_hash(&parent.id, "workspace-a")
        .unwrap();
    repository
        .set_session_fingerprint(&parent.id, &fingerprint)
        .unwrap();
    repository
        .set_native_session_id(&parent.id, "session-a")
        .unwrap();
    repository
        .set_run_status(&parent.id, TerminalRunStatus::Starting, None, None, None)
        .unwrap();
    repository
        .set_run_status(&parent.id, TerminalRunStatus::Running, None, None, None)
        .unwrap();
    let parent = repository
        .set_run_status(
            &parent.id,
            TerminalRunStatus::Completed,
            Some(0),
            None,
            None,
        )
        .unwrap();
    let child = TerminalRun {
        parent_run_id: Some(parent.id.clone()),
        ..parent.clone()
    };
    // A session recorded before V63 (fingerprint v1) refuses any change.
    assert_eq!(
        repository.session_fingerprint_version(&parent.id).unwrap(),
        1
    );
    let workspace =
        |hash: &str| SafeCliWorkspace::from_directory(tempfile::tempdir().unwrap(), hash.into());
    let validate = |run: &TerminalRun, hash: &str, session: &str| {
        validate_resume_parent(&repository, run, &parent, &workspace(hash), Some(session))
    };
    validate(&child, "workspace-a", "session-a").unwrap();
    assert!(validate(&child, "workspace-b", "session-a").is_err());
    assert!(validate(&child, "workspace-a", "session-b").is_err());
    let mut changed = child.clone();
    changed.model = Some("other-model".into());
    assert!(validate(&changed, "workspace-a", "session-a").is_err());
    changed = child;
    changed.backend = Some(CliBackend::Claude);
    assert!(validate(&changed, "workspace-a", "session-a").is_err());
}

fn claude_frame(value: Value) -> ProcessStreamItem {
    ProcessStreamItem::Frame {
        stream: "stdout",
        bytes: value.to_string().into_bytes(),
        received_at: Instant::now(),
    }
}

fn text_delta(text: &str) -> ProcessStreamItem {
    claude_frame(serde_json::json!({"type":"stream_event","session_id":"s-1",
        "event":{"type":"content_block_delta","index":0,
            "delta":{"type":"text_delta","text":text}}}))
}

fn message_start() -> ProcessStreamItem {
    claude_frame(serde_json::json!({"type":"stream_event","session_id":"s-1",
        "event":{"type":"message_start",
            "message":{"id":"m-1","model":"claude-haiku-4-5-20251001"}}}))
}

/// "Haiku CLI teste 1" (2026-09-26): a Claude Test run persisted 12,166
/// events, one per stream frame, and its end trailed the process by 7.8 s.
#[tokio::test]
async fn claude_stream_deltas_share_rows_in_order_around_each_generation() {
    let word_count = 40;
    let protocol_delta_count = 20;
    let (_data, repository, mut run) = audit_test_run();
    run.model = Some("haiku".into());
    let run_id = run.id.clone();
    let (sender, receiver) = mpsc::channel(128);
    sender
        .send(claude_frame(
            serde_json::json!({"type":"system","subtype":"init",
            "session_id":"s-1","model":"claude-haiku-4-5-20251001"}),
        ))
        .await
        .unwrap();
    sender.send(message_start()).await.unwrap();
    for word in 0..word_count {
        sender.send(text_delta(&format!("w{word} "))).await.unwrap();
    }
    for part in 0..protocol_delta_count {
        sender
            .send(claude_frame(
                serde_json::json!({"type":"stream_event","session_id":"s-1",
                "event":{"type":"content_block_delta","index":1,
                    "delta":{"type":"input_json_delta","partial_json":format!("{{\"k{part}\":")}}}),
            ))
            .await
            .unwrap();
    }
    sender
        .send(claude_frame(
            serde_json::json!({"type":"result","subtype":"success",
            "session_id":"s-1","result":"final answer"}),
        ))
        .await
        .unwrap();
    drop(sender);
    let summary = tokio::task::spawn_blocking({
        let repository = repository.clone();
        move || {
            coordinate_process_events_blocking(
                repository,
                Arc::new(CollectTerminalEvents::default()),
                run,
                CliBackend::Claude,
                receiver,
                CancellationToken::new(),
                Arc::new(Mutex::new(None)),
                None,
                None,
            )
        }
    })
    .await
    .unwrap()
    .unwrap();
    assert_eq!(summary.raw_output, "final answer");
    assert_eq!(summary.verified_model_generation_count, 1);
    let events = repository.list_events(&run_id, 0, 1000).unwrap();
    // Wall-clock flushes may split a batch while SQLite or the scheduler is busy.
    // Check coalescing and logical boundaries independently of that timing.
    assert!(
        events
            .windows(2)
            .all(|pair| pair[0].sequence < pair[1].sequence)
    );
    assert!(events.len() >= 5, "{events:#?}");
    assert_eq!(events[0].kind, TerminalEventKind::System);
    assert_eq!(events[0].content, "CLI system received.");
    assert_eq!(events[1].content, "CLI message received.");
    assert_eq!(events[1].kind, TerminalEventKind::Message);
    let middle = &events[2..events.len() - 1];
    let protocol_start = middle
        .iter()
        .position(|event| event.content == "CLI message received.")
        .expect("protocol deltas must retain their metadata boundary");
    let (word_events, protocol_events) = middle.split_at(protocol_start);
    assert!(!word_events.is_empty());
    assert!(
        word_events.len() < word_count,
        "text deltas were not coalesced"
    );
    assert!(
        protocol_events.len() < protocol_delta_count,
        "protocol deltas were not coalesced"
    );
    assert!(
        middle
            .iter()
            .all(|event| event.kind == TerminalEventKind::Message
                && event.stream.as_deref() == Some("stdout"))
    );
    let words = (0..word_count)
        .map(|word| format!("w{word} "))
        .collect::<String>();
    assert_eq!(
        word_events
            .iter()
            .map(|event| event.content.as_str())
            .collect::<String>(),
        words
    );
    assert!(
        protocol_events
            .iter()
            .all(|event| event.content == "CLI message received.")
    );
    assert!(
        events
            .iter()
            .all(|event| !event.content.contains("input_json_delta")
                && !event.content.contains("partial_json"))
    );
    let completed = events.last().expect("final completion event");
    assert_eq!(completed.kind, TerminalEventKind::Completed);
    assert_eq!(completed.content, "final answer");
    let generation: (String, String, u64) = repository
        .connection()
        .unwrap()
        .query_row(
            "SELECT message_id,model_id,event_sequence FROM terminal_model_observations
         WHERE run_id=?1 AND is_generation=1",
            [&run_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        generation,
        (
            "m-1".into(),
            "claude-haiku-4-5-20251001".into(),
            events[1].sequence
        )
    );
    let stored = repository.get_run(&run_id).unwrap();
    assert_eq!(stored.native_session_id.as_deref(), Some("s-1"));
    assert_eq!(stored.event_count, events.len() as u64);
}

/// A pause must never publish a prefix that later becomes a secret.
#[tokio::test]
async fn buffered_deltas_wait_for_completion_and_redact_across_timer_windows() {
    let (_data, repository, mut run) = audit_test_run();
    run.model = Some("haiku".into());
    let run_id = run.id.clone();
    let (sender, receiver) = mpsc::channel(16);
    let audit = tokio::task::spawn_blocking({
        let repository = repository.clone();
        move || {
            coordinate_process_events_blocking(
                repository,
                Arc::new(CollectTerminalEvents::default()),
                run,
                CliBackend::Claude,
                receiver,
                CancellationToken::new(),
                Arc::new(Mutex::new(None)),
                None,
                None,
            )
        }
    });
    sender.send(message_start()).await.unwrap();
    sender
        .send(text_delta("Authorization: Bearer synthetic-"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(350)).await;
    assert!(repository.list_events(&run_id, 0, 100).unwrap().is_empty());
    sender.send(text_delta("split-secret-value")).await.unwrap();
    drop(sender);
    let summary = audit.await.unwrap().unwrap();
    assert_eq!(
        summary.raw_output,
        "Authorization: Bearer synthetic-split-secret-value"
    );
    assert!(!format!("{summary:?}").contains("synthetic-split-secret-value"));
    let events = repository.list_events(&run_id, 0, 100).unwrap();
    assert!(
        events
            .iter()
            .all(|event| !event.content.contains("synthetic-")
                && !event.content.contains("split-secret-value"))
    );
}

fn audit_test_run() -> (tempfile::TempDir, TerminalRepository, TerminalRun) {
    use crate::test_support::TestDatabase;
    let data = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let persistence = TestDatabase::initialize(data.path()).unwrap();
    let project = persistence
        .insert_project(
            workspace.path().to_string_lossy().into_owned(),
            Some("Background audit fixture".into()),
        )
        .unwrap();
    let repository = test_repository(persistence.database_path());
    repository
        .ensure_project_configs(&project.id, true)
        .unwrap();
    let run = repository
        .create_run(CreateTerminalRunRequest {
            project_id: project.id,
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
        .unwrap();
    (data, repository, run)
}

#[tokio::test]
async fn codex_model_less_item_uses_current_turn_context_and_audits_the_source() {
    let (_data, repository, run) = audit_test_run();
    let sessions = tempfile::tempdir().unwrap();
    let day = sessions.path().join("2026/09/23");
    std::fs::create_dir_all(&day).unwrap();
    let session_id = Uuid::new_v4().to_string();
    let turn_id = Uuid::new_v4().to_string();
    let started_at = Utc::now().to_rfc3339();
    let rollout = [
        serde_json::json!({"timestamp":started_at,"type":"session_meta","payload":{"id":session_id}}),
        serde_json::json!({"timestamp":started_at,"type":"event_msg","payload":{"type":"task_started","turn_id":turn_id}}),
        serde_json::json!({"timestamp":started_at,"type":"turn_context","payload":{"turn_id":turn_id,"model":"fixture"}}),
    ];
    std::fs::write(
        day.join(format!("rollout-fixture-{session_id}.jsonl")),
        rollout
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    let (sender, receiver) = mpsc::channel(4);
    for frame in [
        serde_json::json!({"type":"thread.started","thread_id":session_id}),
        serde_json::json!({"type":"turn.started"}),
        serde_json::json!({"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"ok"}}),
    ] {
        sender
            .send(ProcessStreamItem::Frame {
                stream: "stdout",
                bytes: frame.to_string().into_bytes(),
                received_at: Instant::now(),
            })
            .await
            .unwrap();
    }
    drop(sender);
    let repository_for_audit = repository.clone();
    let run_id = run.id.clone();
    let root = sessions.path().to_path_buf();
    let summary = tokio::task::spawn_blocking(move || {
        coordinate_process_events_blocking(
            repository_for_audit,
            Arc::new(CollectTerminalEvents::default()),
            run,
            CliBackend::Codex,
            receiver,
            CancellationToken::new(),
            Arc::new(Mutex::new(None)),
            Some(root),
            None,
        )
    })
    .await
    .unwrap()
    .unwrap();
    assert_eq!(summary.raw_output, "ok");
    assert_eq!(summary.verified_model_generation_count, 1);
    let observation: (Option<String>, String, Option<String>) = repository
        .connection()
        .unwrap()
        .query_row(
            "SELECT model_id,evidence_source,message_id FROM terminal_model_observations WHERE run_id=?1 AND is_generation=1",
            [&run_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(observation.0.as_deref(), Some("fixture"));
    assert_eq!(observation.1, "codex_turn_context");
    assert_eq!(observation.2.as_deref(), Some("item_0"));
}

#[tokio::test]
async fn codex_turn_context_mismatch_rejects_the_item_before_output() {
    let (_data, repository, run) = audit_test_run();
    let sessions = tempfile::tempdir().unwrap();
    let day = sessions.path().join("2026/09/23");
    std::fs::create_dir_all(&day).unwrap();
    let session_id = Uuid::new_v4().to_string();
    let turn_id = Uuid::new_v4().to_string();
    let started_at = Utc::now().to_rfc3339();
    let rollout = [
        serde_json::json!({"timestamp":started_at,"type":"session_meta","payload":{"id":session_id}}),
        serde_json::json!({"timestamp":started_at,"type":"event_msg","payload":{"type":"task_started","turn_id":turn_id}}),
        serde_json::json!({"timestamp":started_at,"type":"turn_context","payload":{"turn_id":turn_id,"model":"different-model"}}),
    ];
    std::fs::write(
        day.join(format!("rollout-fixture-{session_id}.jsonl")),
        rollout
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    let (sender, receiver) = mpsc::channel(4);
    for frame in [
        serde_json::json!({"type":"thread.started","thread_id":session_id}),
        serde_json::json!({"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"must-not-publish"}}),
    ] {
        sender
            .send(ProcessStreamItem::Frame {
                stream: "stdout",
                bytes: frame.to_string().into_bytes(),
                received_at: Instant::now(),
            })
            .await
            .unwrap();
    }
    drop(sender);
    let run_id = run.id.clone();
    let abort = CancellationToken::new();
    let result = tokio::task::spawn_blocking({
        let root = sessions.path().to_path_buf();
        let abort = abort.clone();
        let repository = repository.clone();
        move || {
            coordinate_process_events_blocking(
                repository,
                Arc::new(CollectTerminalEvents::default()),
                run,
                CliBackend::Codex,
                receiver,
                abort,
                Arc::new(Mutex::new(None)),
                Some(root),
                None,
            )
        }
    })
    .await
    .unwrap();
    assert_eq!(result.unwrap_err().code(), "model_mismatch");
    assert!(abort.is_cancelled());
    let observation: (Option<String>, String) = repository
        .connection()
        .unwrap()
        .query_row(
            "SELECT model_id,evidence_source FROM terminal_model_observations WHERE run_id=?1 AND is_generation=1 ORDER BY ordinal LIMIT 1",
            [&run_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(observation.0.as_deref(), Some("different-model"));
    assert_eq!(observation.1, "codex_turn_context");
    assert!(
        !repository
            .list_events(&run_id, 0, 100)
            .unwrap()
            .iter()
            .any(|event| event.content.contains("must-not-publish"))
    );
}

#[tokio::test]
async fn audit_waits_for_sqlite_without_blocking_runtime_or_replaying_heartbeats() {
    let (_data, repository, run) = audit_test_run();
    let id = run.id.clone();
    let bus = Arc::new(FixtureEventBus::new(2));
    let mut unobserved = bus.subscribe();
    let (sender, receiver) = mpsc::channel(16);
    for index in 0..12 {
        let bytes = if index == 0 {
            br#"{"type":"hvc_model_attestation_v1","model":"fixture","messageId":"audit-generation"}"#.to_vec()
        } else {
            format!("frame {index}\n").into_bytes()
        };
        sender
            .send(ProcessStreamItem::Frame {
                stream: "stdout",
                bytes,
                received_at: Instant::now() - Duration::from_secs(1),
            })
            .await
            .unwrap();
    }
    drop(sender);
    let watchdog = ProgressWatchdog::new(AgentTimeoutPolicy::default());
    let db = repository.connection().unwrap();
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    let task = tokio::spawn(coordinate_process_events(
        repository.clone(),
        bus,
        run,
        CliBackend::Custom,
        receiver,
        CancellationToken::new(),
        Arc::new(Mutex::new(None)),
        watchdog.clone(),
        None,
    ));
    // On a single-thread runtime the old synchronous coordinator blocked
    // this release for the entire SQLite busy timeout and failed the run.
    tokio::time::sleep(Duration::from_millis(100)).await;
    db.execute_batch("COMMIT").unwrap();
    let result = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        watchdog.progress_count(),
        0,
        "old queued output is not new process activity"
    );
    assert!(result.max_queue_delay_ms >= 1_000);
    assert_eq!(
        repository.list_events(&id, 0, 100).unwrap().len(),
        11,
        "the model attestation is persisted in the audit table, not as visible output"
    );
    assert!(matches!(
        unobserved.recv().await,
        Err(tokio::sync::broadcast::error::RecvError::Lagged(_))
    ));
}

#[tokio::test]
async fn audit_failure_signals_process_abort_without_an_observer() {
    let (_data, repository, run) = audit_test_run();
    repository
        .connection()
        .unwrap()
        .execute_batch("DROP TABLE terminal_events")
        .unwrap();
    let (sender, receiver) = mpsc::channel(1);
    drop(sender);
    let abort = CancellationToken::new();
    let result = coordinate_process_events(
        repository,
        Arc::new(FixtureEventBus::new(2)),
        run,
        CliBackend::Custom,
        receiver,
        abort.clone(),
        Arc::new(Mutex::new(None)),
        ProgressWatchdog::new(AgentTimeoutPolicy::default()),
        None,
    )
    .await;
    assert!(result.is_err());
    assert!(abort.is_cancelled());
}

#[test]
fn a_cli_call_may_spend_twice_its_reservation_and_never_less_than_the_floor() {
    // Small asks (intake 1,600, specification 8,000) are governed by the
    // floor: their successful runs reached 4,714 and 16,701 tokens.
    assert_eq!(cli_output_ceiling(1_600), 24_000);
    assert_eq!(cli_output_ceiling(8_000), 24_000);
    // The test author asked for 16,000 and a healthy sibling spent 23,175.
    assert_eq!(cli_output_ceiling(16_000), 32_000);
    assert_eq!(cli_output_ceiling(u32::MAX), u64::from(u32::MAX) * 2);
}

fn claude_message(
    id: &str,
    input: u64,
    cached: u64,
    output: Option<u64>,
) -> Vec<ProcessStreamItem> {
    let mut frames = vec![claude_frame(serde_json::json!({
        "type":"stream_event","session_id":"s-1",
        "event":{"type":"message_start","message":{
            "id":id,"model":"claude-haiku-4-5-20251001",
            "usage":{"input_tokens":input,"cache_read_input_tokens":cached,
                "cache_creation_input_tokens":0,"output_tokens":1}}}
    }))];
    if let Some(output) = output {
        frames.push(claude_frame(serde_json::json!({
            "type":"stream_event","session_id":"s-1",
            "event":{"type":"message_delta","usage":{"output_tokens":output}}
        })));
    }
    frames
}

async fn run_claude_stream_against_ceiling(
    frames: Vec<ProcessStreamItem>,
    ceiling: Option<u64>,
) -> (
    Result<ProcessEventSummary, TerminalError>,
    CancellationToken,
    Arc<Mutex<Option<ProcessAbortReason>>>,
    TerminalRepository,
    String,
    tempfile::TempDir,
) {
    let (data, repository, mut run) = audit_test_run();
    run.model = Some("haiku".into());
    let run_id = run.id.clone();
    let (sender, receiver) = mpsc::channel(512);
    for frame in frames {
        sender.send(frame).await.unwrap();
    }
    drop(sender);
    let abort = CancellationToken::new();
    let reason = Arc::new(Mutex::new(None));
    let result = tokio::task::spawn_blocking({
        let (repository, abort, reason) = (repository.clone(), abort.clone(), reason.clone());
        move || {
            coordinate_process_events_blocking(
                repository,
                Arc::new(CollectTerminalEvents::default()),
                run,
                CliBackend::Claude,
                receiver,
                abort,
                reason,
                None,
                ceiling,
            )
        }
    })
    .await
    .unwrap();
    (result, abort, reason, repository, run_id, data)
}

#[tokio::test]
async fn claude_synthetic_auth_failure_is_audited_without_a_model_mismatch() {
    let frames = vec![
        claude_frame(serde_json::json!({
            "type":"system","subtype":"init","model":"claude-haiku-4-5-20251001"
        })),
        claude_frame(serde_json::json!({
            "type":"assistant","error":"authentication_failed","is_api_error_message":true,
            "message":{"id":"auth-error","model":"<synthetic>","content":[{
                "type":"text","text":"Failed to authenticate: OAuth session expired and could not be refreshed"
            }]}
        })),
    ];
    let (result, abort, _, repository, run_id, _data) =
        run_claude_stream_against_ceiling(frames, None).await;
    assert_eq!(result.unwrap_err().code(), "cli_auth_required");
    assert!(abort.is_cancelled());
    assert_eq!(repository.get_run(&run_id).unwrap().observed_model, None);
    let events = repository.list_events(&run_id, 0, 100).unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.kind == TerminalEventKind::Error
                && event.content.contains("requires authentication"))
    );
    let db = repository.connection().unwrap();
    let count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM terminal_model_observations WHERE run_id=?1 AND is_generation=1",
            [&run_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn claude_error_result_cannot_be_accepted_after_a_verified_generation() {
    let mut frames = claude_message("m-1", 8, 0, Some(5));
    frames.push(claude_frame(serde_json::json!({
        "type":"result","is_error":true,"result":"API Error: Rate limit reached"
    })));
    let (result, abort, ..) = run_claude_stream_against_ceiling(frames, None).await;
    assert_eq!(result.unwrap_err().code(), "cli_rate_limited");
    assert!(abort.is_cancelled());
}

#[tokio::test]
async fn claude_synthetic_error_diagnostics_are_redacted_before_audit_and_return() {
    let secret = "fixture-secret-1234567890";
    let frames = vec![claude_frame(serde_json::json!({
        "type":"assistant","is_api_error_message":true,
        "message":{"model":"<synthetic>","content":[{
            "type":"text","text":format!("API Error: Authorization: Bearer {secret}")
        }]}
    }))];
    let (result, abort, _, repository, run_id, _data) =
        run_claude_stream_against_ceiling(frames, None).await;
    let error = result.unwrap_err();
    assert_eq!(error.code(), "cli_process_failed");
    assert!(!error.to_string().contains(secret));
    assert!(error.to_string().contains("incomplete output was withheld"));
    assert!(abort.is_cancelled());
    let events = repository.list_events(&run_id, 0, 100).unwrap();
    assert!(events.iter().all(|event| !event.content.contains(secret)));
}

#[tokio::test]
async fn claude_synthetic_success_and_marked_real_model_mismatch_still_fail_closed() {
    for (model, error) in [("<synthetic>", false), ("claude-sonnet-5", true)] {
        let frames = vec![claude_frame(serde_json::json!({
            "type":"assistant","is_api_error_message":error,
            "message":{"id":"m-1","model":model,"content":[{"type":"text","text":"answer"}]}
        }))];
        let (result, abort, ..) = run_claude_stream_against_ceiling(frames, None).await;
        assert_eq!(result.unwrap_err().code(), "model_mismatch");
        assert!(abort.is_cancelled());
    }
}

/// "Haiku CLI teste 1" (2026-09-26): seven messages of 6,955, 393, 9,438,
/// 9,284, 6,554, 6,619 and 5,111 output tokens (44,354 in all) against a
/// 16,000-token reservation. The host must end the run while it is still
/// producing, keep what it had spent, and not touch a run that stays under.
#[tokio::test]
async fn a_claude_run_past_its_output_ceiling_is_stopped_and_its_partial_usage_kept() {
    let outputs = [6_955, 393, 9_438, 9_284, 6_554, 6_619, 5_111];
    let mut frames = vec![claude_frame(serde_json::json!({
        "type":"system","subtype":"init","session_id":"s-1",
        "model":"claude-haiku-4-5-20251001"}))];
    for (index, output) in outputs.into_iter().enumerate() {
        // The stale `assistant` echo must not count for more than it says.
        frames.extend(claude_message(
            &format!("m-{index}"),
            8,
            20_000,
            Some(output),
        ));
        frames.push(claude_frame(serde_json::json!({
            "type":"assistant","session_id":"s-1",
            "message":{"id":format!("m-{index}"),"model":"claude-haiku-4-5-20251001",
                "content":[],"usage":{"output_tokens":2}}})));
    }
    let (result, abort, reason, repository, run_id, _data) =
        run_claude_stream_against_ceiling(frames, Some(32_000)).await;

    // 6,955 + 393 + 9,438 + 9,284 = 26,070; the fifth message closes at 32,624.
    match result.unwrap_err() {
        TerminalError::OutputBudgetExceeded { observed, ceiling } => {
            assert_eq!((observed, ceiling), (32_624, 32_000));
        }
        other => panic!("unexpected error: {other}"),
    }
    assert!(abort.is_cancelled(), "the process must be told to die");
    assert!(matches!(
        *reason.lock().unwrap(),
        Some(ProcessAbortReason::OutputBudget)
    ));
    let db = repository.connection().unwrap();
    let (input, output, quality, key_count): (i64, i64, String, i64) = db
        .query_row(
            "SELECT input_tokens,output_tokens,quality,(SELECT COUNT(*) FROM token_usage_records WHERE source_id=?1)
             FROM token_usage_records WHERE source_kind='terminal_run' AND source_id=?1",
            [&run_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        key_count, 1,
        "the empty placeholder is replaced, not joined"
    );
    assert_eq!(output, 32_624);
    assert_eq!(input, 5 * (8 + 20_000));
    assert_eq!(quality, "partial");
}

#[tokio::test]
async fn a_claude_message_still_streaming_counts_by_estimate_and_ends_the_run_mid_message() {
    let mut frames = claude_message("m-1", 8, 0, Some(900));
    frames.extend(claude_message("m-2", 8, 0, None));
    // 4,000 characters estimate 1,000 tokens: 900 + 1,000 passes 1,500
    // before the second message ever closes.
    for _ in 0..10 {
        frames.push(text_delta(&"x".repeat(400)));
    }
    let (result, _, _, _, _, _data) = run_claude_stream_against_ceiling(frames, Some(1_500)).await;
    assert!(matches!(
        result,
        Err(TerminalError::OutputBudgetExceeded { ceiling: 1_500, .. })
    ));
}

#[tokio::test]
async fn a_run_that_stays_under_its_ceiling_or_has_none_is_left_alone() {
    let build = || {
        let mut frames = claude_message("m-1", 8, 0, Some(900));
        frames.extend(claude_message("m-2", 8, 0, Some(900)));
        frames.push(claude_frame(serde_json::json!({
            "type":"result","subtype":"success","session_id":"s-1","result":"done"})));
        frames
    };
    let (under, abort, ..) = run_claude_stream_against_ceiling(build(), Some(1_800)).await;
    assert_eq!(under.unwrap().raw_output, "done");
    assert!(!abort.is_cancelled());
    let (unbounded, abort, ..) = run_claude_stream_against_ceiling(build(), None).await;
    assert_eq!(unbounded.unwrap().raw_output, "done");
    assert!(!abort.is_cancelled());
}

#[test]
fn transport_read_clock_keeps_the_longest_gap_including_the_tail() {
    let clock = TransportReadClock::new();
    let start = clock.state.lock().unwrap().0;
    // A read 30 ms in, another right after it, then 80 ms of silence to
    // the exit: the tail is the longest gap.
    std::thread::sleep(Duration::from_millis(30));
    clock.record_read();
    clock.record_read();
    let (last, gap) = *clock.state.lock().unwrap();
    assert!(gap >= 30, "{gap}");
    let silence = clock.max_silence_ms(last + Duration::from_millis(80));
    assert!((80..200).contains(&silence), "{silence}");
    // An exit stamped before the last read never goes negative.
    assert_eq!(clock.max_silence_ms(start), gap);
}

#[tokio::test]
async fn transport_activity_and_reasoning_do_not_depend_on_audit_consumption() {
    let watchdog = ProgressWatchdog::new(AgentTimeoutPolicy::default());
    let (sender, mut receiver) = mpsc::channel(4);
    let read_clock = Arc::new(TransportReadClock::new());
    read_process_stream(
        "stdout",
        std::io::Cursor::new(b"{\"type\":\"step_start\"}\n"),
        sender.clone(),
        Some(watchdog.clone()),
        CliBackend::Opencode,
        Arc::clone(&read_clock),
    )
    .await;
    read_process_stream(
        "stderr",
        std::io::Cursor::new(b"connection bookkeeping\n"),
        sender.clone(),
        Some(watchdog.clone()),
        CliBackend::Opencode,
        Arc::clone(&read_clock),
    )
    .await;
    assert_eq!(
        watchdog.progress_count(),
        2,
        "one heartbeat per actual read"
    );
    assert!(watchdog.diagnostic().contains("reasoning=true"));
    assert!(matches!(
        receiver.try_recv().unwrap(),
        ProcessStreamItem::Frame {
            stream: "stdout",
            ..
        }
    ));
    observe_received_frame(
        &watchdog,
        CliBackend::Opencode,
        "stdout",
        br#"{"type":"text","part":{"text":"answer"}}"#,
    );
    assert!(watchdog.diagnostic().contains("reasoning=false"));
    assert_eq!(
        watchdog.progress_count(),
        2,
        "semantic transitions are not extra reads"
    );
}

#[tokio::test]
async fn probe_reader_aborts_when_output_exceeds_the_bound() {
    let abort = CancellationToken::new();
    let error = read_bounded_probe_stream(
        std::io::Cursor::new(vec![b'x'; MAX_PROBE_OUTPUT_BYTES + 1]),
        abort.clone(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(abort.is_cancelled());
}

#[tokio::test]
async fn event_coordinator_persists_one_monotonic_stream_with_run_snapshots() {
    use crate::test_support::TestDatabase;

    let data = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let persistence = TestDatabase::initialize(data.path()).unwrap();
    let project = persistence
        .insert_project(
            workspace.path().to_string_lossy().into_owned(),
            Some("Agents stream".into()),
        )
        .unwrap();
    let repository = test_repository(persistence.database_path());
    repository
        .ensure_project_configs(&project.id, true)
        .unwrap();
    let run = repository
        .create_run(CreateTerminalRunRequest {
            project_id: project.id,
            agent_role: AgentRole::BackendAgent,
            origin: ExecutionOrigin::Manual,
            execution_mode: AgentExecutionMode::CliProcess,
            backend: Some(CliBackend::Codex),
            model: Some("gpt-5.6-luna".into()),
            pulsation_id: None,
            job_id: None,
            agent_run_id: None,
            llm_execution_id: None,
        })
        .unwrap();
    repository
        .set_run_status(&run.id, TerminalRunStatus::Starting, None, None, None)
        .unwrap();
    let run = repository
        .set_run_status(&run.id, TerminalRunStatus::Running, None, None, None)
        .unwrap();
    let run_id = run.id.clone();
    let collected = Arc::new(CollectTerminalEvents::default());
    let sink: Arc<dyn TerminalEventSink> = collected.clone();
    let (sender, receiver) = mpsc::channel(PROCESS_EVENT_CHANNEL_CAPACITY);
    sender
        .send(ProcessStreamItem::Frame {
            received_at: Instant::now(),
            stream: "stdout",
            bytes: br#"{"type":"item.completed","model":"gpt-5.6-luna","item":{"id":"msg-1","type":"agent_message","text":"first\n"}}"#.to_vec(),
        })
        .await
        .unwrap();
    sender
        .send(ProcessStreamItem::Frame {
            received_at: Instant::now(),
            stream: "stderr",
            bytes: b"warning\n".to_vec(),
        })
        .await
        .unwrap();
    drop(sender);

    let output = coordinate_process_events(
        repository.clone(),
        sink,
        run,
        CliBackend::Codex,
        receiver,
        CancellationToken::new(),
        Arc::new(Mutex::new(None)),
        ProgressWatchdog::new(AgentTimeoutPolicy::default()),
        None,
    )
    .await
    .unwrap();
    assert_eq!(output.raw_output, "first\n");
    assert_eq!(output.last_stderr, "warning\n");
    let events = repository.list_events(&run_id, 0, 20).unwrap();
    assert_eq!(
        events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    let persisted_run = repository.get_run(&run_id).unwrap();
    assert_eq!(persisted_run.event_count, 2);
    assert_eq!(persisted_run.output_bytes, 14);
    let envelopes = collected
        .envelopes
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(
        envelopes
            .iter()
            .map(|envelope| envelope.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert!(
        envelopes
            .iter()
            .all(|envelope| envelope.protocol_version == 2
                && envelope.run.status == TerminalRunStatus::Running)
    );
}

#[tokio::test]
async fn coordinate_process_events_does_not_overwrite_raw_output_with_non_text_completed_frame() {
    use crate::test_support::TestDatabase;

    let data = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let persistence = TestDatabase::initialize(data.path()).unwrap();
    let project = persistence
        .insert_project(
            workspace.path().to_string_lossy().into_owned(),
            Some("Agents stream non-text".into()),
        )
        .unwrap();
    let repository = test_repository(persistence.database_path());
    repository
        .ensure_project_configs(&project.id, true)
        .unwrap();
    let run = repository
        .create_run(CreateTerminalRunRequest {
            project_id: project.id,
            agent_role: AgentRole::BackendAgent,
            origin: ExecutionOrigin::Manual,
            execution_mode: AgentExecutionMode::CliProcess,
            backend: Some(CliBackend::Codex),
            model: Some("gpt-5.6-luna".into()),
            pulsation_id: None,
            job_id: None,
            agent_run_id: None,
            llm_execution_id: None,
        })
        .unwrap();
    repository
        .set_run_status(&run.id, TerminalRunStatus::Starting, None, None, None)
        .unwrap();
    let run = repository
        .set_run_status(&run.id, TerminalRunStatus::Running, None, None, None)
        .unwrap();
    let collected = Arc::new(CollectTerminalEvents::default());
    let sink: Arc<dyn TerminalEventSink> = collected.clone();
    let (sender, receiver) = mpsc::channel(PROCESS_EVENT_CHANNEL_CAPACITY);
    sender
        .send(ProcessStreamItem::Frame {
            received_at: Instant::now(),
            stream: "stdout",
            bytes: br#"{"type":"item.completed","model":"gpt-5.6-luna","item":{"id":"msg-2","type":"agent_message","text":"{\"outputLanguage\":\"en\",\"message\":\"ok\"}"}}"#.to_vec(),
        })
        .await
        .unwrap();
    sender
        .send(ProcessStreamItem::Frame {
            received_at: Instant::now(),
            stream: "stdout",
            bytes: br#"{"type":"turn.completed","usage":{"input_tokens":100}}"#.to_vec(),
        })
        .await
        .unwrap();
    drop(sender);

    let output = coordinate_process_events(
        repository.clone(),
        sink,
        run,
        CliBackend::Codex,
        receiver,
        CancellationToken::new(),
        Arc::new(Mutex::new(None)),
        ProgressWatchdog::new(AgentTimeoutPolicy::default()),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        output.raw_output,
        "{\"outputLanguage\":\"en\",\"message\":\"ok\"}"
    );
}

#[tokio::test]
async fn coordinate_process_events_accumulates_streamed_deltas_and_drops_the_prompt_echo() {
    use crate::test_support::TestDatabase;

    let data = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let persistence = TestDatabase::initialize(data.path()).unwrap();
    let project = persistence
        .insert_project(
            workspace.path().to_string_lossy().into_owned(),
            Some("Agents stream deltas".into()),
        )
        .unwrap();
    let repository = test_repository(persistence.database_path());
    repository
        .ensure_project_configs(&project.id, true)
        .unwrap();
    let run = repository
        .create_run(CreateTerminalRunRequest {
            project_id: project.id,
            agent_role: AgentRole::BackendAgent,
            origin: ExecutionOrigin::Manual,
            execution_mode: AgentExecutionMode::CliProcess,
            backend: Some(CliBackend::Gemini),
            model: Some("gemini-2.5-flash".into()),
            pulsation_id: None,
            job_id: None,
            agent_run_id: None,
            llm_execution_id: None,
        })
        .unwrap();
    repository
        .set_run_status(&run.id, TerminalRunStatus::Starting, None, None, None)
        .unwrap();
    let run = repository
        .set_run_status(&run.id, TerminalRunStatus::Running, None, None, None)
        .unwrap();
    let collected = Arc::new(CollectTerminalEvents::default());
    let sink: Arc<dyn TerminalEventSink> = collected.clone();
    let (sender, receiver) = mpsc::channel(PROCESS_EVENT_CHANNEL_CAPACITY);
    // The prompt is echoed back first with no `delta` marker - it must never
    // survive into raw_output once real assistant deltas start arriving.
    sender
        .send(ProcessStreamItem::Frame {
            received_at: Instant::now(),
            stream: "stdout",
            bytes: br#"{"type":"message","role":"user","content":"do the task"}"#.to_vec(),
        })
        .await
        .unwrap();
    sender
        .send(ProcessStreamItem::Frame {
            received_at: Instant::now(),
            stream: "stdout",
            bytes:
                br#"{"type":"message","role":"assistant","model":"gemini-2.5-flash","message_id":"gen-1","content":"{\"a\":1,","delta":true}"#
                    .to_vec(),
        })
        .await
        .unwrap();
    sender
        .send(ProcessStreamItem::Frame {
            received_at: Instant::now(),
            stream: "stdout",
            bytes:
                br#"{"type":"message","role":"assistant","model":"gemini-2.5-flash","message_id":"gen-1","content":"\"b\":2}","delta":true}"#
                    .to_vec(),
        })
        .await
        .unwrap();
    // The final `result` frame carries no visible text and must not clobber
    // the fragments already accumulated.
    sender
        .send(ProcessStreamItem::Frame {
            received_at: Instant::now(),
            stream: "stdout",
            bytes: br#"{"type":"result","status":"success","stats":{"input_tokens":1}}"#.to_vec(),
        })
        .await
        .unwrap();
    drop(sender);

    let output = coordinate_process_events(
        repository.clone(),
        sink,
        run,
        CliBackend::Gemini,
        receiver,
        CancellationToken::new(),
        Arc::new(Mutex::new(None)),
        ProgressWatchdog::new(AgentTimeoutPolicy::default()),
        None,
    )
    .await
    .unwrap();
    assert_eq!(output.raw_output, "{\"a\":1,\"b\":2}");
}

fn discovery_fixture(scan: usize) -> Vec<CliAvailability> {
    vec![CliAvailability {
        backend: CliBackend::Custom,
        state: CliAvailabilityState::Available,
        executable_path: None,
        version: Some(format!("fixture-{scan}")),
        diagnostic: None,
        install_url: "",
        install_command: "",
        capabilities: adapters::capabilities(CliBackend::Custom, None),
        quota: Vec::new(),
    }]
}

#[tokio::test]
async fn discovery_rescans_after_the_ttl_elapses_even_without_force() {
    use crate::test_support::TestDatabase;

    let data = tempfile::tempdir().unwrap();
    let persistence = TestDatabase::initialize(data.path()).unwrap();
    let sink: Arc<dyn TerminalEventSink> = Arc::new(CollectTerminalEvents::default());
    let service = test_service(
        persistence.database_path(),
        tokio::runtime::Handle::current(),
        sink,
    )
    .unwrap();

    let scans = AtomicUsize::new(0);
    let discover = || {
        let scan = scans.fetch_add(1, Ordering::SeqCst) + 1;
        std::future::ready(discovery_fixture(scan))
    };
    let first = service.discovery_with(false, &discover).await;
    assert_eq!(first, discovery_fixture(1));
    assert_eq!(scans.load(Ordering::SeqCst), 1);
    let stale_at = Instant::now()
        .checked_sub(CLI_DISCOVERY_CACHE_TTL + Duration::from_secs(1))
        .expect("test clock should support subtracting the TTL");
    {
        let mut guard = service
            .discovery_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard
            .as_mut()
            .expect("first call must populate the cache")
            .discovered_at = stale_at;
    }
    let second = service.discovery_with(false, &discover).await;
    assert_eq!(second, discovery_fixture(2));
    assert_eq!(scans.load(Ordering::SeqCst), 2);
    let refreshed_at = service
        .discovery_cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .unwrap()
        .discovered_at;
    assert!(
        refreshed_at > stale_at,
        "a cache older than the TTL must be rescanned even without force=true"
    );
}

#[tokio::test]
async fn discovery_reuses_a_still_fresh_cache_without_rescanning() {
    use crate::test_support::TestDatabase;

    let data = tempfile::tempdir().unwrap();
    let persistence = TestDatabase::initialize(data.path()).unwrap();
    let sink: Arc<dyn TerminalEventSink> = Arc::new(CollectTerminalEvents::default());
    let service = test_service(
        persistence.database_path(),
        tokio::runtime::Handle::current(),
        sink,
    )
    .unwrap();

    let scans = AtomicUsize::new(0);
    let discover = || {
        let scan = scans.fetch_add(1, Ordering::SeqCst) + 1;
        std::future::ready(discovery_fixture(scan))
    };
    let first = service.discovery_with(false, &discover).await;
    assert_eq!(first, discovery_fixture(1));
    let first_stamp = service
        .discovery_cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .unwrap()
        .discovered_at;
    let second = service.discovery_with(false, &discover).await;
    assert_eq!(second, first);
    assert_eq!(scans.load(Ordering::SeqCst), 1);
    let second_stamp = service
        .discovery_cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .unwrap()
        .discovered_at;
    assert_eq!(
        first_stamp, second_stamp,
        "a fresh cache must not trigger a rescan on every call"
    );
}

fn fixture_availability(backend: CliBackend, state: CliAvailabilityState) -> CliAvailability {
    let (install_url, install_command) = install_guidance(backend);
    CliAvailability {
        backend,
        state,
        executable_path: Some(PathBuf::from("stale-fixture-cli.exe")),
        version: Some("0.0.0-stale".into()),
        diagnostic: None,
        install_url,
        install_command,
        capabilities: adapters::capabilities(backend, None),
        quota: Vec::new(),
    }
}

fn stale_discovery_service(
    items: Vec<CliAvailability>,
) -> (TerminalService, Instant, tempfile::TempDir) {
    use crate::test_support::TestDatabase;
    let data = tempfile::tempdir().unwrap();
    let persistence = TestDatabase::initialize(data.path()).unwrap();
    let sink: Arc<dyn TerminalEventSink> = Arc::new(CollectTerminalEvents::default());
    let service = test_service(
        persistence.database_path(),
        tokio::runtime::Handle::current(),
        sink,
    )
    .unwrap();
    let stale_at = Instant::now()
        .checked_sub(CLI_DISCOVERY_CACHE_TTL + Duration::from_secs(1))
        .expect("test clock should support subtracting the TTL");
    store_cli_discovery(&service.discovery_cache, items);
    service
        .discovery_cache
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .discovered_at = stale_at;
    (service, stale_at, data)
}

fn discovered_at(service: &TerminalService) -> Instant {
    service
        .discovery_cache
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .discovered_at
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stale_snapshot_with_the_backend_available_launches_at_once_and_rescans_once() {
    let (service, stale_at, _data) = stale_discovery_service(vec![fixture_availability(
        CliBackend::Codex,
        CliAvailabilityState::Available,
    )]);
    let served = service.discovery_for_launch(CliBackend::Codex).await;
    assert_eq!(
        served[0].version.as_deref(),
        Some("0.0.0-stale"),
        "the launch is not blocked behind a probe of every CLI"
    );
    // A second launch while the rescan runs starts no second rescan.
    let again = service.discovery_for_launch(CliBackend::Codex).await;
    assert_eq!(again[0].version.as_deref(), Some("0.0.0-stale"));
    let deadline = Instant::now() + Duration::from_secs(60);
    while discovered_at(&service) == stale_at {
        assert!(Instant::now() < deadline, "background rescan never landed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    while service
        .discovery_refreshing
        .load(std::sync::atomic::Ordering::SeqCst)
    {
        assert!(
            Instant::now() < deadline,
            "single-flight flag never cleared"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        !service
            .discovery_cache
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .items
            .iter()
            .any(|item| item.version.as_deref() == Some("0.0.0-stale")),
        "the rescan replaced the stale snapshot"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stale_snapshot_without_the_backend_available_waits_for_a_fresh_scan() {
    let (service, stale_at, _data) = stale_discovery_service(vec![fixture_availability(
        CliBackend::Codex,
        CliAvailabilityState::AuthRequired,
    )]);
    let served = service.discovery_for_launch(CliBackend::Codex).await;
    assert!(
        served
            .iter()
            .all(|item| item.version.as_deref() != Some("0.0.0-stale")),
        "an unhealthy entry is never trusted from a stale snapshot"
    );
    assert!(discovered_at(&service) > stale_at);
    assert!(
        !service
            .discovery_refreshing
            .load(std::sync::atomic::Ordering::SeqCst)
    );
}

fn scoped_unit_request(executor_kind: &str, context_scope: UnitContextScope) -> LlmRequest {
    LlmRequest {
        execution_id: crate::host::ExecutionId("scope-execution".into()),
        project_id: "project".into(),
        pulsation_id: Some("pulsation".into()),
        job_id: None,
        agent_run_id: None,
        job_chain: "pulsation:scope".into(),
        target: ikore_contracts::AgentExecutionTarget::canonical(AgentRole::Integrator),
        stage: "Integrator".into(),
        system_prompt: "Read-only".into(),
        user_prompt: "unused".into(),
        response_format: ResponseFormat::Text,
        max_output_tokens: 100,
        deadline: std::time::Instant::now() + Duration::from_secs(30),
        dispatch: crate::host::TerminalDispatchPolicy {
            unit: Some(ikore_contracts::UnitDispatch {
                stage_execution_id: "execution".into(),
                unit_id: "unit".into(),
                attempt_id: "attempt".into(),
                executor_kind: executor_kind.into(),
                frozen_transport: "cli".into(),
                frozen_backend: Some("claude".into()),
                frozen_model: "fixture-model".into(),
                context_scope,
            }),
            ..crate::host::TerminalDispatchPolicy::default()
        },
    }
}

#[test]
fn a_composed_unit_runs_only_with_the_scope_its_specialty_requires() {
    // ADR-034 T1: the transport accepts exactly the scope the shared table
    // names for each specialty and refuses every other combination.
    let workspace_kinds = [
        "intake_classifier",
        "specification_author",
        "contract_author",
        "code_implementer",
        "code_repairer",
        "test_author",
        "test_repairer",
    ];
    for kind in workspace_kinds {
        let accepted = cli_context_scope(&scoped_unit_request(kind, UnitContextScope::Workspace));
        assert!(matches!(accepted, Ok(false)), "{kind}");
        let refused = cli_context_scope(&scoped_unit_request(
            kind,
            UnitContextScope::HostContextOnly,
        ))
        .unwrap_err()
        .to_string();
        assert!(refused.contains("workspace snapshot"), "{kind}: {refused}");
    }
    let reviewer = cli_context_scope(&scoped_unit_request(
        "integration_reviewer",
        UnitContextScope::HostContextOnly,
    ));
    assert!(matches!(reviewer, Ok(true)));
    let widened = cli_context_scope(&scoped_unit_request(
        "integration_reviewer",
        UnitContextScope::Workspace,
    ))
    .unwrap_err()
    .to_string();
    assert!(widened.contains("host-selected context only"), "{widened}");

    // Boundary: an unknown or mistyped specialty is never given a guessed scope.
    for unknown in ["", "implement_batch", "Integration_Reviewer"] {
        let error = cli_context_scope(&scoped_unit_request(unknown, UnitContextScope::Workspace))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("not a composed specialty"),
            "{unknown:?}: {error}"
        );
    }

    // Requests outside composition carry no unit and keep their own rules.
    let mut free_form = scoped_unit_request("code_implementer", UnitContextScope::Workspace);
    free_form.dispatch.unit = None;
    assert!(matches!(cli_context_scope(&free_form), Ok(false)));
}
