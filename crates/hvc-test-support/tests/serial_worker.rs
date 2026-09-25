//! Real serial worker + provider parser + SQLite + host publisher (no cfg(test) fallback).
use hvc_core::{
    AppCoreDependencies,
    llm::{CredentialStore, LlmEventEnvelope, LlmEventSink, LlmStreamEvent},
    persistence_service::{
        CreatePulsationRequest, EnsureProjectForWorkspaceRequest, PersistenceService,
    },
};
use hvc_test_support::{
    FixedEmbeddingGateway, IsolatedTestEnvironment, MemoryCredentialStore, ScriptedHttpServer,
    ScriptedResponse,
};
use rusqlite::{Connection, params};
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

struct StopAfterResponse {
    stop: CancellationToken,
    db: PathBuf,
    revise_input: bool,
    completed: AtomicUsize,
}

impl LlmEventSink for StopAfterResponse {
    fn emit(&self, envelope: &LlmEventEnvelope) {
        if matches!(envelope.event, LlmStreamEvent::RouteCompleted { .. }) {
            self.completed.fetch_add(1, Ordering::SeqCst);
            if self.revise_input {
                let db = Connection::open(&self.db).unwrap();
                db.busy_timeout(Duration::from_secs(5)).unwrap();
                // Deterministic fence change after the real response, before stage acceptance.
                db.execute("UPDATE pulsations SET input_revision=input_revision+1,state_version=state_version+1",[]).unwrap();
            }
            self.stop.cancel(); // stops subsequent claims, not the current execution token
        }
        if matches!(envelope.event, LlmStreamEvent::RouteFailed { .. }) {
            self.stop.cancel();
        }
    }
}

#[test]
fn real_provider_results_are_accepted_failed_or_fenced_without_duplicate_effects() {
    for (status, stale, invalid_schema) in [
        ("advance", false, false),
        ("failed", false, false),
        ("advance", true, false),
        ("unknown", false, true),
    ] {
        let env = IsolatedTestEnvironment::new();
        let db_path = env.data_dir().join("hard-vibe-code.db");
        let output=json!({"outputLanguage":"en","summary":"Evidence from fixture provider","statusUpdate":status,"contextUsage":[],"notes":[],"artifacts":[{"name":"result","kind":"stage_output","relativePath":"generated.md","content":"  exact provider bytes\r\n"}]}).to_string();
        let server = ScriptedHttpServer::respond_with(move |request| {
            let body: serde_json::Value = serde_json::from_str(&request.body).unwrap();
            let model = body["model"].as_str().expect("provider request model identity");
            ScriptedResponse::json(200,json!({"model":model,"choices":[{"message":{"content":output},"finish_reason":"stop"}],"usage":{"prompt_tokens":8,"completion_tokens":12}}).to_string())
        });
        let stop = CancellationToken::new();
        let sink = Arc::new(StopAfterResponse {
            stop: stop.clone(),
            db: db_path.clone(),
            revise_input: stale,
            completed: AtomicUsize::new(0),
        });
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.set("openai", "fixture-only").unwrap();
        let deps = AppCoreDependencies::default();
        let service = PersistenceService::initialize_with_dependencies(
            env.data_dir(),
            sink.clone(),
            credentials,
            Arc::new(FixedEmbeddingGateway::default()),
            deps.clock,
            deps.jitter,
            deps.http_client,
        )
        .unwrap();
        let db = Connection::open(&db_path).unwrap();
        db.execute(
            "UPDATE provider_connections SET provider_id='ollama',endpoint_url=?1 WHERE id='legacy-openai'",
            [server.url("")],
        )
        .unwrap();
        db.execute(
            "UPDATE provider_route_targets SET enabled=0 WHERE connection_id<>'legacy-openai'",
            [],
        )
        .unwrap();
        db.execute("UPDATE provider_routes SET max_attempts=1", [])
            .unwrap();
        // The local Ollama profile lacks native json_schema. Legacy mode still validates
        // the real returned schema in the router/runtime, without opening a remote endpoint.
        db.execute(
            "UPDATE runtime_settings SET value='off' WHERE key='runtime.routing_v2_mode'",
            [],
        )
        .unwrap();
        db.execute("UPDATE provider_route_targets SET model_id='qwen2.5-coder:7b' WHERE connection_id='legacy-openai'",[]).unwrap();
        let project = service
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: env.workspace_dir().to_string_lossy().into_owned(),
                preferred_name: None,
            })
            .unwrap()
            .project;
        let pulse = service
            .create_pulsation(CreatePulsationRequest {
                project_id: project.id,
                feature_name: "Generate bounded artifact".into(),
            })
            .unwrap()
            .pulsation;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(10), service.run_job_worker_until(stop))
                .await
                .unwrap();
        });
        let diagnostic: Option<String> = db
            .query_row(
                "SELECT last_error FROM jobs WHERE kind='run_stage::Intake'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let attempts = db
            .prepare("SELECT error_kind FROM llm_attempts")
            .unwrap()
            .query_map([], |r| r.get::<_, Option<String>>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            sink.completed.load(Ordering::SeqCst),
            if invalid_schema { 0 } else { 1 },
            "real provider must complete: {diagnostic:?}; attempts={attempts:?}; requests={:?}",
            server.wait_for_requests(1, Duration::from_millis(1))
        );
        let requests = server.wait_for_requests(1, Duration::from_secs(1));
        assert_eq!(requests.len(), 1);
        let count = |table: &str| {
            db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap()
        };
        assert_eq!(count("agent_runs"), 1);
        assert_eq!(
            count("context_retrieval_runs"),
            1,
            "telemetry must survive accepted, failed, stale, and schema-invalid responses"
        );
        if stale {
            assert_eq!(count("stage_acceptances"), 0);
            assert_eq!(count("artifacts"), 0);
            assert_eq!(
                db.query_row(
                    "SELECT status FROM pulsations WHERE id=?1",
                    [&pulse.id],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
                "running",
                "late response must not fail the newer input"
            );
        } else if invalid_schema {
            assert_eq!(count("stage_acceptances"), 0);
            assert_eq!(count("artifacts"), 0);
            let job_status: String = db
                .query_row(
                    "SELECT status FROM jobs WHERE pulsation_id=?1 AND kind='run_stage::Intake'",
                    params![pulse.id],
                    |r| r.get::<_, String>(0),
                )
                .unwrap();
            assert!(
                matches!(job_status.as_str(), "pending" | "failed"),
                "schema failure must remain retryable or terminate explicitly: {job_status}"
            );
        } else {
            assert_eq!(count("stage_acceptances"), 1);
            assert_eq!(count("artifacts"), if status == "failed" { 0 } else { 1 });
            assert_eq!(
                db.query_row(
                    "SELECT status FROM jobs WHERE pulsation_id=?1 AND kind='run_stage::Intake'",
                    params![pulse.id],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
                if status == "failed" { "failed" } else { "done" }
            );
        }
        if status == "advance" && !stale && !invalid_schema {
            // Shutdown stops new publication units; startup replays the accepted outbox.
            service.recover_interrupted_jobs().unwrap();
            assert_eq!(
                std::fs::read_to_string(env.workspace_dir().join("generated.md")).unwrap(),
                "  exact provider bytes\r\n"
            );
            service.recover_interrupted_jobs().unwrap();
            assert_eq!(count("artifacts"), 1);
        } else {
            assert!(!env.workspace_dir().join("generated.md").exists());
        }
    }
}
