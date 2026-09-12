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

#[test]
fn real_inflight_learning_yields_to_foreground_or_kill_switch_without_late_publication() {
    use hvc_core::{
        orchestration::PreparationSettings, persistence_service::GetPulsationRuntimeSnapshotRequest,
    };
    use hvc_test_support::pulsation_benchmark::{configure_loopback, stage_response};
    struct Quiet;
    impl LlmEventSink for Quiet {
        fn emit(&self, _: &LlmEventEnvelope) {}
    }
    for kill_switch in [false, true] {
        let env = IsolatedTestEnvironment::new();
        let clock = Arc::new(ManualClock::new("2026-08-28T12:00:00Z".parse().unwrap()));
        let deps = AppCoreDependencies::default();
        let service = PersistenceService::initialize_with_dependencies(
            env.data_dir(),
            Arc::new(Quiet),
            Arc::new(MemoryCredentialStore::default()),
            Arc::new(FixedEmbeddingGateway::default()),
            clock.clone(),
            deps.jitter,
            deps.http_client,
        )
        .unwrap();
        let project = service
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: env.workspace_dir().to_str().unwrap().into(),
                preferred_name: None,
            })
            .unwrap()
            .project
            .id;
        let done = service
            .create_pulsation(CreatePulsationRequest {
                project_id: project.clone(),
                feature_name: "Completed fixture".into(),
            })
            .unwrap()
            .pulsation
            .id;
        let db = Connection::open(service.database_path()).unwrap();
        // Post-integration fixture only; preemption and human actions are not SQL transitions.
        db.execute("UPDATE jobs SET status='done'", []).unwrap();
        db.execute("UPDATE pulsations SET status='done',stage='Integrator'", [])
            .unwrap();
        db.execute(
            "UPDATE project_pipeline_state SET base_epoch=1,active_pulsation_id=NULL",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO jobs(id,project_id,pulsation_id,kind,status,attempts,created_at,updated_at,job_class) VALUES('learning',?1,?2,'learning_report','pending',0,'now','now','background')",params![project,done]).unwrap();
        db.execute("INSERT INTO learning_runs(job_id,pulsation_id,integration_epoch,created_at,updated_at) VALUES('learning',?1,1,'now','now')",[&done]).unwrap();
        let (release, blocked) = std::sync::mpsc::channel();
        let mut first = true;
        let server = ScriptedHttpServer::respond_with(move |request| {
            if std::mem::take(&mut first) {
                blocked
                    .recv_timeout(Duration::from_secs(10))
                    .expect("release bounded provider barrier");
                let output = json!({"schema_version":2,"observations":[],"recommendations":[],"context_assessments":[],"graph_selections":[],"limitations":["Late result must be ignored"]});
                ScriptedResponse::json(200,json!({"choices":[{"message":{"content":output.to_string()},"finish_reason":"stop"}]}).to_string())
            } else {
                stage_response(request)
            }
        });
        configure_loopback(&service, &server);
        clock.advance(chrono::Duration::seconds(5));
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let stop = CancellationToken::new();
        let guard = stop.clone().drop_guard();
        let work = service.clone();
        let worker = runtime.spawn(async move { work.run_job_worker_until(stop).await });
        assert_eq!(server.wait_for_requests(1, Duration::from_secs(5)).len(), 1);
        let expected = if kill_switch {
            service
                .update_preparation_settings(PreparationSettings {
                    preparation_enabled: true,
                    ..Default::default()
                })
                .unwrap();
            service
                .create_pulsation(CreatePulsationRequest {
                    project_id: project.clone(),
                    feature_name: "melhore o sistema".into(),
                })
                .unwrap();
            service
                .update_preparation_settings(PreparationSettings {
                    preparation_enabled: false,
                    learning_enabled: false,
                    ..Default::default()
                })
                .unwrap();
            "disabled"
        } else {
            service
                .create_pulsation(CreatePulsationRequest {
                    project_id: project.clone(),
                    feature_name: "crie validação auth quando token expirado deve retornar 401"
                        .into(),
                })
                .unwrap();
            "preempted"
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let state: String = db
                .query_row("SELECT state FROM learning_runs", [], |r| r.get(0))
                .unwrap();
            if state == expected {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "Learning did not yield: {state}"
            );
            std::thread::yield_now();
        }
        release.send(()).unwrap();
        if !kill_switch {
            assert_eq!(server.wait_for_requests(2, Duration::from_secs(5)).len(), 2);
        }
        drop(guard);
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), worker)
                .await
                .unwrap()
                .unwrap()
        });
        service.recover_interrupted_jobs().unwrap();
        assert_eq!(
            db.query_row("SELECT budget_calls FROM learning_runs", [], |r| r
                .get::<_, u32>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            db.query_row("SELECT status FROM pulsations WHERE id=?1", [&done], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
            "done"
        );
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM artifacts WHERE kind='learning_report'",
                [],
                |r| r.get::<_, usize>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM runtime_slots", [], |r| r
                .get::<_, usize>(0))
                .unwrap(),
            0
        );
        if kill_switch {
            let snapshot = service
                .get_pulsation_runtime_snapshot(GetPulsationRuntimeSnapshotRequest {
                    project_id: project,
                })
                .unwrap()
                .snapshot;
            assert!(
                snapshot
                    .details
                    .iter()
                    .any(|d| d.preparation.description_review
                        == hvc_core::orchestration::DescriptionReviewState::Pending)
            );
        }
    }
}

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
        let output = json!({"schema_version":2,"observations":[{"text":"Evidence-backed observation","evidence_ids":[if unknown_evidence {"invented".into()}else{format!("pulse:{}",pulse.id)}],"recurring":false}],"recommendations":[],"context_assessments":[],"graph_selections":[],"limitations":["Local scope"]});
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
        // A report citing evidence it was never given is the model's mistake,
        // not the host's, so it is parked for another of the three granted
        // calls rather than ending the run on its first. What must never happen
        // either way is publication - asserted just below.
        assert_eq!(
            state,
            (
                if unknown_evidence {
                    "pending"
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
