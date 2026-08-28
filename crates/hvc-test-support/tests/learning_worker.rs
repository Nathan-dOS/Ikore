//! Real router/provider parsing, never the unit-test stage fallback or real credentials.
use hvc_core::{
    AppCoreDependencies,
    llm::{LlmEventEnvelope, LlmEventSink, LlmStreamEvent},
    persistence_service::{
        CreatePulsationRequest, EnsureProjectForWorkspaceRequest, PersistenceService,
    },
};
use hvc_test_support::{
    FixedEmbeddingGateway, IsolatedTestEnvironment, ManualClock, MemoryCredentialStore,
    ScriptedHttpServer, ScriptedResponse,
};
use rusqlite::{Connection, params};
use serde_json::json;
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

struct StopAfterProvider(CancellationToken);
impl LlmEventSink for StopAfterProvider {
    fn emit(&self, e: &LlmEventEnvelope) {
        if matches!(
            e.event,
            LlmStreamEvent::RouteCompleted { .. } | LlmStreamEvent::RouteFailed { .. }
        ) {
            self.0.cancel();
        }
    }
}

#[test]
fn real_learning_request_is_bounded_and_unknown_evidence_never_publishes() {
    for unknown_evidence in [false, true] {
        let env = IsolatedTestEnvironment::new();
        let stop = CancellationToken::new();
        let clock = Arc::new(ManualClock::new("2026-08-28T12:00:00Z".parse().unwrap()));
        let deps = AppCoreDependencies::default();
        let service = PersistenceService::initialize_with_dependencies(
            env.data_dir(),
            Arc::new(StopAfterProvider(stop.clone())),
            Arc::new(MemoryCredentialStore::default()),
            Arc::new(FixedEmbeddingGateway::default()),
            clock.clone(),
            deps.jitter,
            deps.http_client,
        )
        .unwrap();
        let db = Connection::open(env.data_dir().join("hard-vibe-code.db")).unwrap();
        let project = service
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: env.workspace_dir().to_str().unwrap().into(),
                preferred_name: None,
            })
            .unwrap()
            .project;
        let pulse = service
            .create_pulsation(CreatePulsationRequest {
                project_id: project.id.clone(),
                feature_name: "Test completed delivery".into(),
            })
            .unwrap()
            .pulsation;
        // Seed a post-integration fixture; atomic real integration is covered by core tests.
        db.execute("UPDATE jobs SET status='done'", []).unwrap();
        db.execute("UPDATE pulsations SET status='done',stage='Integrator'", [])
            .unwrap();
        db.execute(
            "UPDATE project_pipeline_state SET base_epoch=1,active_pulsation_id=NULL",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO jobs(id,project_id,pulsation_id,kind,status,attempts,created_at,updated_at,job_class) VALUES('learning',?1,?2,'learning_report','pending',0,'now','now','background')",params![project.id,pulse.id]).unwrap();
        db.execute("INSERT INTO learning_runs(job_id,pulsation_id,integration_epoch,created_at,updated_at) VALUES('learning',?1,1,'now','now')",[&pulse.id]).unwrap();
        let output = json!({"schema_version":1,"observations":[{"text":"Evidence-backed observation","evidence_ids":[if unknown_evidence {"invented".into()}else{format!("pulse:{}",pulse.id)}],"recurring":false}],"recommendations":[],"limitations":["Local scope"]});
        let server=ScriptedHttpServer::start([ScriptedResponse::json(200,json!({"choices":[{"message":{"content":output.to_string()},"finish_reason":"stop"}],"usage":{"prompt_tokens":21,"completion_tokens":30}}).to_string())]);
        db.execute("UPDATE provider_connections SET provider_id='ollama',endpoint_url=?1 WHERE id='legacy-openai'",[server.url("")]).unwrap();
        db.execute(
            "UPDATE runtime_settings SET value='off' WHERE key='runtime.routing_v2_mode'",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO runtime_settings(key,value,updated_at) VALUES('runtime.max_output_tokens','4096','now') ON CONFLICT(key) DO UPDATE SET value=excluded.value",[]).unwrap();
        clock.advance(chrono::Duration::seconds(5));
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(10), service.run_job_worker_until(stop))
                .await
                .unwrap();
        });
        let requests = server.wait_for_requests(1, Duration::from_millis(1));
        assert_eq!(requests.len(), 1);
        let body: serde_json::Value = serde_json::from_str(&requests[0].body).unwrap();
        assert_eq!(body["max_tokens"], 1600);
        let input_text = body["messages"][1]["content"].as_str().unwrap();
        assert!(input_text.chars().count() <= 16000);
        let state: (String, u32) = db
            .query_row("SELECT state,budget_calls FROM learning_runs", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(
            state,
            (
                if unknown_evidence {
                    "failed"
                } else {
                    "publishing"
                }
                .into(),
                1
            )
        );
        let artifacts: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM artifacts WHERE kind='learning_report'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(artifacts, if unknown_evidence { 0 } else { 1 });
        let status: String = db
            .query_row("SELECT status FROM pulsations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(status, "done");
        let attempts: i64 = db
            .query_row("SELECT COUNT(*) FROM llm_attempts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(attempts, 1);
    }
}
