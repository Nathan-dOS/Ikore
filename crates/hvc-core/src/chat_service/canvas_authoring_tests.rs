use super::*;
use crate::chat_service::{
    CompanionControlEnvelope, CreateChatConversationRequest, NoopChatEventSink,
    companion_schema_for_backend, companion_tool_names, exceeds_remaining_read_budget,
    is_companion_tool, parse_cli_companion_envelope,
};
use crate::companion_drawings::CompanionDrawingsService;
use crate::companion_drawings::authoring::apply_canvas_commands;
use crate::companion_drawings::semantics::{CanvasDocumentV1, CanvasProfileV1};
use crate::llm::KeyringCredentialStore;
use crate::persistence_service::PersistenceService;
use tokio::runtime::Handle;

fn offer() -> CanvasOfferProposalV1 {
    CanvasOfferProposalV1 {
        profile: CanvasProfileV1::Architecture,
        language: "en".into(),
        title: "Local architecture".into(),
        request_text: "May I explain this using Canvas?".into(),
    }
}

#[test]
fn canvas_terminal_offer_is_explicit_closed_and_keeps_the_read_allowlist() {
    let arguments = serde_json::to_value(CanvasTerminalAnswerV1 {
        text: "UI calls Core.".into(),
        offer: offer(),
    })
    .unwrap();
    let response = ModelResponse {
        content: String::new(),
        tool_calls: vec![NormalizedToolCall {
            id: "terminal-a".into(),
            name: CANVAS_OFFER_CONTROL.into(),
            arguments: arguments.clone(),
        }],
        input_tokens: None,
        output_tokens: None,
        streamed: false,
    };
    assert_eq!(
        parse_terminal_offer(&response).unwrap().unwrap().text,
        "UI calls Core."
    );
    assert_eq!(companion_tool_names().len(), 10);
    assert!(!is_companion_tool(CANVAS_OFFER_CONTROL));
    for forbidden in ["approved", "project_id", "session_id", "drawing_id"] {
        let mut forged = response.clone();
        forged.tool_calls[0].arguments[forbidden] = json!(true);
        assert!(parse_terminal_offer(&forged).is_err());
    }
    let mut mixed = response.clone();
    mixed.tool_calls.push(NormalizedToolCall {
        id: "read-a".into(),
        name: "list_repository_tree".into(),
        arguments: json!({}),
    });
    assert!(parse_terminal_offer(&mixed).is_err());
    let prose = ModelResponse {
        content: "May I use Canvas?".into(),
        tool_calls: vec![],
        input_tokens: None,
        output_tokens: None,
        streamed: false,
    };
    assert!(parse_terminal_offer(&prose).unwrap().is_none());
}

#[test]
fn canvas_cli_terminal_variant_is_final_not_a_read_or_a_grant() {
    let payload = json!({"type":"final_with_canvas_offer","text":"UI calls Core.","offer":offer()});
    let envelope = parse_cli_companion_envelope(&payload.to_string(), CliBackend::Codex).unwrap();
    assert!(matches!(
        envelope,
        CompanionControlEnvelope::FinalWithCanvasOffer { .. }
    ));
    assert!(!exceeds_remaining_read_budget(&envelope, 0));
    let mut forged = payload;
    forged["approved"] = json!(true);
    assert!(parse_cli_companion_envelope(&forged.to_string(), CliBackend::Claude).is_err());
    for backend in [CliBackend::Codex, CliBackend::Claude, CliBackend::Gemini] {
        assert!(
            companion_schema_for_backend(backend)
                .to_string()
                .contains("final_with_canvas_offer")
        );
    }
}

fn architecture_scene() -> CanvasSceneV1 {
    CanvasSceneV1 {
        document: Some(CanvasDocumentV1 {
            schema_version: 1,
            profile: CanvasProfileV1::Architecture,
            language: "en".into(),
            narrative: String::new(),
            provenance: None,
            elements: vec![],
        }),
        ..Default::default()
    }
}

#[test]
fn canvas_wire_dsl_creates_typed_nodes_and_connections_without_writable_metadata() {
    let input = json!({"operation_id":"batch-a","expected_revision":0,"operations":[
        {"op":"upsert_node","id":"ui","kind":"component","label":"UI","x":0,"y":0},
        {"op":"upsert_node","id":"core","kind":"component","label":"Core","x":300,"y":0},
        {"op":"connect","id":"calls","kind":"relationship","source_id":"ui","target_id":"core","label":"Calls"}
    ]});
    let batch: CanvasWireBatchV1 = serde_json::from_value(input.clone()).unwrap();
    let scene = architecture_scene();
    let compiled = compile_batch(&scene, &batch).unwrap();
    let accepted = apply_canvas_commands(&scene, &compiled.commands).unwrap();
    assert_eq!(accepted.elements.len(), 3);
    assert_eq!(accepted.document.as_ref().unwrap().elements.len(), 3);
    let edge = accepted
        .elements
        .iter()
        .find(|element| element.id == "calls")
        .unwrap();
    assert!(matches!(
        edge.shape,
        CanvasElementKindV1::Arrow {
            start_binding: Some(_),
            end_binding: Some(_),
            ..
        }
    ));
    for field in [
        "profile",
        "language",
        "provenance",
        "project_id",
        "approved",
        "path",
    ] {
        let mut forged = input.clone();
        forged[field] = json!(true);
        assert!(serde_json::from_value::<CanvasWireBatchV1>(forged).is_err());
    }
    let mut wrong = input;
    wrong["operations"][2]["kind"] = json!("sequence_flow");
    let wrong: CanvasWireBatchV1 = serde_json::from_value(wrong).unwrap();
    assert!(
        apply_canvas_commands(&scene, &compile_batch(&scene, &wrong).unwrap().commands).is_err()
    );
    assert!(scene.elements.is_empty());
}

#[test]
fn canvas_authoring_schema_exposes_only_the_four_scoped_tools() {
    let schemas = canvas_tool_schemas();
    assert_eq!(
        schemas.iter().map(|(name, _, _)| *name).collect::<Vec<_>>(),
        vec![
            "canvas_inspect",
            "canvas_apply_operations",
            "canvas_validate",
            "canvas_finish"
        ]
    );
    for backend in [CliBackend::Codex, CliBackend::Claude, CliBackend::Gemini] {
        let schema = canvas_cli_schema(backend);
        assert!(!schema.to_string().contains("read_repository_file"));
        assert!(!schema.to_string().contains("shell"));
    }
}

struct CanvasApiFixture {
    _directory: tempfile::TempDir,
    persistence: PersistenceService,
    service: Arc<ChatService>,
    drawings: CompanionDrawingsService,
    conversation: String,
    request: CanvasAuthoringRequestV1,
}

impl CanvasApiFixture {
    fn new(endpoint: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let persistence = PersistenceService::initialize(directory.path()).unwrap();
        let workspace = directory.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let db = persistence.connection().unwrap();
        db.execute("INSERT INTO projects(id,name,workspace_path,is_archived,created_at,updated_at) VALUES('canvas-project','Canvas test',?1,0,'now','now')", [workspace.to_string_lossy()]).unwrap();
        db.execute("INSERT INTO provider_connections(id,provider_id,display_name,credential_ref,endpoint_url,default_model,enabled,state,max_concurrency,version,created_at,updated_at) VALUES('canvas-connection','ollama','Local fixture','none',?1,'qwen2.5-coder:7b',1,'active',1,1,'now','now')", [endpoint]).unwrap();
        let service = Arc::new(ChatService::with_http_client_and_runtime(
            persistence.database_path(),
            Arc::new(KeyringCredentialStore),
            reqwest::Client::builder().no_proxy().build().unwrap(),
            Arc::new(NoopChatEventSink),
            Handle::current(),
        ));
        let drawings = CompanionDrawingsService::new(persistence.clone());
        let store = CanvasAuthoringStore::new(drawings.clone());
        service.attach_canvas_authoring(store.clone()).unwrap();
        let conversation = service
            .create_conversation(CreateChatConversationRequest {
                project_id: "canvas-project".into(),
                target: ChatExecutionTargetRequest::Api {
                    connection_id: "canvas-connection".into(),
                },
            })
            .unwrap()
            .conversation
            .id;
        db.execute("INSERT INTO chat_messages(id,conversation_id,ordinal,role,content,status,created_at,updated_at) VALUES('canvas-source',?1,0,'assistant','The UI calls the Core service.','completed','now','now')", [&conversation]).unwrap();
        let request = store
            .propose(
                "canvas-project",
                &conversation,
                "canvas-source",
                &offer(),
                unix_now(),
            )
            .unwrap();
        Self {
            _directory: directory,
            persistence,
            service,
            drawings,
            conversation,
            request,
        }
    }

    fn approve(&self) -> CanvasAuthoringRequestV1 {
        self.service
            .approve_canvas_request(ApproveCanvasRequestV1 {
                project_id: "canvas-project".into(),
                conversation_id: self.conversation.clone(),
                request_id: self.request.request_id.clone(),
                expected_request_revision: 1,
                action_id: "fixture-approval".into(),
                expected_target: ChatExecutionTargetRequest::Api {
                    connection_id: "canvas-connection".into(),
                },
            })
            .unwrap()
    }

    fn get(&self) -> CanvasAuthoringRequestV1 {
        self.service
            .get_canvas_request(GetCanvasRequestV1 {
                project_id: "canvas-project".into(),
                conversation_id: self.conversation.clone(),
                request_id: self.request.request_id.clone(),
            })
            .unwrap()
    }
}

async fn receive_canvas_request(socket: &mut tokio::net::TcpStream) -> Value {
    use tokio::io::AsyncReadExt;
    let mut bytes = Vec::new();
    loop {
        let mut chunk = [0; 4096];
        let count = socket.read(&mut chunk).await.unwrap();
        assert!(count > 0, "request ended before its JSON body");
        bytes.extend_from_slice(&chunk[..count]);
        assert!(bytes.len() <= crate::secret_redactor::MAX_SANITIZATION_BYTES + 8192);
        if let Some(end) = bytes.windows(4).position(|value| value == b"\r\n\r\n") {
            let headers = std::str::from_utf8(&bytes[..end]).unwrap();
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().unwrap())
                })
                .unwrap();
            if bytes.len() >= end + 4 + length {
                return serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap();
            }
        }
    }
}

fn api_canvas_calls(calls: &[(&str, Value)]) -> Value {
    json!({"model":"qwen2.5-coder:7b", "choices":[{"message":{"role":"assistant","content":null,
        "tool_calls":calls.iter().enumerate().map(|(index,(name,args))|json!({"id":format!("call-{index}"),"type":"function","function":{"name":name,"arguments":args.to_string()}})).collect::<Vec<_>>()}}],
        "usage":{"prompt_tokens":12,"completion_tokens":7,"total_tokens":19}})
}

fn fixture_apply() -> Value {
    json!({"operation_id":"nodes","expected_revision":0,"operations":[
        {"op":"upsert_node","id":"ui","kind":"component","label":"UI","x":0,"y":0},
        {"op":"upsert_node","id":"core","kind":"component","label":"Core","x":300,"y":0},
        {"op":"connect","id":"calls","kind":"relationship","source_id":"ui","target_id":"core","label":"Calls"}
    ]})
}

#[tokio::test]
async fn canvas_api_authorization_generates_one_hidden_candidate_then_one_ready_drawing() {
    use tokio::io::AsyncWriteExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture = CanvasApiFixture::new(&format!("http://{}", listener.local_addr().unwrap()));
    assert!(fixture.drawings.list("canvas-project").unwrap().is_empty());
    assert_eq!(
        fixture.get().state,
        CanvasAuthoringRequestStateV1::AwaitingAuthorization
    );
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        for calls in [
            vec![("canvas_apply_operations", fixture_apply())],
            vec![
                ("canvas_validate", json!({})),
                (
                    "canvas_finish",
                    json!({"operation_id":"finish","expected_revision":1}),
                ),
            ],
        ] {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = receive_canvas_request(&mut socket).await;
            assert_eq!(request["model"], "qwen2.5-coder:7b");
            assert_eq!(request["tools"].as_array().unwrap().len(), 4);
            assert!(
                !request["tools"]
                    .to_string()
                    .contains("read_repository_file")
            );
            requests.push(request);
            let body = api_canvas_calls(&calls).to_string();
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        }
        requests
    });
    let running = fixture.approve();
    assert_eq!(running.state, CanvasAuthoringRequestStateV1::Running);
    tokio::time::timeout(
        Duration::from_secs(15),
        fixture.service.wait_for_canvas_idle(),
    )
    .await
    .unwrap();
    let requests = tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    let ready = fixture.get();
    assert_eq!(
        ready.state,
        CanvasAuthoringRequestStateV1::Ready,
        "{:?}",
        ready.failure
    );
    assert!(ready.drawing_available);
    let drawings = fixture.drawings.list("canvas-project").unwrap();
    assert_eq!(drawings.len(), 1);
    assert_eq!(drawings[0].id, ready.ready.as_ref().unwrap().drawing_id);
    assert!(
        requests[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value["role"] == "tool")
    );
    assert_eq!(fixture.approve().ready, ready.ready);
    assert_eq!(fixture.drawings.list("canvas-project").unwrap().len(), 1);
    let db = fixture.persistence.connection().unwrap();
    let content: String = db
        .query_row(
            "SELECT content FROM chat_messages WHERE id='canvas-source'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(content, "The UI calls the Core service.");
    let usage: (i64,i64) = db.query_row("SELECT count(*),sum(total_tokens) FROM token_usage_records WHERE source_kind='canvas_model_call'",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(usage, (2, 38));
    let linked_calls: i64 = db.query_row("SELECT count(*) FROM token_usage_records WHERE source_kind='canvas_model_call' AND source_id LIKE ?1",[format!("{}:%", ready.job_id.unwrap())],|row|row.get(0)).unwrap();
    assert_eq!(linked_calls, 2);
}

#[tokio::test]
async fn canvas_api_cancellation_stops_the_inflight_request_without_a_drawing() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture = CanvasApiFixture::new(&format!("http://{}", listener.local_addr().unwrap()));
    let running = fixture.approve();
    let (mut socket, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let _request = receive_canvas_request(&mut socket).await;
    fixture
        .service
        .cancel_canvas_job(DecideCanvasRequestV1 {
            project_id: "canvas-project".into(),
            conversation_id: fixture.conversation.clone(),
            request_id: running.request_id,
            expected_request_revision: running.request_revision,
            action_id: "stop-canvas".into(),
        })
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(5),
        fixture.service.wait_for_canvas_idle(),
    )
    .await
    .unwrap();
    assert_eq!(
        fixture.get().state,
        CanvasAuthoringRequestStateV1::Cancelled
    );
    assert!(fixture.drawings.list("canvas-project").unwrap().is_empty());
    let content: String = fixture
        .persistence
        .connection()
        .unwrap()
        .query_row(
            "SELECT content FROM chat_messages WHERE id='canvas-source'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(content, "The UI calls the Core service.");
}

#[tokio::test]
async fn canvas_api_configuration_change_before_finish_fails_without_publication() {
    use tokio::io::AsyncWriteExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture = CanvasApiFixture::new(&format!("http://{}", listener.local_addr().unwrap()));
    fixture.approve();
    let (mut socket, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let _request = receive_canvas_request(&mut socket).await;
    fixture
        .persistence
        .connection()
        .unwrap()
        .execute(
            "UPDATE provider_connections SET version=version+1 WHERE id='canvas-connection'",
            [],
        )
        .unwrap();
    let body = api_canvas_calls(&[
        ("canvas_apply_operations", fixture_apply()),
        (
            "canvas_finish",
            json!({"operation_id":"finish","expected_revision":1}),
        ),
    ])
    .to_string();
    socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
    tokio::time::timeout(
        Duration::from_secs(5),
        fixture.service.wait_for_canvas_idle(),
    )
    .await
    .unwrap();
    let failed = fixture.get();
    assert_eq!(failed.state, CanvasAuthoringRequestStateV1::Failed);
    assert_eq!(failed.failure.unwrap().code, "connection_changed");
    assert!(fixture.drawings.list("canvas-project").unwrap().is_empty());
}

#[tokio::test]
async fn canvas_project_deletion_guard_blocks_admission_and_releases_after_failure() {
    let fixture = CanvasApiFixture::new("http://127.0.0.1:1");
    let guard = fixture
        .service
        .block_canvas_project_deletion("canvas-project")
        .unwrap();
    let request = ApproveCanvasRequestV1 {
        project_id: "canvas-project".into(),
        conversation_id: fixture.conversation.clone(),
        request_id: fixture.request.request_id.clone(),
        expected_request_revision: 1,
        action_id: "guard-approval".into(),
        expected_target: ChatExecutionTargetRequest::Api {
            connection_id: "canvas-connection".into(),
        },
    };
    assert_eq!(
        fixture
            .service
            .approve_canvas_request(request.clone())
            .unwrap_err()
            .code,
        "project_deleting"
    );
    assert_eq!(fixture.get(), fixture.request);
    assert!(
        fixture
            .service
            .block_canvas_project_deletion("canvas-project")
            .is_err()
    );
    drop(guard);
    assert!(
        !fixture
            .service
            .runtime
            .canvas_authoring
            .blocked_projects
            .lock()
            .unwrap()
            .contains("canvas-project")
    );
    assert_eq!(
        fixture
            .service
            .approve_canvas_request(request)
            .unwrap()
            .state,
        CanvasAuthoringRequestStateV1::Running
    );
    tokio::time::timeout(
        Duration::from_secs(5),
        fixture.service.wait_for_canvas_idle(),
    )
    .await
    .unwrap();
    assert_eq!(fixture.get().state, CanvasAuthoringRequestStateV1::Failed);
}
