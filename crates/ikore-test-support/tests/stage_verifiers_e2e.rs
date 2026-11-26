#![allow(
    clippy::unwrap_used,
    reason = "Test-only integration suite fails immediately when fixture setup is invalid"
)]
//! Feature 25 (M4): comprehensive Tier-4 and traceability E2E suite for stage verifiers.

mod composition_support;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use composition_support::{
    SilentEvents, approve, completed_reply, expiry_task, wait_for_checkpoint, wait_until,
};
use ikore_contracts::exchange::{AgentProducer, ExchangeKind};
use ikore_core::attestation::{DERIVER_VERSION, RederivationCheck, StatementV1};
use ikore_core::custom_pipelines::{
    AgentStep, CustomPipelineDefinitionV1, DefaultEvent, PipelineRunStatus, PipelineStep,
    PipelineTrigger, SaveCustomPipelineOutcome, SaveCustomPipelineRequest, StepAgent, StepOutput,
    StepTransport,
};
use ikore_core::feature_forge::{
    CreatePulsationFromTasksRequest, CreateTasksRequest, TaskSelection, TaskService,
};
use ikore_core::orchestration::PreparationSettings;
use ikore_core::orchestration::composition::settings::CompositionSettings;
use ikore_core::orchestration::failure::{FailureClass, offered_decisions, offered_decisions_v4};
use ikore_core::persistence_service::{
    AgentDockSession, EnsureProjectForWorkspaceRequest, GetPulsationRuntimeSnapshotRequest,
    PersistenceService, PersistenceServiceError, PulsationStage, PulsationStatus,
    ResolveStageFailureRequest, SaveUiSessionRequest, StageFailureDecision,
};
use ikore_core::stage_verifiers::{
    MAX_STAGE_RESULT_INLINE_BYTES, MAX_VERIFIERS_PER_PROJECT, MAX_VERIFIERS_PER_STAGE,
    ResolveVerificationErrorRequest, SaveStageVerifierOutcome, StageVerifierDraft,
    VerificationErrorDecision,
};
use ikore_core::terminals::TerminalService;
use ikore_test_support::pulsation_benchmark::{configure_loopback, force_api_direct};
use ikore_test_support::{
    FixedEmbeddingGateway, IsolatedTestEnvironment, MemoryCredentialStore, ScriptedHttpServer,
    ScriptedResponse,
};
use rusqlite::Connection;
use serde_json::json;
use tokio_util::sync::CancellationToken;

static PATH_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

struct PathRestore(Option<std::ffi::OsString>);

impl Drop for PathRestore {
    fn drop(&mut self) {
        // SAFETY: PATH_LOCK serializes every PATH mutation in this test binary.
        unsafe {
            match &self.0 {
                Some(path) => std::env::set_var("PATH", path),
                None => std::env::remove_var("PATH"),
            }
        }
    }
}

struct Fixtures {
    directory: tempfile::TempDir,
    _restore: PathRestore,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Fixtures {
    fn install() -> Self {
        let lock = PATH_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let directory = tempfile::tempdir().unwrap();
        for name in ["codex", "claude", "gemini", "opencode", "agy"] {
            std::fs::copy(
                env!("CARGO_BIN_EXE_cli_fixture"),
                directory
                    .path()
                    .join(format!("{name}{}", std::env::consts::EXE_SUFFIX)),
            )
            .unwrap();
        }
        let original = std::env::var_os("PATH");
        let mut paths = vec![directory.path().to_path_buf()];
        if let Some(original) = &original {
            paths.extend(std::env::split_paths(original));
        }
        // SAFETY: PATH_LOCK is held and PathRestore restores PATH on drop.
        unsafe { std::env::set_var("PATH", std::env::join_paths(paths).unwrap()) };
        Self {
            directory,
            _restore: PathRestore(original),
            _lock: lock,
        }
    }

    fn wait_started(&self, key: &str, call: usize) {
        let path = self
            .directory
            .path()
            .join(format!("{key}.verifier-started-{call}"));
        let deadline = Instant::now() + Duration::from_secs(30);
        while !path.exists() {
            assert!(Instant::now() < deadline, "timed out waiting for {path:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn release(&self, key: &str) {
        std::fs::write(self.directory.path().join(format!("{key}.release")), b"go").unwrap();
    }

    fn prompt(&self, key: &str, call: usize) -> String {
        std::fs::read_to_string(
            self.directory
                .path()
                .join(format!("{key}.prompt-{call}.txt")),
        )
        .unwrap()
    }

    fn count(&self, key: &str) -> usize {
        std::fs::read_to_string(self.directory.path().join(format!("{key}.verifier-count")))
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }

    fn kill_pid(&self, key: &str) {
        let pid_path = self.directory.path().join(format!("{key}.pid"));
        if let Ok(text) = std::fs::read_to_string(&pid_path)
            && let Ok(pid) = text.trim().parse::<u32>()
        {
            #[cfg(windows)]
            let _ = std::process::Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/F", "/T"])
                .output();
            #[cfg(not(windows))]
            let _ = std::process::Command::new("kill")
                .args(["-9", &pid.to_string()])
                .output();
        }
    }
}

fn enable_stage_verifiers(service: &PersistenceService, project: &str, enabled: bool) {
    let mut settings = service.get_composition_settings(project).unwrap();
    settings.stage_verifiers_enabled = enabled;
    service
        .update_composition_settings(project, settings)
        .unwrap();
}

fn cli_verifier_draft(
    stage: &str,
    label: &str,
    position: i64,
    reject_limit: u8,
    backend: &str,
    model: &str,
    marker: &str,
) -> StageVerifierDraft {
    StageVerifierDraft {
        stage: stage.to_owned(),
        label: label.to_owned(),
        position,
        enabled: true,
        reject_limit,
        agent: StepAgent {
            transport: StepTransport::Cli,
            backend: Some(backend.to_owned()),
            model: Some(model.to_owned()),
            model_profile: None,
            thinking_level: "default".to_owned(),
            api_route_id: None,
        },
        instructions: format!("Check carefully. {marker}"),
    }
}

fn create_verifier(
    service: &PersistenceService,
    project: &str,
    draft: StageVerifierDraft,
) -> String {
    match service.create_stage_verifier(project, draft).unwrap() {
        SaveStageVerifierOutcome::Saved { verifier } => verifier.id,
        SaveStageVerifierOutcome::Invalid { errors } => panic!("invalid verifier: {errors:?}"),
    }
}

#[test]
fn labs_off_default_unchanged_even_with_blocking_verifiers_configured() {
    let _fixtures = Fixtures::install();
    let env = IsolatedTestEnvironment::new();
    std::fs::create_dir_all(env.workspace_dir().join("src")).unwrap();
    std::fs::write(
        env.workspace_dir().join("src/auth.rs"),
        composition_support::ORIGINAL_SOURCE,
    )
    .unwrap();

    let h = composition_support::Harness::start_prepared(
        env,
        vec![expiry_task()],
        CompositionSettings {
            enabled: true,
            stage_verifiers_enabled: false,
            ..CompositionSettings::default()
        }
        .with_all_recipes_qualified(),
        |facts, _, _| completed_reply(facts),
        |harness| {
            let terminals = TerminalService::new(
                harness.service.database_path(),
                harness.runtime_handle(),
                Arc::new(SilentEvents),
            )
            .unwrap();
            harness.service.attach_terminal_service(Arc::new(terminals));
            create_verifier(
                &harness.service,
                &harness.project,
                cli_verifier_draft(
                    "SPECIFICATION",
                    "Strict spec blocker",
                    0,
                    2,
                    "claude",
                    "haiku",
                    "stage-verifier-fixture:always-reject:labsoff",
                ),
            );
            create_verifier(
                &harness.service,
                &harness.project,
                cli_verifier_draft(
                    "Code",
                    "Strict code blocker",
                    0,
                    2,
                    "codex",
                    "gpt-5.6-luna",
                    "stage-verifier-fixture:always-reject:labsoff_code",
                ),
            );
        },
    );

    for stage in ["SPECIFICATION", "Contract", "Test", "Integrator"] {
        h.wait_for_checkpoint(stage);
        h.approve();
    }
    wait_until(&h.service, &h.pulse, |db| {
        db.query_row(
            "SELECT status='done' FROM pulsations WHERE id=?1",
            [&h.pulse],
            |r| r.get(0),
        )
        .unwrap()
    });
    let runs: u32 = h
        .db()
        .query_row("SELECT count(*) FROM stage_verifier_runs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(runs, 0, "Labs OFF must never execute any stage verifier");
    h.stop();
}

#[test]
fn t4_01_full_pulsation_advisory_intake_blocking_code_reject_once_and_deferred_trigger() {
    let fixtures = Fixtures::install();
    let env = IsolatedTestEnvironment::new();
    std::fs::create_dir_all(env.workspace_dir().join("src")).unwrap();
    std::fs::write(
        env.workspace_dir().join("src/auth.rs"),
        composition_support::ORIGINAL_SOURCE,
    )
    .unwrap();

    let after_code_id: Arc<Mutex<Option<String>>> = Arc::default();
    let after_code_slot = after_code_id.clone();

    let h = composition_support::Harness::start_prepared(
        env,
        vec![expiry_task()],
        CompositionSettings {
            enabled: true,
            stage_verifiers_enabled: true,
            ..CompositionSettings::default()
        }
        .with_all_recipes_qualified(),
        |facts, _, _| completed_reply(facts),
        move |harness| {
            let terminals = TerminalService::new(
                harness.service.database_path(),
                harness.runtime_handle(),
                Arc::new(SilentEvents),
            )
            .unwrap();
            harness.service.attach_terminal_service(Arc::new(terminals));
            create_verifier(
                &harness.service,
                &harness.project,
                cli_verifier_draft(
                    "Intake",
                    "Intake advisor",
                    0,
                    0,
                    "claude",
                    "haiku",
                    "stage-verifier-fixture:reject-20-advisory-mixed:t401_intake",
                ),
            );
            create_verifier(
                &harness.service,
                &harness.project,
                cli_verifier_draft(
                    "Code",
                    "Code gatekeeper",
                    0,
                    2,
                    "codex",
                    "gpt-5.6-luna",
                    "stage-verifier-fixture:reject-1-then-pass:t401_code",
                ),
            );
            let pipeline = match harness
                .service
                .save_custom_pipeline(SaveCustomPipelineRequest {
                    project_id: harness.project.clone(),
                    pipeline_id: None,
                    expected_version: None,
                    scope: None,
                    parent_pipeline_id: None,
                    name: "Post-Code Hook".into(),
                    description: String::new(),
                    definition: CustomPipelineDefinitionV1 {
                        schema_version: 1,
                        trigger: PipelineTrigger::DefaultEvent {
                            event: DefaultEvent::StagePassed,
                            stage: Some("Code".into()),
                        },
                        steps: vec![PipelineStep::Agent(AgentStep {
                            id: "post".into(),
                            label: "Post check".into(),
                            agent: StepAgent {
                                transport: StepTransport::Cli,
                                backend: Some("claude".into()),
                                model: Some("haiku".into()),
                                model_profile: None,
                                thinking_level: "default".into(),
                                api_route_id: None,
                            },
                            instructions: "Follow pipeline-step-fixture:text".into(),
                            output: StepOutput::Text,
                        })],
                    },
                })
                .unwrap()
            {
                SaveCustomPipelineOutcome::Saved { pipeline } => pipeline.id,
                SaveCustomPipelineOutcome::Invalid { errors } => panic!("{errors:?}"),
            };
            *after_code_slot.lock().unwrap() = Some(pipeline);
        },
    );

    // Wait for Intake's advisory verifier to pass without blocking, then Specification opens.
    h.wait_for_checkpoint("SPECIFICATION");
    let spec_findings = h
        .service
        .list_checkpoint_verification_findings(&h.pulse, PulsationStage::Specification)
        .unwrap();
    assert_eq!(
        spec_findings.len(),
        1,
        "Advisory Intake findings carry over to Specification checkpoint"
    );
    assert!(spec_findings[0].advisory);
    h.approve();

    h.wait_for_checkpoint("Contract");
    // While Code has not passed yet, Post-Code Hook must have zero runs.
    h.approve();

    // Code runs twice because t401_code rejects once then passes.
    h.wait_for_checkpoint("Test");
    assert_eq!(fixtures.count("t401_code"), 2);

    let db = h.db();
    let code_gens: i64 = db
        .query_row(
            "SELECT MAX(stage_generation) FROM stage_verifier_runs WHERE pulsation_id=?1 AND stage='Code'",
            [&h.pulse],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        code_gens >= 2,
        "Code must have re-run after rejection; max gen={code_gens}"
    );
    // Confirm the Post-Code custom pipeline started only after Code verification passed.
    let post_id = after_code_id.lock().unwrap().clone().unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let runs = h
            .service
            .custom_pipelines_snapshot(&h.project)
            .unwrap()
            .runs;
        if runs
            .iter()
            .any(|r| r.pipeline_id == post_id && r.status == PipelineRunStatus::Done)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "post-Code trigger did not finish"
        );
        std::thread::sleep(Duration::from_millis(25));
    }

    h.approve();
    h.wait_for_checkpoint("Integrator");
    h.approve();
    wait_until(&h.service, &h.pulse, |db| {
        db.query_row(
            "SELECT status='done' FROM pulsations WHERE id=?1",
            [&h.pulse],
            |r| r.get(0),
        )
        .unwrap()
    });
    h.stop();
}

#[test]
fn t4_02_reject_limit_exhausted_continue_anyway_sanitized_report_and_attestation_v2() {
    let _fixtures = Fixtures::install();
    let env = IsolatedTestEnvironment::new();
    std::fs::create_dir_all(env.workspace_dir().join("src")).unwrap();
    std::fs::write(
        env.workspace_dir().join("src/auth.rs"),
        composition_support::ORIGINAL_SOURCE,
    )
    .unwrap();

    let h = composition_support::Harness::start_prepared(
        env,
        vec![expiry_task()],
        CompositionSettings {
            enabled: true,
            stage_verifiers_enabled: true,
            ..CompositionSettings::default()
        }
        .with_all_recipes_qualified(),
        |facts, _, _| completed_reply(facts),
        |harness| {
            let terminals = TerminalService::new(
                harness.service.database_path(),
                harness.runtime_handle(),
                Arc::new(SilentEvents),
            )
            .unwrap();
            harness.service.attach_terminal_service(Arc::new(terminals));
            create_verifier(
                &harness.service,
                &harness.project,
                cli_verifier_draft(
                    "SPECIFICATION",
                    "Stubborn spec verifier",
                    0,
                    1,
                    "claude",
                    "haiku",
                    "stage-verifier-fixture:always-reject:t402",
                ),
            );
        },
    );

    let cp_id = h.wait_for_failure_checkpoint();
    let snap = h
        .service
        .get_pulsation_runtime_snapshot(GetPulsationRuntimeSnapshotRequest {
            project_id: h.project.clone(),
        })
        .unwrap()
        .snapshot;
    let cp = snap
        .failure_checkpoints
        .iter()
        .find(|c| c.checkpoint_id == cp_id)
        .unwrap();
    assert_eq!(cp.failure_class, "verification_rejected");
    assert!(cp.allow_retry);
    assert!(cp.allow_revision);
    assert!(cp.allow_continue_anyway);

    // 501 chars must be rejected inline / persistence side while keeping checkpoint waiting.
    let err = h
        .service
        .resolve_stage_failure_with_convergence(
            ResolveStageFailureRequest {
                checkpoint_id: cp.checkpoint_id.clone(),
                expected_state_version: cp.state_version,
                decision: StageFailureDecision::ContinueAnyway,
                feedback: Some("x".repeat(501)),
            },
            None,
            None,
        )
        .unwrap_err();
    assert!(matches!(err, PersistenceServiceError::Validation(_)));

    // Retry grants +1 round -> still rejects -> Continue anyway with canary secret & HTML tag.
    h.service
        .resolve_stage_failure_with_convergence(
            ResolveStageFailureRequest {
                checkpoint_id: cp.checkpoint_id.clone(),
                expected_state_version: cp.state_version,
                decision: StageFailureDecision::RetryStage,
                feedback: None,
            },
            None,
            None,
        )
        .unwrap();
    let cp2_id = h.wait_for_failure_checkpoint();
    let cp2 = h
        .service
        .get_pulsation_runtime_snapshot(GetPulsationRuntimeSnapshotRequest {
            project_id: h.project.clone(),
        })
        .unwrap()
        .snapshot
        .failure_checkpoints
        .into_iter()
        .find(|c| c.checkpoint_id == cp2_id)
        .unwrap();

    let canary_secret = "sk-ant-api03-CANARYSECRETTOKEN9876543210ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let justification =
        format!("Approved risk <script>alert(1)</script> token={canary_secret} trade-off.");
    h.service
        .resolve_stage_failure_with_convergence(
            ResolveStageFailureRequest {
                checkpoint_id: cp2.checkpoint_id.clone(),
                expected_state_version: cp2.state_version,
                decision: StageFailureDecision::ContinueAnyway,
                feedback: Some(justification),
            },
            None,
            None,
        )
        .unwrap();

    // Deferred Specification checkpoint now opens; remove verifier or disable before finishing.
    h.wait_for_checkpoint("SPECIFICATION");
    h.approve();
    for stage in ["Contract", "Test", "Integrator"] {
        h.wait_for_checkpoint(stage);
        h.approve();
    }
    wait_until(&h.service, &h.pulse, |db| {
        db.query_row(
            "SELECT status='done' FROM pulsations WHERE id=?1",
            [&h.pulse],
            |r| r.get(0),
        )
        .unwrap()
    });

    // HTML report has verifications & redacted/escaped justification.
    let report = h.service.build_pulsation_report(&h.pulse).unwrap();
    assert!(report.verifications.is_some());
    let html = ikore_core::reports::render_pulsation_report(&report);
    assert!(html.contains("id=\"verifications\""));
    assert!(!html.contains(canary_secret), "secret leaked in HTML");
    assert!(
        !html.contains("<script>alert(1)</script>"),
        "unescaped HTML"
    );

    // Attestation v2 includes verification digests with ZERO free text/canary.
    let _ = h.service.create_attestation_key();
    while h.service.seal_next_attestation().unwrap_or(false) {}
    let verification = h.service.verify_attestation(&h.pulse).unwrap();
    assert_eq!(
        verification.unwrap().rederivation,
        RederivationCheck::Matches
    );
    let raw_statement: String = h
        .db()
        .query_row(
            "SELECT statement_json FROM pulsation_attestations WHERE pulsation_id=?1",
            [&h.pulse],
            |r| r.get(0),
        )
        .unwrap();
    let statement: StatementV1 = serde_json::from_str(&raw_statement).unwrap();
    assert_eq!(statement.predicate.sealing.deriver_version, DERIVER_VERSION);
    assert!(!statement.predicate.evidence.verifications.is_empty());
    assert!(!raw_statement.contains(canary_secret));
    assert!(!raw_statement.contains("<script>"));
    assert!(!raw_statement.contains("trade-off"));

    h.stop();
}

#[test]
fn t4_03_multi_verifiers_cli_and_api_json_recovery_missing_file_and_carryover() {
    let fixtures = Fixtures::install();
    let env = IsolatedTestEnvironment::new();
    std::fs::create_dir_all(env.workspace_dir().join("src")).unwrap();
    std::fs::write(
        env.workspace_dir().join("src/auth.rs"),
        composition_support::ORIGINAL_SOURCE,
    )
    .unwrap();

    // Custom HTTP router handling composed stages + Visualizer + API verifier.
    let server = ScriptedHttpServer::respond_with(|request| {
        if request.body.trim().is_empty() {
            return ScriptedResponse::json(400, r#"{"error":"empty request"}"#);
        }
        let body: serde_json::Value = serde_json::from_str(&request.body).unwrap_or_default();
        let system = body["messages"][0]["content"].as_str().unwrap_or_default();
        let user = body["messages"][1]["content"].as_str().unwrap_or_default();
        if system.contains("stage-verifier-fixture:") || user.contains("stage-verifier-fixture:") {
            let reply = json!({
                "verdict": "pass",
                "summary": "Missing file path check.",
                "findings": [{
                    "id": "w1",
                    "severity": "warning",
                    "message": "Defect referencing absent file.",
                    "file": "does/not/exist.rs",
                    "line": 7,
                    "suggestion": "Verify path"
                }]
            })
            .to_string();
            return ScriptedResponse::sse([
                format!(
                    "data: {}\n\n",
                    json!({"model":"gpt-4.1-mini","choices":[{"delta":{"content":reply}}]})
                ),
                format!(
                    "data: {}\n\ndata: [DONE]\n\n",
                    json!({"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}})
                ),
            ]);
        }
        let facts = ikore_test_support::composition_replies::parse_prompt(request);
        completed_reply(&facts)
    });

    let deps = ikore_core::AppCoreDependencies::default();
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
            preferred_name: Some("Multi verifier".into()),
        })
        .unwrap()
        .project
        .id;
    force_api_direct(&service, &project);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let term_service = Arc::new(
        TerminalService::new(
            service.database_path(),
            runtime.handle().clone(),
            Arc::new(SilentEvents),
        )
        .unwrap(),
    );
    service.attach_terminal_service(term_service);

    service
        .update_composition_settings(
            &project,
            CompositionSettings {
                enabled: true,
                stage_verifiers_enabled: true,
                ..CompositionSettings::default()
            }
            .with_all_recipes_qualified(),
        )
        .unwrap();

    // CLI verifier: invalid JSON -> retries once with "## Your previous answer" -> passes!
    create_verifier(
        &service,
        &project,
        cli_verifier_draft(
            "Code",
            "CLI verifier",
            0,
            2,
            "claude",
            "haiku",
            "stage-verifier-fixture:invalid-once-then-pass:t403_cli",
        ),
    );
    // API advisory verifier: returns missing file path -> host normalizes file_not_found = true.
    create_verifier(
        &service,
        &project,
        StageVerifierDraft {
            stage: "Code".into(),
            label: "API advisor".into(),
            position: 1,
            enabled: true,
            reject_limit: 0,
            agent: StepAgent {
                transport: StepTransport::Api,
                backend: None,
                model: None,
                model_profile: None,
                thinking_level: "default".into(),
                api_route_id: Some("legacy-default".into()),
            },
            instructions: "API check. stage-verifier-fixture:reject-missing-file:t403_api".into(),
        },
    );

    let created = TaskService::new(service.clone())
        .create(CreateTasksRequest {
            project_id: project.clone(),
            request_id: "t403-tasks".into(),
            tasks: vec![expiry_task()],
        })
        .unwrap();
    let pulse = service
        .create_pulsation_from_tasks(CreatePulsationFromTasksRequest {
            project_id: project.clone(),
            request_id: "t403-pulse".into(),
            title: "Multi verifier pulse".into(),
            tasks: created
                .iter()
                .map(|t| TaskSelection {
                    task_id: t.id.clone(),
                    expected_revision: t.revision,
                    manual_path_hint: None,
                })
                .collect(),
        })
        .unwrap()
        .id;

    let stop = CancellationToken::new();
    let worker_stop = stop.clone();
    let worker_svc = service.clone();
    let worker = runtime.spawn(async move { worker_svc.run_job_worker_until(worker_stop).await });

    wait_for_checkpoint(&service, &pulse, "SPECIFICATION");
    approve(&service, &pulse);
    wait_for_checkpoint(&service, &pulse, "Contract");
    approve(&service, &pulse);
    wait_for_checkpoint(&service, &pulse, "Test");

    // Verify CLI JSON retry prompt + missing file normalization + Code->Test carryover.
    assert!(fixtures.count("t403_cli") >= 2);
    let second_prompt = fixtures.prompt("t403_cli", 2);
    assert!(second_prompt.contains("## Your previous answer"));
    let findings = service
        .list_checkpoint_verification_findings(&pulse, PulsationStage::Test)
        .unwrap();
    let api_run = findings
        .iter()
        .find(|r| r.verifier_label == "API advisor")
        .expect("Code advisory carried over into Test Brief");
    assert!(
        api_run
            .verdict
            .as_ref()
            .unwrap()
            .findings
            .iter()
            .any(|f| f.file_not_found)
    );
    assert!(!ExchangeKind::VerificationFinding.is_reserved());
    let _ = AgentProducer::StageVerifier;
    let _ = MAX_STAGE_RESULT_INLINE_BYTES;

    stop.cancel();
    service.cancel_all_active_jobs();
    runtime.block_on(async {
        let _ = tokio::time::timeout(Duration::from_secs(10), worker).await;
    });
    runtime.shutdown_timeout(Duration::from_secs(5));
}

#[test]
fn t4_04_dirty_crash_recovery_and_verification_error_decisions_with_receipts() {
    let fixtures = Fixtures::install();
    let env = IsolatedTestEnvironment::new();
    std::fs::create_dir_all(env.workspace_dir().join("src")).unwrap();
    std::fs::write(
        env.workspace_dir().join("src/auth.rs"),
        composition_support::ORIGINAL_SOURCE,
    )
    .unwrap();
    let creds = Arc::new(MemoryCredentialStore::default());

    let server = ScriptedHttpServer::respond_with(|req| {
        let facts = ikore_test_support::composition_replies::parse_prompt(req);
        completed_reply(&facts)
    });

    let deps = ikore_core::AppCoreDependencies::default();
    let svc = PersistenceService::initialize_with_dependencies(
        env.data_dir(),
        Arc::new(SilentEvents),
        creds.clone(),
        Arc::new(FixedEmbeddingGateway::default()),
        deps.clock,
        deps.jitter,
        deps.http_client,
    )
    .unwrap();
    configure_loopback(&svc, &server);
    svc.update_preparation_settings(PreparationSettings {
        preparation_enabled: false,
        learning_enabled: false,
        ..PreparationSettings::default()
    })
    .unwrap();
    let project = svc
        .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
            workspace_path: env.workspace_dir().to_str().unwrap().into(),
            preferred_name: Some("Crash QA".into()),
        })
        .unwrap()
        .project
        .id;
    force_api_direct(&svc, &project);
    svc.update_composition_settings(
        &project,
        CompositionSettings {
            enabled: true,
            stage_verifiers_enabled: true,
            ..CompositionSettings::default()
        }
        .with_all_recipes_qualified(),
    )
    .unwrap();

    create_verifier(
        &svc,
        &project,
        cli_verifier_draft(
            "SPECIFICATION",
            "Crash recovery verifier",
            0,
            2,
            "codex",
            "gpt-5.6-luna",
            "stage-verifier-fixture:block-until-release-pass:t404_crash",
        ),
    );
    let created = TaskService::new(svc.clone())
        .create(CreateTasksRequest {
            project_id: project.clone(),
            request_id: "t404-tasks".into(),
            tasks: vec![expiry_task()],
        })
        .unwrap();
    let pulse = svc
        .create_pulsation_from_tasks(CreatePulsationFromTasksRequest {
            project_id: project.clone(),
            request_id: "t404-pulse".into(),
            title: "Crash pulse".into(),
            tasks: created
                .iter()
                .map(|t| TaskSelection {
                    task_id: t.id.clone(),
                    expected_revision: t.revision,
                    manual_path_hint: None,
                })
                .collect(),
        })
        .unwrap()
        .id;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let terminals = Arc::new(
        TerminalService::new(
            svc.database_path(),
            runtime.handle().clone(),
            Arc::new(SilentEvents),
        )
        .unwrap(),
    );
    svc.attach_terminal_service(terminals);
    let stop = CancellationToken::new();
    let worker_svc = svc.clone();
    let worker_stop = stop.clone();
    let worker_handle =
        runtime.spawn(async move { worker_svc.run_job_worker_until(worker_stop).await });

    // Wait until the verifier is running on disk, then simulate a dirty crash (abort without stop).
    fixtures.wait_started("t404_crash", 1);
    let db = Connection::open(svc.database_path()).unwrap();
    let active_slots: u32 = db
        .query_row(
            "SELECT count(*) FROM automation_slots WHERE class='verification'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let running_rows: u32 = db
        .query_row(
            "SELECT count(*) FROM stage_verifier_runs WHERE status='running'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(active_slots, 1);
    assert_eq!(running_rows, 1);

    worker_handle.abort();
    fixtures.kill_pid("t404_crash");
    drop(db);
    drop(svc);
    runtime.shutdown_timeout(Duration::from_secs(2));

    // Release fixture so recovered attempt passes immediately after reopen!
    fixtures.release("t404_crash");

    let deps2 = ikore_core::AppCoreDependencies::default();
    let svc2 = PersistenceService::initialize_with_dependencies(
        env.data_dir(),
        Arc::new(SilentEvents),
        creds,
        Arc::new(FixedEmbeddingGateway::default()),
        deps2.clock,
        deps2.jitter,
        deps2.http_client,
    )
    .unwrap();
    configure_loopback(&svc2, &server);
    svc2.recover_interrupted_jobs().unwrap();
    let runtime2 = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let terminals2 = Arc::new(
        TerminalService::new(
            svc2.database_path(),
            runtime2.handle().clone(),
            Arc::new(SilentEvents),
        )
        .unwrap(),
    );
    svc2.attach_terminal_service(terminals2);
    let stop2 = CancellationToken::new();
    let w2 = svc2.clone();
    let w2_stop = stop2.clone();
    let h2 = runtime2.spawn(async move { w2.run_job_worker_until(w2_stop).await });

    // Recovered run re-runs once cleanly and advances to Specification HITL!
    wait_for_checkpoint(&svc2, &pulse, "SPECIFICATION");

    // Also confirm crash-verifier -> verification_error with zero cli_fallback & RetryVerifier / SkipVerifier.
    let crash_v = create_verifier(
        &svc2,
        &project,
        cli_verifier_draft(
            "Contract",
            "Crash verifier",
            0,
            2,
            "claude",
            "haiku",
            "stage-verifier-fixture:crash:t404_err",
        ),
    );
    approve(&svc2, &pulse);
    let err_cp = {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let snap = svc2
                .get_pulsation_runtime_snapshot(GetPulsationRuntimeSnapshotRequest {
                    project_id: project.clone(),
                })
                .unwrap()
                .snapshot;
            if let Some(cp) = snap.verification_error_checkpoints.into_iter().next() {
                break cp;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for verification_error"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    };
    assert_eq!(err_cp.verifier_id, crash_v);
    let fallbacks: u32 = Connection::open(svc2.database_path())
        .unwrap()
        .query_row(
            "SELECT count(*) FROM checkpoints WHERE pulsation_id=?1 AND kind='cli_fallback'",
            [&pulse],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(fallbacks, 0, "verifier errors must never open cli_fallback");

    svc2.resolve_verification_error_checkpoint(ResolveVerificationErrorRequest {
        checkpoint_id: err_cp.checkpoint_id.clone(),
        expected_state_version: err_cp.state_version,
        decision: VerificationErrorDecision::SkipVerifier,
    })
    .unwrap();
    wait_for_checkpoint(&svc2, &pulse, "Contract");

    let receipts: u32 = Connection::open(svc2.database_path())
        .unwrap()
        .query_row(
            "SELECT count(*) FROM checkpoint_approval_receipts WHERE pulsation_id=?1",
            [&pulse],
            |r| r.get(0),
        )
        .unwrap();
    assert!(receipts >= 2);

    stop2.cancel();
    svc2.cancel_all_active_jobs();
    runtime2.block_on(async {
        let _ = tokio::time::timeout(Duration::from_secs(10), h2).await;
    });
    runtime2.shutdown_timeout(Duration::from_secs(5));
}

#[test]
fn t4_05_counter_isolation_and_verification_rejected_six_stage_matrix() {
    // 6-stage decision matrix assertion on VerificationRejected (legacy & v4).
    for stage in [
        PulsationStage::Intake,
        PulsationStage::Specification,
        PulsationStage::Contract,
        PulsationStage::Code,
        PulsationStage::Test,
        PulsationStage::Integrator,
    ] {
        let dec = offered_decisions(FailureClass::VerificationRejected, stage).unwrap();
        assert!(dec.allow_retry);
        assert!(dec.allow_revision);
        assert!(dec.allow_continue_anyway);
        assert_eq!(dec.revision_stage, Some(stage));

        let v4 = offered_decisions_v4(FailureClass::VerificationRejected, stage, 0, false).unwrap();
        assert!(v4.allow_retry);
        assert!(v4.allow_continue_anyway);
        if matches!(stage, PulsationStage::Test | PulsationStage::Integrator) {
            assert!(v4.revision_targets.contains(&PulsationStage::Code));
        }
    }

    // Counter isolation on a live Contract verifier reject + human RequestRevision.
    let _fixtures = Fixtures::install();
    let env = IsolatedTestEnvironment::new();
    std::fs::create_dir_all(env.workspace_dir().join("src")).unwrap();
    std::fs::write(
        env.workspace_dir().join("src/auth.rs"),
        composition_support::ORIGINAL_SOURCE,
    )
    .unwrap();

    let h = composition_support::Harness::start_prepared(
        env,
        vec![expiry_task()],
        CompositionSettings {
            enabled: true,
            stage_verifiers_enabled: true,
            ..CompositionSettings::default()
        }
        .with_all_recipes_qualified(),
        |facts, _, _| completed_reply(facts),
        |harness| {
            let terminals = TerminalService::new(
                harness.service.database_path(),
                harness.runtime_handle(),
                Arc::new(SilentEvents),
            )
            .unwrap();
            harness.service.attach_terminal_service(Arc::new(terminals));
            create_verifier(
                &harness.service,
                &harness.project,
                cli_verifier_draft(
                    "Contract",
                    "Contract checker",
                    0,
                    2,
                    "codex",
                    "gpt-5.6-luna",
                    "stage-verifier-fixture:reject-1-then-pass:t405",
                ),
            );
        },
    );
    h.wait_for_checkpoint("SPECIFICATION");
    h.approve();
    h.wait_for_checkpoint("Contract");
    let status = h.service.convergence_status(&h.pulse).unwrap();
    assert_eq!(
        status.retries, 0,
        "verifier rejection does not count as stage retry"
    );
    assert!(
        status.human_rounds.is_empty(),
        "verifier rejection does not count as human round"
    );
    assert_eq!(
        status.rounds, 0,
        "verifier rejection does not count as human round"
    );
    h.stop();
}

#[test]
fn t4_06_t4_07_crud_caps_multi_cli_adapters_project_delete_and_dock_validation() {
    let fixtures = Fixtures::install();
    let env = IsolatedTestEnvironment::new();
    std::fs::create_dir_all(env.workspace_dir().join("src")).unwrap();
    std::fs::write(
        env.workspace_dir().join("src/auth.rs"),
        composition_support::ORIGINAL_SOURCE,
    )
    .unwrap();

    let h = composition_support::Harness::start_prepared(
        env,
        vec![expiry_task()],
        CompositionSettings {
            enabled: true,
            stage_verifiers_enabled: true,
            ..CompositionSettings::default()
        }
        .with_all_recipes_qualified(),
        |facts, _, _| completed_reply(facts),
        |harness| {
            let terminals = TerminalService::new(
                harness.service.database_path(),
                harness.runtime_handle(),
                Arc::new(SilentEvents),
            )
            .unwrap();
            harness.service.attach_terminal_service(Arc::new(terminals));
        },
    );

    // Dock validation accepts verifier:<id> and rejects control chars.
    let save_dock = |agent_key: &str| {
        h.service
            .save_ui_session(SaveUiSessionRequest {
                project_id: h.project.clone(),
                workspace_root: h.workspace().to_str().unwrap().into(),
                active_tab_path: None,
                workspace_view_mode: "editor".into(),
                active_view: "workspace".into(),
                sidebar_collapsed: false,
                context_drawer_open: true,
                active_pulsation_id: None,
                selected_vault_path: None,
                active_chat_conversation_id: None,
                scroll_top: None,
                tabs: Vec::new(),
                auto_open_review: false,
                work_panel_tab: None,
                agent_dock: Some(AgentDockSession {
                    mode: None,
                    height: None,
                    follow: true,
                    agent: Some(agent_key.into()),
                }),
            })
            .is_ok()
    };
    assert!(save_dock("verifier:v1"));
    assert!(!save_dock("verifier:\x07"));

    // Multi-CLI adapters: Gemini (default thinking), OpenCode, Antigravity (unwrapped stdin).
    for (pos, (backend, model)) in [
        ("gemini", "gemini-2.5-flash"),
        ("opencode", "fixture-opencode-model"),
        ("antigravity", "gemini-2.5-pro"),
    ]
    .into_iter()
    .enumerate()
    {
        create_verifier(
            &h.service,
            &h.project,
            cli_verifier_draft(
                "SPECIFICATION",
                &format!("Verifier {backend}"),
                pos as i64,
                0,
                backend,
                model,
                &format!("stage-verifier-fixture:pass:t407_{backend}"),
            ),
        );
    }

    h.wait_for_checkpoint("SPECIFICATION");
    assert_eq!(fixtures.count("t407_gemini"), 1);
    assert_eq!(fixtures.count("t407_opencode"), 1);
    assert_eq!(fixtures.count("t407_antigravity"), 1);

    // Check remaining stage slots (3 used on SPECIFICATION -> 2 left before MAX_VERIFIERS_PER_STAGE=5).
    create_verifier(
        &h.service,
        &h.project,
        cli_verifier_draft("SPECIFICATION", "Spec 4", 3, 0, "codex", "gpt-5", "ok"),
    );
    create_verifier(
        &h.service,
        &h.project,
        cli_verifier_draft("SPECIFICATION", "Spec 5", 4, 0, "codex", "gpt-5", "ok"),
    );
    let over_stage = h
        .service
        .create_stage_verifier(
            &h.project,
            cli_verifier_draft("SPECIFICATION", "Spec 6", 5, 0, "codex", "gpt-5", "ok"),
        )
        .unwrap();
    assert!(matches!(
        over_stage,
        SaveStageVerifierOutcome::Invalid { .. }
    ));

    // NUL byte & trimmed uniqueness checks.
    let nul_err = h
        .service
        .create_stage_verifier(
            &h.project,
            cli_verifier_draft("Intake", "Bad\0Label", 0, 0, "codex", "gpt-5", "ok"),
        )
        .unwrap();
    assert!(matches!(nul_err, SaveStageVerifierOutcome::Invalid { .. }));
    let dup_err = h
        .service
        .create_stage_verifier(
            &h.project,
            cli_verifier_draft("SPECIFICATION", "  Spec 4 ", 6, 0, "codex", "gpt-5", "ok"),
        )
        .unwrap();
    assert!(matches!(dup_err, SaveStageVerifierOutcome::Invalid { .. }));

    // Cap 15 per project across remaining stages.
    for stage in ["Intake", "Contract", "Code", "Test"] {
        for idx in 0..MAX_VERIFIERS_PER_STAGE {
            let total = h.service.list_stage_verifiers(&h.project).unwrap().len();
            if total >= MAX_VERIFIERS_PER_PROJECT {
                break;
            }
            create_verifier(
                &h.service,
                &h.project,
                cli_verifier_draft(
                    stage,
                    &format!("{stage}-{idx}"),
                    idx as i64,
                    0,
                    "codex",
                    "gpt-5",
                    "ok",
                ),
            );
        }
    }
    let over_proj = h
        .service
        .create_stage_verifier(
            &h.project,
            cli_verifier_draft("Integrator", "Extra", 0, 0, "codex", "gpt-5", "ok"),
        )
        .unwrap();
    assert!(matches!(
        over_proj,
        SaveStageVerifierOutcome::Invalid { .. }
    ));

    // DELETE is restricted when run_count > 0.
    let gemini_v = h
        .service
        .list_stage_verifiers(&h.project)
        .unwrap()
        .into_iter()
        .find(|v| v.label == "Verifier gemini")
        .unwrap();
    assert!(gemini_v.run_count >= 1);
    assert!(
        h.service
            .delete_stage_verifier(&h.project, &gemini_v.id)
            .is_err()
    );

    // Ensure Labs can be toggled & project deletion cleans verifier tables.
    enable_stage_verifiers(&h.service, &h.project, false);
    h.approve();
    for stage in ["Contract", "Test", "Integrator"] {
        h.wait_for_checkpoint(stage);
        h.approve();
    }
    wait_until(&h.service, &h.pulse, |db| {
        db.query_row(
            "SELECT status='done' FROM pulsations WHERE id=?1",
            [&h.pulse],
            |r| r.get(0),
        )
        .unwrap()
    });
    let proj_name: String = h
        .db()
        .query_row("SELECT name FROM projects WHERE id=?1", [&h.project], |r| {
            r.get(0)
        })
        .unwrap();
    let stopped = h.stop();
    let db_path = stopped.service.database_path().to_path_buf();
    let proj_id = stopped.project.clone();
    let _ = PathBuf::from(&db_path);
    stopped
        .service
        .delete_project_permanently(&proj_id, &proj_name)
        .unwrap();
    let db = Connection::open(db_path).unwrap();
    let leftover: u32 = db
        .query_row(
            "SELECT count(*) FROM stage_verifiers WHERE project_id=?1",
            [&proj_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(leftover, 0);
}

#[test]
fn t4_06_multi_project_automation_ceiling_and_api_timeout_opens_verification_error() {
    let fixtures = Fixtures::install();
    let env = IsolatedTestEnvironment::new();
    std::fs::create_dir_all(env.workspace_dir().join("src")).unwrap();
    std::fs::write(
        env.workspace_dir().join("src/auth.rs"),
        composition_support::ORIGINAL_SOURCE,
    )
    .unwrap();

    // Scripted HTTP server: slow responder (> 1.5s) on slow-timeout verifier instructions.
    let server = ScriptedHttpServer::respond_with(|request| {
        if request.body.trim().is_empty() {
            return ScriptedResponse::json(400, r#"{"error":"empty"}"#);
        }
        let body: serde_json::Value = serde_json::from_str(&request.body).unwrap_or_default();
        let system = body["messages"][0]["content"].as_str().unwrap_or_default();
        let user = body["messages"][1]["content"].as_str().unwrap_or_default();
        if system.contains("stage-verifier-fixture:slow-api")
            || user.contains("stage-verifier-fixture:slow-api")
        {
            std::thread::sleep(Duration::from_millis(1600));
            return ScriptedResponse::json(200, r#"{"choices":[]}"#);
        }
        let facts = ikore_test_support::composition_replies::parse_prompt(request);
        completed_reply(&facts)
    });

    let deps = ikore_core::AppCoreDependencies::default();
    let svc = PersistenceService::initialize_with_dependencies(
        env.data_dir(),
        Arc::new(SilentEvents),
        Arc::new(MemoryCredentialStore::default()),
        Arc::new(FixedEmbeddingGateway::default()),
        deps.clock,
        deps.jitter,
        deps.http_client,
    )
    .unwrap();
    configure_loopback(&svc, &server);
    svc.update_preparation_settings(PreparationSettings {
        preparation_enabled: false,
        learning_enabled: false,
        ..PreparationSettings::default()
    })
    .unwrap();

    // Dedicated 1s-timeout loopback route (`localhost` origin keeps `127.0.0.1` circuit breaker clean).
    let slow_endpoint = server.url("").replace("127.0.0.1", "localhost");
    let db = Connection::open(svc.database_path()).unwrap();
    db.execute(
        "INSERT INTO provider_connections(id,provider_id,display_name,credential_ref,endpoint_url,default_model,enabled,state,created_at,updated_at)
         VALUES('slow-api-conn','ollama','Slow API','slow-api-cred',?1,'qwen2.5-coder:7b',1,'active','2026-11-25T00:00:00Z','2026-11-25T00:00:00Z')",
        [&slow_endpoint],
    )
    .unwrap();
    db.execute(
        "INSERT INTO provider_routes(id,name,strategy,max_attempts,attempt_timeout_ms,route_timeout_ms,enabled,version,created_at,updated_at)
         VALUES('slow-api-route','Slow API route','priority',1,1000,1000,1,1,'2026-11-25T00:00:00Z','2026-11-25T00:00:00Z')",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO provider_route_targets(route_id,position,connection_id,model_id,enabled,created_at,updated_at)
         VALUES('slow-api-route',0,'slow-api-conn','qwen2.5-coder:7b',1,'2026-11-25T00:00:00Z','2026-11-25T00:00:00Z')",
        [],
    )
    .unwrap();

    let project_a = svc
        .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
            workspace_path: env.workspace_dir().to_str().unwrap().into(),
            preferred_name: Some("Project A".into()),
        })
        .unwrap()
        .project
        .id;
    let ws_b = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(ws_b.path().join("src")).unwrap();
    std::fs::write(
        ws_b.path().join("src/auth.rs"),
        composition_support::ORIGINAL_SOURCE,
    )
    .unwrap();
    let project_b = svc
        .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
            workspace_path: ws_b.path().to_str().unwrap().into(),
            preferred_name: Some("Project B".into()),
        })
        .unwrap()
        .project
        .id;

    force_api_direct(&svc, &project_a);
    force_api_direct(&svc, &project_b);
    for proj in [&project_a, &project_b] {
        svc.update_composition_settings(
            proj,
            CompositionSettings {
                enabled: true,
                stage_verifiers_enabled: true,
                ..CompositionSettings::default()
            }
            .with_all_recipes_qualified(),
        )
        .unwrap();
    }
    svc.set_automation_max_concurrent(1).unwrap();

    // Project B has a manual custom pipeline + a stage verifier on Project A:
    // Project A's Intake verifier completes; API timeout opens verification_error without cli_fallback.
    create_verifier(
        &svc,
        &project_a,
        StageVerifierDraft {
            stage: "Intake".into(),
            label: "Slow API verifier".into(),
            position: 0,
            enabled: true,
            reject_limit: 2,
            agent: StepAgent {
                transport: StepTransport::Api,
                backend: None,
                model: None,
                model_profile: None,
                thinking_level: "default".into(),
                api_route_id: Some("slow-api-route".into()),
            },
            instructions: "stage-verifier-fixture:slow-api".into(),
        },
    );
    create_verifier(
        &svc,
        &project_b,
        cli_verifier_draft(
            "Intake",
            "Project B verifier",
            0,
            0,
            "codex",
            "gpt-5.6-luna",
            "stage-verifier-fixture:pass:t406_b",
        ),
    );

    let created_a = TaskService::new(svc.clone())
        .create(CreateTasksRequest {
            project_id: project_a.clone(),
            request_id: "t406-a".into(),
            tasks: vec![expiry_task()],
        })
        .unwrap();
    let pulse_a = svc
        .create_pulsation_from_tasks(CreatePulsationFromTasksRequest {
            project_id: project_a.clone(),
            request_id: "t406-pa".into(),
            title: "Timeout A".into(),
            tasks: created_a
                .iter()
                .map(|t| TaskSelection {
                    task_id: t.id.clone(),
                    expected_revision: t.revision,
                    manual_path_hint: None,
                })
                .collect(),
        })
        .unwrap()
        .id;
    let created_b = TaskService::new(svc.clone())
        .create(CreateTasksRequest {
            project_id: project_b.clone(),
            request_id: "t406-b".into(),
            tasks: vec![expiry_task()],
        })
        .unwrap();
    let pulse_b = svc
        .create_pulsation_from_tasks(CreatePulsationFromTasksRequest {
            project_id: project_b.clone(),
            request_id: "t406-pb".into(),
            title: "Ceiling B".into(),
            tasks: created_b
                .iter()
                .map(|t| TaskSelection {
                    task_id: t.id.clone(),
                    expected_revision: t.revision,
                    manual_path_hint: None,
                })
                .collect(),
        })
        .unwrap()
        .id;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let terminals = Arc::new(
        TerminalService::new(
            svc.database_path(),
            runtime.handle().clone(),
            Arc::new(SilentEvents),
        )
        .unwrap(),
    );
    svc.attach_terminal_service(terminals);
    let stop = CancellationToken::new();
    let w_svc = svc.clone();
    let w_stop = stop.clone();
    let worker = runtime.spawn(async move { w_svc.run_job_worker_until(w_stop).await });

    // Wait for slow API verifier to time out and open verification_error (no cli_fallback).
    let deadline = Instant::now() + Duration::from_secs(30);
    let cp = loop {
        let snap = svc
            .get_pulsation_runtime_snapshot(GetPulsationRuntimeSnapshotRequest {
                project_id: project_a.clone(),
            })
            .unwrap()
            .snapshot;
        if let Some(cp) = snap.verification_error_checkpoints.into_iter().next() {
            break cp;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for API timeout verification_error"
        );
        std::thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(cp.pulsation_id, pulse_a);
    assert!(!cp.error_message.is_empty());
    assert!(
        !cp.error_message
            .contains("No LLM target on this route is eligible."),
        "unexpected route eligibility error: {}",
        cp.error_message
    );
    let timeout_attempts: u32 = Connection::open(svc.database_path())
        .unwrap()
        .query_row(
            "SELECT count(*) FROM llm_attempts WHERE error_kind='provider_timeout'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        timeout_attempts >= 1,
        "expected at least 1 provider_timeout attempt in llm_attempts, got error: {}",
        cp.error_message
    );
    wait_for_checkpoint(&svc, &pulse_b, "SPECIFICATION");
    assert_eq!(fixtures.count("t406_b"), 1);

    // Abort from verification_error closes cleanly with receipt.
    let res = svc
        .resolve_verification_error_checkpoint(ResolveVerificationErrorRequest {
            checkpoint_id: cp.checkpoint_id,
            expected_state_version: cp.state_version,
            decision: VerificationErrorDecision::Abort,
        })
        .unwrap();
    assert_eq!(res.pulsation.status, PulsationStatus::Failed);

    stop.cancel();
    svc.cancel_all_active_jobs();
    runtime.block_on(async {
        let _ = tokio::time::timeout(Duration::from_secs(10), worker).await;
    });
    runtime.shutdown_timeout(Duration::from_secs(5));
}
