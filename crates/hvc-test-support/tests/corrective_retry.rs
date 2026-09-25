//! End-to-end coverage for the corrective retry: a model response that fails
//! validation must not be accepted (same validation gate, every attempt), but
//! the next attempt should be re-prompted with the previous raw output and
//! the exact validation error, instead of either failing the pulsation or
//! blindly resampling with an unchanged prompt.
//!
//! The failure is deliberately schema-VALID-but-non-English rather than
//! malformed JSON: on the API-direct transport used here, the LLM router's
//! own `validate_output` (`llm/router.rs`) re-validates raw JSON syntax and
//! schema conformance before `agent_runtime::execute_stage_inner` ever sees
//! the response, so a genuinely malformed response never reaches this
//! crate's capture point at all - it fails at the router as
//! `LlmErrorKind::InvalidResponse`/`OutputSchema` and blind-retries with no
//! raw text persisted anywhere, which is the accepted, documented limitation
//! for that specific path (the real incidents this feature was built for
//! were all CLI-transport, which bypasses the router entirely). A non-English
//! `summary` passes the router's structural JSON-schema check cleanly and is
//! only rejected downstream, inside `accept_stage_result`, which is exactly
//! the capture point this test exercises.
use hvc_core::{
    AppCoreDependencies,
    orchestration::PreparationSettings,
    persistence_service::{
        CreatePulsationRequest, EnsureProjectForWorkspaceRequest, PersistenceService,
    },
};
use hvc_test_support::{
    FixedEmbeddingGateway, IsolatedTestEnvironment, MemoryCredentialStore, RecordedRequest,
    ScriptedHttpServer, ScriptedResponse,
    pulsation_benchmark::{configure_loopback, stage_response},
};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

struct SilentEvents;
impl hvc_core::llm::LlmEventSink for SilentEvents {
    fn emit(&self, _: &hvc_core::llm::LlmEventEnvelope) {}
}

fn wait_until(mut predicate: impl FnMut() -> bool, message: &str) -> Instant {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if predicate() {
            return Instant::now();
        }
        assert!(Instant::now() < deadline, "{message}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn stage_of(request: &RecordedRequest) -> String {
    let body: Value = serde_json::from_str(&request.body).expect("provider request JSON");
    let prompt = body["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .filter_map(|m| m["content"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    prompt
        .lines()
        .find_map(|l| l.strip_prefix("- stage: "))
        .expect("stage prompt")
        .to_string()
}

/// Schema-valid (passes the router's own re-validation) but rejected
/// downstream by `validate_stage_output_language` for the accented,
/// non-ASCII `summary` - a failure the JSON schema itself cannot express and
/// therefore cannot catch at the router.
fn non_english_intake_response() -> String {
    json!({
        "outputLanguage": "en",
        "summary": "Intake concluído: classificação da tarefa em português",
        "statusUpdate": "advance",
        "notes": [],
        "contextUsage": [],
        "artifacts": []
    })
    .to_string()
}

#[test]
fn a_non_english_intake_response_is_corrected_on_the_next_attempt() {
    let env = IsolatedTestEnvironment::new();
    let intake_calls = Arc::new(AtomicUsize::new(0));
    let intake_calls_for_handler = Arc::clone(&intake_calls);
    let server = ScriptedHttpServer::respond_with(move |request| {
        let stage = stage_of(request);
        let body: Value = serde_json::from_str(&request.body).expect("provider request JSON");
        let model = body["model"].as_str().expect("provider request model identity");
        if stage == "Intake" && intake_calls_for_handler.fetch_add(1, Ordering::SeqCst) == 0 {
            let broken = non_english_intake_response();
            return ScriptedResponse::json(
                200,
                json!({
                    "model": model,
                    "choices": [{"message": {"content": broken}, "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 10, "completion_tokens": 20}
                })
                .to_string(),
            );
        }
        stage_response(request)
    });

    let deps = AppCoreDependencies::default();
    let service = PersistenceService::initialize_with_dependencies(
        env.data_dir(),
        Arc::new(SilentEvents),
        Arc::new(MemoryCredentialStore::default()),
        Arc::new(FixedEmbeddingGateway::default()),
        deps.clock,
        deps.jitter,
        deps.http_client,
    )
    .unwrap();
    configure_loopback(&service, &server);
    service
        .update_preparation_settings(PreparationSettings {
            preparation_enabled: false,
            learning_enabled: false,
            ..PreparationSettings::default()
        })
        .unwrap();
    let project = service
        .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
            workspace_path: env.workspace_dir().to_str().unwrap().into(),
            preferred_name: Some("Corrective retry fixture".into()),
        })
        .unwrap()
        .project
        .id;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let stop = CancellationToken::new();
    let stop_on_drop = stop.clone().drop_guard();
    let worker_service = service.clone();
    let _worker = runtime.spawn(async move { worker_service.run_job_worker_until(stop).await });

    let pulse = service
        .create_pulsation(CreatePulsationRequest {
            project_id: project.clone(),
            feature_name: "corrective retry regression".into(),
        })
        .unwrap()
        .pulsation
        .id;

    // Intake completing (advance) means it survived the rejected first
    // attempt and the corrected second one was accepted; SPECIFICATION
    // reaching a checkpoint is the signal, since Intake itself auto-advances.
    let db = Connection::open(service.database_path()).unwrap();
    wait_until(
        || {
            db.query_row(
                "SELECT EXISTS(SELECT 1 FROM pulsations p JOIN checkpoints c ON c.pulsation_id=p.id
                 WHERE p.id=?1 AND p.stage='SPECIFICATION' AND p.status='checkpoint'
                 AND c.status='waiting' AND c.kind='artifact_review')",
                [&pulse],
                |r| r.get(0),
            )
            .unwrap()
        },
        "pulsation never advanced past Intake",
    );

    let requests = server.wait_for_requests(2, Duration::from_secs(5));
    let intake_requests: Vec<_> = requests
        .iter()
        .filter(|r| stage_of(r) == "Intake")
        .collect();
    assert_eq!(
        intake_requests.len(),
        2,
        "Intake must be attempted exactly twice: the rejected response, then the correction"
    );
    let corrected_prompt = &intake_requests[1].body;
    assert!(
        corrected_prompt.contains("## Previous attempt correction"),
        "the retried prompt must carry the correction section"
    );
    assert!(
        corrected_prompt.contains("classificação da tarefa em português"),
        "the retried prompt must show the model its own previous rejected output"
    );
    assert!(
        corrected_prompt.contains("must be English") || corrected_prompt.contains("English"),
        "the retried prompt must carry the specific validation error, not a generic message"
    );

    let intake_job_id: String = db
        .query_row(
            "SELECT id FROM jobs WHERE pulsation_id=?1 AND kind='run_stage::Intake'",
            [&pulse],
            |r| r.get(0),
        )
        .unwrap();
    let failed_rows: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM agent_run_outputs WHERE job_id=?1 AND validation_error IS NOT NULL",
            [&intake_job_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        failed_rows, 1,
        "the rejected first attempt must be durably captured with its validation error"
    );
    let succeeded_rows: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM agent_run_outputs WHERE job_id=?1 AND validation_error IS NULL",
            [&intake_job_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        succeeded_rows, 1,
        "the corrected second attempt must succeed and persist normally"
    );

    drop(stop_on_drop);
}
