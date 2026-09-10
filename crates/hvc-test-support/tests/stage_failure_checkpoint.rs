//! End-to-end coverage for the stage failure checkpoint.
//!
//! Exhausting `runtime.max_retries` used to make the pulsation terminal, which meant
//! re-running Intake, Specification, Contract and every checkpoint in between just to
//! reach the same failing stage again. It now parks on a decision instead, keeping the
//! job and its fence intact so the developer can resume from where it stopped.
//!
//! The failure driven here is the same one `corrective_retry.rs` uses: a schema-valid
//! but non-English `summary`, rejected by `validate_stage_output_language` inside
//! `accept_stage_result` *before* any `stage_acceptances` row exists. That matters —
//! it is what leaves the job resumable, and it is the difference between this class
//! and `ModelReportedFailure`, whose durable acceptance forbids a retry.
use hvc_core::{
    AppCoreDependencies,
    orchestration::PreparationSettings,
    persistence_service::{
        CreatePulsationRequest, EnsureProjectForWorkspaceRequest, PersistenceService,
        ResolveStageFailureRequest, StageFailureDecision,
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
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

struct SilentEvents;
impl hvc_core::llm::LlmEventSink for SilentEvents {
    fn emit(&self, _: &hvc_core::llm::LlmEventEnvelope) {}
}

fn wait_until(mut predicate: impl FnMut() -> bool, message: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if predicate() {
            return;
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

struct Parked {
    checkpoint_id: String,
    state_version: u64,
}

/// Reads the waiting stage-failure checkpoint straight from the schema, so the test
/// fails loudly if the host ever stops writing one of these columns.
fn parked_failure(db: &Connection, pulse: &str) -> Option<Parked> {
    db.query_row(
        "SELECT c.id,p.state_version FROM stage_failure_requests f
         JOIN checkpoints c ON c.id=f.checkpoint_id
         JOIN pulsations p ON p.id=f.pulsation_id
         WHERE f.pulsation_id=?1 AND f.decision IS NULL
           AND c.status='waiting' AND c.purpose='stage_failure' AND p.status='checkpoint'",
        [pulse],
        |r| {
            Ok(Parked {
                checkpoint_id: r.get(0)?,
                state_version: r.get(1)?,
            })
        },
    )
    .ok()
}

fn job_row(db: &Connection, pulse: &str) -> (String, String, i64, i64) {
    db.query_row(
        "SELECT id,status,attempts,generation FROM jobs
         WHERE pulsation_id=?1 AND kind='run_stage::Intake'",
        [pulse],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )
    .expect("intake job")
}

#[test]
fn an_exhausted_retry_budget_parks_on_a_decision_and_resumes_where_it_stopped() {
    let env = IsolatedTestEnvironment::new();
    let intake_calls = Arc::new(AtomicUsize::new(0));
    // Flipped by the test once it has seen the checkpoint, so the operator's second
    // manual retry is the one that succeeds.
    let repaired = Arc::new(AtomicBool::new(false));
    let calls_for_handler = Arc::clone(&intake_calls);
    let repaired_for_handler = Arc::clone(&repaired);
    let server = ScriptedHttpServer::respond_with(move |request| {
        if stage_of(request) == "Intake" {
            calls_for_handler.fetch_add(1, Ordering::SeqCst);
            if !repaired_for_handler.load(Ordering::SeqCst) {
                return ScriptedResponse::json(
                    200,
                    json!({
                        "choices": [{
                            "message": {"content": non_english_intake_response()},
                            "finish_reason": "stop"
                        }],
                        "usage": {"prompt_tokens": 10, "completion_tokens": 20}
                    })
                    .to_string(),
                );
            }
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
            preferred_name: Some("Stage failure fixture".into()),
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
    let _stop_on_drop = stop.clone().drop_guard();
    let worker_service = service.clone();
    let _worker = runtime.spawn(async move { worker_service.run_job_worker_until(stop).await });

    let pulse = service
        .create_pulsation(CreatePulsationRequest {
            project_id: project.clone(),
            feature_name: "stage failure regression".into(),
        })
        .unwrap()
        .pulsation
        .id;

    let db = Connection::open(service.database_path()).unwrap();
    wait_until(
        || parked_failure(&db, &pulse).is_some(),
        "the exhausted retry budget never parked on a checkpoint",
    );

    // Default max_retries is 2, so attempts 1 and 2 auto-retry and the third parks.
    let automatic_calls = intake_calls.load(Ordering::SeqCst);
    assert_eq!(
        automatic_calls, 3,
        "the automatic budget should spend exactly three attempts before asking a human"
    );

    let (class, allow_retry, allow_revision, revision_stage, error_code): (
        String,
        i64,
        i64,
        Option<String>,
        String,
    ) = db
        .query_row(
            "SELECT failure_class,allow_retry,allow_revision,revision_stage,error_code
             FROM stage_failure_requests WHERE pulsation_id=?1",
            [&pulse],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(class, "retry_budget_exhausted");
    assert_eq!(error_code, "model_output_invalid");
    assert_eq!(allow_retry, 1);
    // Nothing precedes Intake, so there is no revision target and no revision button.
    assert_eq!(allow_revision, 0);
    assert_eq!(revision_stage, None);

    // The base must stay clean while the pulsation is resumable: needs_review would
    // block claim_next_pending_job and make the retry unexecutable.
    let blocking: Option<String> = db
        .query_row(
            "SELECT blocking_reason FROM project_pipeline_state WHERE project_id=?1",
            [&project],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(blocking, None, "the base was flagged while still resumable");
    // Learning is deferred to Abort: the pulsation has not actually failed yet.
    let learning: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM jobs WHERE pulsation_id=?1 AND kind='learning_report'",
            [&pulse],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(learning, 0, "learning ran before the failure was decided");

    let (job_id, status, attempts, generation) = job_row(&db, &pulse);
    assert_eq!(status, "interrupted", "the job must be preserved to resume");
    // The rejected output is kept so the retry re-prompts with the correction.
    let captured: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM agent_run_outputs WHERE job_id=?1 AND validation_error IS NOT NULL",
            [&job_id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(captured >= 1, "the rejected output was not captured");
    let detail = service
        .read_stage_failure_detail(&parked_failure(&db, &pulse).unwrap().checkpoint_id)
        .unwrap();
    assert!(
        detail
            .raw_output
            .as_deref()
            .is_some_and(|raw| raw.contains("classificação da tarefa em português")),
        "the panel must show the operator the agent's own rejected output"
    );

    // First manual retry, still broken: it must buy exactly one attempt and park again
    // rather than spending a fresh automatic budget or looping.
    let parked = parked_failure(&db, &pulse).unwrap();
    service
        .resolve_stage_failure(ResolveStageFailureRequest {
            checkpoint_id: parked.checkpoint_id.clone(),
            expected_state_version: parked.state_version,
            decision: StageFailureDecision::RetryStage,
            feedback: None,
        })
        .unwrap();
    let (_, resumed_status, resumed_attempts, resumed_generation) = job_row(&db, &pulse);
    assert_eq!(resumed_status, "pending");
    assert_eq!(
        resumed_attempts, attempts,
        "attempts must not reset: one click buys one attempt"
    );
    assert_eq!(resumed_generation, generation + 1);

    wait_until(
        || {
            parked_failure(&db, &pulse)
                .is_some_and(|next| next.checkpoint_id != parked.checkpoint_id)
        },
        "a failing manual retry never reopened the checkpoint",
    );
    assert_eq!(
        intake_calls.load(Ordering::SeqCst),
        automatic_calls + 1,
        "a manual retry must cost exactly one agent call"
    );

    // Second manual retry, with the provider now behaving: the pulsation resumes from
    // Intake without re-running anything before it.
    repaired.store(true, Ordering::SeqCst);
    let parked = parked_failure(&db, &pulse).unwrap();
    service
        .resolve_stage_failure(ResolveStageFailureRequest {
            checkpoint_id: parked.checkpoint_id,
            expected_state_version: parked.state_version,
            decision: StageFailureDecision::RetryStage,
            feedback: None,
        })
        .unwrap();
    wait_until(
        || {
            db.query_row(
                "SELECT EXISTS(SELECT 1 FROM pulsations p JOIN checkpoints c ON c.pulsation_id=p.id
                 WHERE p.id=?1 AND p.stage='SPECIFICATION' AND p.status='checkpoint'
                 AND c.status='waiting' AND c.purpose='artifact_review')",
                [&pulse],
                |r| r.get(0),
            )
            .unwrap()
        },
        "the resumed stage never advanced past Intake",
    );
    assert!(
        parked_failure(&db, &pulse).is_none(),
        "the failure checkpoint outlived its resolution"
    );
}

#[test]
fn aborting_a_parked_failure_is_what_finally_makes_it_terminal() {
    let env = IsolatedTestEnvironment::new();
    let server = ScriptedHttpServer::respond_with(move |request| {
        if stage_of(request) == "Intake" {
            return ScriptedResponse::json(
                200,
                json!({
                    "choices": [{
                        "message": {"content": non_english_intake_response()},
                        "finish_reason": "stop"
                    }],
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
            learning_enabled: true,
            ..PreparationSettings::default()
        })
        .unwrap();
    let project = service
        .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
            workspace_path: env.workspace_dir().to_str().unwrap().into(),
            preferred_name: Some("Stage failure abort fixture".into()),
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
    let _stop_on_drop = stop.clone().drop_guard();
    let worker_service = service.clone();
    let _worker = runtime.spawn(async move { worker_service.run_job_worker_until(stop).await });

    let pulse = service
        .create_pulsation(CreatePulsationRequest {
            project_id: project.clone(),
            feature_name: "stage failure abort".into(),
        })
        .unwrap()
        .pulsation
        .id;
    let db = Connection::open(service.database_path()).unwrap();
    wait_until(
        || parked_failure(&db, &pulse).is_some(),
        "the exhausted retry budget never parked on a checkpoint",
    );

    let parked = parked_failure(&db, &pulse).unwrap();
    let response = service
        .resolve_stage_failure(ResolveStageFailureRequest {
            checkpoint_id: parked.checkpoint_id.clone(),
            expected_state_version: parked.state_version,
            decision: StageFailureDecision::Abort,
            feedback: None,
        })
        .unwrap();
    assert_eq!(
        response.pulsation.status,
        hvc_core::persistence_service::PulsationStatus::Failed
    );
    // The terminal reason names the class, so the list no longer says a bare "Failed".
    assert_eq!(
        response.pulsation.terminal_reason.as_deref(),
        Some("retry_budget_exhausted")
    );
    let (_, status, _, _) = job_row(&db, &pulse);
    assert_eq!(status, "failed", "abort must close the preserved job");
    // Deferred from checkpoint-open time, these run exactly once, here.
    wait_until(
        || {
            db.query_row(
                "SELECT COUNT(*) FROM jobs WHERE pulsation_id=?1 AND kind='learning_report'",
                [&pulse],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
                >= 1
        },
        "aborting never enqueued the learning report",
    );

    // Deciding twice must not be possible.
    let repeated = service.resolve_stage_failure(ResolveStageFailureRequest {
        checkpoint_id: parked.checkpoint_id,
        expected_state_version: parked.state_version,
        decision: StageFailureDecision::Abort,
        feedback: None,
    });
    assert!(
        repeated.is_err(),
        "a resolved checkpoint accepted a second decision"
    );
}
