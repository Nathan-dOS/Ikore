//! Real provider/parser, fresh no-vault code context, durable Companion questions and resume.
use hvc_core::{
    AppCoreDependencies,
    llm::{CredentialStore, LlmEventEnvelope, LlmEventSink, LlmStreamEvent},
    orchestration::*,
    persistence_service::*,
};
use hvc_test_support::{
    FixedEmbeddingGateway, IsolatedTestEnvironment, MemoryCredentialStore, ScriptedHttpServer,
    ScriptedResponse,
};
use rusqlite::Connection;
use serde_json::json;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct StopEachResponse {
    stop: Mutex<CancellationToken>,
    calls: AtomicUsize,
}
impl LlmEventSink for StopEachResponse {
    fn emit(&self, envelope: &LlmEventEnvelope) {
        if matches!(envelope.event, LlmStreamEvent::RouteCompleted { .. }) {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.stop.lock().unwrap().cancel();
        }
        if matches!(envelope.event, LlmStreamEvent::RouteFailed { .. }) {
            self.stop.lock().unwrap().cancel();
        }
    }
}
fn initialize(
    env: &IsolatedTestEnvironment,
    sink: Arc<StopEachResponse>,
    credentials: Arc<MemoryCredentialStore>,
) -> PersistenceService {
    let deps = AppCoreDependencies::default();
    PersistenceService::initialize_with_dependencies(
        env.data_dir(),
        sink,
        credentials,
        Arc::new(FixedEmbeddingGateway::default()),
        deps.clock,
        deps.jitter,
        deps.http_client,
    )
    .unwrap()
}
fn phase(runtime: &tokio::runtime::Runtime, service: &PersistenceService, sink: &StopEachResponse) {
    let stop = CancellationToken::new();
    *sink.stop.lock().unwrap() = stop.clone();
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(15), service.run_job_worker_until(stop))
            .await
            .unwrap();
    });
}

#[test]
fn first_task_clarifies_through_companion_and_resumes_after_restart_without_extra_llm() {
    let env = IsolatedTestEnvironment::new();
    std::fs::write(
        env.workspace_dir().join("auth.rs"),
        "pub fn validate_token() -> bool { false } // CURRENT_CODE_EVIDENCE\n",
    )
    .unwrap();
    assert!(!env.workspace_dir().join("knowledge/obsidian").exists());
    let outputs = [
        json!({"summary":"Intake concluído","statusUpdate":"advance","notes":[],"artifacts":[]}),
        json!({"summary":"Falta formato","statusUpdate":"needs_clarification","notes":[],"artifacts":[],"questions":[{"id":"token_format","question":"Qual formato de token usar?","rationale":"Define validação de auth.rs"}]}),
        json!({"summary":"Spec após resposta humana","statusUpdate":"checkpoint_ready","notes":[],"artifacts":[{"name":"Specification","kind":"specification","content":"JWT: rejeitar token expirado com 401."}]}),
    ];
    let server=ScriptedHttpServer::start(outputs.map(|output|ScriptedResponse::json(200,json!({"choices":[{"message":{"content":output.to_string()},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":20}}).to_string())));
    let sink = Arc::new(StopEachResponse::default());
    let credentials = Arc::new(MemoryCredentialStore::default());
    credentials.set("openai", "fixture-only").unwrap();
    let service = initialize(&env, sink.clone(), credentials.clone());
    let db = Connection::open(env.data_dir().join("hard-vibe-code.db")).unwrap();
    db.execute("UPDATE provider_connections SET provider_id='ollama',endpoint_url=?1 WHERE id='legacy-openai'",[server.url("")]).unwrap();
    db.execute(
        "UPDATE runtime_settings SET value='off' WHERE key='runtime.routing_v2_mode'",
        [],
    )
    .unwrap();
    let project = service
        .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
            workspace_path: env.workspace_dir().to_string_lossy().into_owned(),
            preferred_name: None,
        })
        .unwrap()
        .project;
    let pulse = service
        .create_pulsation(CreatePulsationRequest {
            project_id: project.id.clone(),
            feature_name: "crie validação em auth.rs quando token expirado deve retornar 401"
                .into(),
        })
        .unwrap()
        .pulsation;
    assert!(
        !service
            .get_preparation_settings()
            .unwrap()
            .preparation_enabled
    );
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    phase(&runtime, &service, &sink);
    phase(&runtime, &service, &sink);
    let snapshot = service
        .get_pulsation_runtime_snapshot(GetPulsationRuntimeSnapshotRequest {
            project_id: project.id.clone(),
        })
        .unwrap()
        .snapshot;
    assert!(
        snapshot.hitl_checkpoints.is_empty(),
        "cannot approve incomplete spec"
    );
    let pending = snapshot.details[0].questions.clone().unwrap();
    let request = SubmitSpecificationAnswersRequest {
        pulsation_id: pulse.id.clone(),
        question_set_id: pending.id,
        checkpoint_id: pending.checkpoint_id,
        expected_input_revision: 1,
        expected_state_version: pending.fence.state_version,
        answers: vec![SpecificationAnswer {
            question_id: "token_format".into(),
            answer: "JWT assinado".into(),
        }],
    };
    let requests = server.wait_for_requests(2, Duration::from_secs(1));
    assert_eq!(requests.len(), 2);
    assert!(requests[1].body.contains("CURRENT_CODE_EVIDENCE"));
    assert_eq!(
        snapshot.details[0].context_disposition.as_deref(),
        Some("on_demand")
    );
    drop(service);
    let service = initialize(&env, sink.clone(), credentials);
    service.recover_interrupted_jobs().unwrap();
    assert_eq!(
        sink.calls.load(Ordering::SeqCst),
        2,
        "restart and Companion presentation have no model call"
    );
    service
        .submit_specification_answers(request.clone())
        .unwrap();
    assert!(service.submit_specification_answers(request).is_err());
    phase(&runtime, &service, &sink);
    assert_eq!(sink.calls.load(Ordering::SeqCst), 3);
    let snapshot = service
        .get_pulsation_runtime_snapshot(GetPulsationRuntimeSnapshotRequest {
            project_id: project.id,
        })
        .unwrap()
        .snapshot;
    assert!(snapshot.details[0].questions.is_none());
    assert_eq!(snapshot.hitl_checkpoints.len(), 1);
    let requests = server.wait_for_requests(3, Duration::from_secs(1));
    assert_eq!(requests.len(), 3);
    assert!(requests[2].body.contains("JWT assinado"));
    assert!(requests[2].body.contains("Qual formato de token usar?"));
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM specification_question_sets",
            [],
            |r| r.get::<_, usize>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM chat_messages", [], |r| r
            .get::<_, usize>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        db.query_row(
            "SELECT MAX(generation) FROM jobs WHERE kind='run_stage::SPECIFICATION'",
            [],
            |r| r.get::<_, usize>(0)
        )
        .unwrap(),
        2
    );
}
