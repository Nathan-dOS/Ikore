#![allow(
    clippy::unwrap_used,
    reason = "Test-only integration helpers fail immediately when fixture setup is invalid"
)]
//! Feature 25 (M2a): the developer's pipelines on the automation lane, with
//! official CLIs played by the `cli_fixture` binary on PATH and an API step
//! answered by a scripted loopback provider.
mod composition_support;

use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use composition_support::{SilentEvents, completed_reply, expiry_task, wait_for_checkpoint};
use ikore_core::custom_pipelines::{
    AgentStep, CustomPipelineDefinitionV1, CustomPipelineView, DecidePipelineGateRequest, GateStep,
    PipelineRunStatus, PipelineRunView, PipelineStep, PipelineTrigger, SaveCustomPipelineOutcome,
    SaveCustomPipelineRequest, StepAgent, StepOutput, StepRunStatus, StepTransport, Verdict,
};
use ikore_core::orchestration::composition::settings::CompositionSettings;
use ikore_core::persistence_service::{CreateProjectRequest, PersistenceService};
use ikore_core::terminals::TerminalService;
use ikore_test_support::pulsation_benchmark::configure_loopback;
use ikore_test_support::{
    FixedEmbeddingGateway, IsolatedTestEnvironment, MemoryCredentialStore, ScriptedHttpServer,
    ScriptedResponse,
};
use rusqlite::Connection;
use serde_json::json;
use tokio_util::sync::CancellationToken;

/// PATH is process-wide: the tests that put fixtures on it run one at a time.
static PATH_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

struct PathRestore(Option<std::ffi::OsString>);

impl Drop for PathRestore {
    fn drop(&mut self) {
        // SAFETY: PATH_LOCK serializes every PATH mutation of this binary.
        unsafe {
            match &self.0 {
                Some(path) => std::env::set_var("PATH", path),
                None => std::env::remove_var("PATH"),
            }
        }
    }
}

/// Copies of the fixture under every official CLI's name, first on PATH.
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
        // SAFETY: PATH_LOCK is held, and PathRestore puts the value back.
        unsafe { std::env::set_var("PATH", std::env::join_paths(paths).unwrap()) };
        Self {
            directory,
            _restore: PathRestore(original),
            _lock: lock,
        }
    }

    /// What the fixture recorded of every pipeline step it ran as `backend`.
    fn log(&self, backend: &str) -> String {
        std::fs::read_to_string(
            self.directory
                .path()
                .join(format!("{backend}.pipeline-log")),
        )
        .unwrap_or_default()
    }

    fn path(&self) -> &Path {
        self.directory.path()
    }
}

struct Harness {
    _data: tempfile::TempDir,
    workspace: tempfile::TempDir,
    service: PersistenceService,
    project: String,
    runtime: Option<tokio::runtime::Runtime>,
    worker: Option<tokio::task::JoinHandle<()>>,
    stop: CancellationToken,
}

impl Harness {
    fn start() -> Self {
        let data = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(workspace.path().join("src")).unwrap();
        std::fs::write(workspace.path().join("src/lib.rs"), "pub fn fixture() {}\n").unwrap();
        let deps = ikore_core::AppCoreDependencies::default();
        let service = PersistenceService::initialize_with_dependencies(
            data.path(),
            Arc::new(SilentEvents),
            Arc::new(MemoryCredentialStore::default()),
            Arc::new(FixedEmbeddingGateway::default()),
            deps.clock,
            deps.jitter,
            deps.http_client,
        )
        .unwrap();
        let project = service
            .create_project(CreateProjectRequest {
                workspace_path: workspace.path().to_string_lossy().into_owned(),
                name: Some("Pipelines".into()),
            })
            .unwrap()
            .project
            .id;
        let mut harness = Self {
            _data: data,
            workspace,
            service,
            project,
            runtime: None,
            worker: None,
            stop: CancellationToken::new(),
        };
        harness.boot();
        harness
    }

    /// Starts the app's runtime: a terminal service and the supervisor.
    fn boot(&mut self) {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let terminals = TerminalService::new(
            self.service.database_path(),
            runtime.handle().clone(),
            Arc::new(SilentEvents),
        )
        .unwrap();
        self.service.attach_terminal_service(Arc::new(terminals));
        self.stop = CancellationToken::new();
        let worker = self.service.clone();
        let stop = self.stop.clone();
        self.worker = Some(runtime.spawn(async move { worker.run_job_worker_until(stop).await }));
        self.runtime = Some(runtime);
    }

    /// Closes the app as `AppCore::shutdown` does: the supervisor stops
    /// first, and its lanes are joined before the runtime goes away.
    fn shut_down(&mut self) {
        self.stop.cancel();
        self.service.cancel_all_active_jobs();
        if let Some(runtime) = self.runtime.take() {
            if let Some(worker) = self.worker.take() {
                runtime.block_on(async {
                    let _ = tokio::time::timeout(Duration::from_secs(30), worker).await;
                });
            }
            runtime.shutdown_timeout(Duration::from_secs(10));
        }
    }

    fn db(&self) -> Connection {
        Connection::open(self.service.database_path()).unwrap()
    }

    fn create(&self, name: &str, steps: Vec<PipelineStep>) -> CustomPipelineView {
        match self
            .service
            .save_custom_pipeline(SaveCustomPipelineRequest {
                project_id: self.project.clone(),
                pipeline_id: None,
                expected_version: None,
                name: name.into(),
                description: String::new(),
                definition: CustomPipelineDefinitionV1 {
                    schema_version: 1,
                    trigger: PipelineTrigger::Manual,
                    steps,
                },
            })
            .unwrap()
        {
            SaveCustomPipelineOutcome::Saved { pipeline } => *pipeline,
            SaveCustomPipelineOutcome::Invalid { errors } => panic!("{errors:?}"),
        }
    }

    fn run(&self, pipeline: &CustomPipelineView) -> String {
        self.service
            .start_custom_pipeline_run(&self.project, &pipeline.id)
            .unwrap()
    }

    fn view(&self, run: &str) -> PipelineRunView {
        self.service
            .custom_pipeline_run(&self.project, run)
            .unwrap()
    }

    fn wait(
        &self,
        run: &str,
        what: &str,
        mut done: impl FnMut(&PipelineRunView) -> bool,
    ) -> PipelineRunView {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let view = self.view(run);
            if done(&view) {
                return view;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}: {view:#?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_status(&self, run: &str, status: PipelineRunStatus) -> PipelineRunView {
        self.wait(run, status.as_str(), |view| view.status == status)
    }
}

fn agent(id: &str, backend: &str, marker: &str, output: StepOutput) -> PipelineStep {
    PipelineStep::Agent(AgentStep {
        id: id.into(),
        label: format!("Step {id}"),
        agent: StepAgent {
            transport: StepTransport::Cli,
            backend: Some(backend.into()),
            model: Some(
                if backend == "claude" {
                    "haiku"
                } else {
                    "gpt-5.6-luna"
                }
                .into(),
            ),
            model_profile: None,
            thinking_level: "default".into(),
            api_route_id: None,
        },
        instructions: format!("Follow the fixture. pipeline-step-fixture:{marker}"),
        output,
    })
}

fn gate(id: &str) -> PipelineStep {
    PipelineStep::Gate(GateStep {
        id: id.into(),
        label: format!("Gate {id}"),
        prompt: "Read the summary.".into(),
    })
}

fn terminal_run(db: &Connection, id: &str) -> (String, String, Option<String>, String) {
    db.query_row(
        "SELECT origin,slot_kind,agent_role,status FROM terminal_runs WHERE id=?1",
        [id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )
    .unwrap()
}

#[test]
fn steps_run_in_order_on_their_own_slots_and_a_gate_waits_for_the_developer() {
    let fixtures = Fixtures::install();
    let mut h = Harness::start();
    let pipeline = h.create(
        "Release checks",
        vec![
            agent("summary", "claude", "text", StepOutput::Text),
            gate("approve"),
            agent("verdict", "codex", "pass", StepOutput::Verdict),
        ],
    );
    let run = h.run(&pipeline);
    let waiting = h.wait_status(&run, PipelineRunStatus::WaitingGate);
    let summary = waiting.step(0).unwrap();
    assert_eq!(summary.status, StepRunStatus::Done);
    assert_eq!(
        summary.output_text.as_deref(),
        Some("Step text from the fixture.")
    );
    let db = h.db();
    let first_run = summary.terminal_run_id.clone().unwrap();
    assert_eq!(
        terminal_run(&db, &first_run),
        (
            "automation".into(),
            "automation".into(),
            None,
            "completed".into()
        )
    );
    h.service
        .decide_custom_pipeline_gate(DecidePipelineGateRequest {
            project_id: h.project.clone(),
            run_id: run.clone(),
            step_index: 1,
            approve: true,
            note: Some("Also look at the README.".into()),
        })
        .unwrap();
    let done = h.wait_status(&run, PipelineRunStatus::Done);
    let verdict = done.step(2).unwrap();
    assert_eq!(verdict.status, StepRunStatus::Passed);
    let parsed = verdict.verdict.clone().unwrap();
    assert_eq!(parsed.verdict, Verdict::Pass);
    assert_eq!(parsed.findings[0].file.as_deref(), Some("src/lib.rs"));
    // The second step read the first one's output and the gate's note, as
    // data and as the developer's words; it ran read-only.
    let codex = fixtures.log("codex");
    assert!(codex.contains("--sandbox read-only"), "{codex}");
    assert!(codex.contains("--ignore-user-config"), "{codex}");
    assert!(
        codex.contains("## Previous step output: `Step summary`"),
        "{codex}"
    );
    assert!(codex.contains("Step text from the fixture."), "{codex}");
    assert!(
        codex.contains(
            "## Gate note: `Gate approve`\n> provenance: human \u{b7} checkpoint feedback"
        ),
        "{codex}"
    );
    assert!(codex.contains("Also look at the README."), "{codex}");
    assert!(fixtures.log("claude").contains("--strict-mcp-config"));
    // Each step keeps its own slot, and no run published anything.
    let slots: Vec<(String, String)> = db
        .prepare("SELECT id,label FROM terminal_slots WHERE kind='automation' ORDER BY id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(slots.len(), 2);
    assert!(
        slots
            .iter()
            .any(|(_, label)| label == "Release checks \u{b7} Step summary")
    );
    let published: u32 = db
        .query_row("SELECT count(*) FROM artifact_publications", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(published, 0);
    assert_eq!(
        std::fs::read_to_string(h.workspace.path().join("src/lib.rs")).unwrap(),
        "pub fn fixture() {}\n"
    );
    h.shut_down();
}

#[test]
fn a_rejecting_verdict_fails_the_run_and_an_invalid_one_gets_one_correction() {
    let _fixtures = Fixtures::install();
    let mut h = Harness::start();
    let rejecting = h.create(
        "Rejecting",
        vec![
            agent("check", "claude", "reject", StepOutput::Verdict),
            gate("never"),
        ],
    );
    let run = h.run(&rejecting);
    let failed = h.wait_status(&run, PipelineRunStatus::Failed);
    assert_eq!(
        failed.error.as_deref(),
        Some("Rejected by Step check: Found a problem.")
    );
    let step = failed.step(0).unwrap();
    assert_eq!(step.status, StepRunStatus::Rejected);
    let finding = &step.verdict.as_ref().unwrap().findings[0];
    // `missing.rs` is not in the workspace: the path is dropped and marked.
    assert!(finding.file.is_none() && finding.file_not_found);
    assert!(failed.step(1).is_none());

    let corrected = h.create(
        "Corrected",
        vec![agent(
            "check",
            "claude",
            "invalid-then-pass",
            StepOutput::Verdict,
        )],
    );
    let run = h.run(&corrected);
    let done = h.wait_status(&run, PipelineRunStatus::Done);
    assert_eq!(done.step(0).unwrap().status, StepRunStatus::Passed);
    let calls: u32 = h
        .db()
        .query_row(
            "SELECT count(*) FROM terminal_runs WHERE origin='automation' AND slot_id LIKE ?1",
            [format!("automation:%:{}:check", corrected.id)],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(calls, 2);

    let invalid = h.create(
        "Invalid",
        vec![agent("check", "claude", "invalid", StepOutput::Verdict)],
    );
    let run = h.run(&invalid);
    let failed = h.wait_status(&run, PipelineRunStatus::Failed);
    let error = failed.step(0).unwrap().error.clone().unwrap();
    assert!(
        error.starts_with("The verdict was invalid twice"),
        "{error}"
    );
    h.shut_down();
}

#[test]
fn cancel_stops_the_running_step() {
    let _fixtures = Fixtures::install();
    let mut h = Harness::start();
    let pipeline = h.create(
        "Slow",
        vec![agent("slow", "claude", "hang", StepOutput::Text)],
    );
    let run = h.run(&pipeline);
    h.wait(&run, "a running step", |view| {
        view.step(0)
            .is_some_and(|step| step.status == StepRunStatus::Running)
    });
    std::thread::sleep(Duration::from_millis(300));
    h.service
        .cancel_custom_pipeline_run(&h.project, &run)
        .unwrap();
    let cancelled = h.wait(&run, "the cancelled step", |view| {
        view.step(0)
            .is_some_and(|step| step.status == StepRunStatus::Cancelled)
    });
    assert_eq!(cancelled.status, PipelineRunStatus::Cancelled);
    let db = h.db();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let open: u32 = db
            .query_row(
                "SELECT count(*) FROM terminal_runs WHERE origin='automation'
                  AND status IN ('queued','starting','running')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let slots: u32 = db
            .query_row("SELECT count(*) FROM automation_slots", [], |row| {
                row.get(0)
            })
            .unwrap();
        if open == 0 && slots == 0 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the step's process is still open"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    h.shut_down();
}

#[test]
fn a_step_the_closing_app_stops_is_interrupted_and_resumes_as_a_new_attempt() {
    let fixtures = Fixtures::install();
    let mut h = Harness::start();
    let pipeline = h.create(
        "Resumable",
        vec![agent("long", "claude", "hang-once", StepOutput::Text)],
    );
    let run = h.run(&pipeline);
    h.wait(&run, "a running step", |view| {
        view.step(0)
            .is_some_and(|step| step.status == StepRunStatus::Running)
    });
    // The fixture marks its first launch before it hangs.
    let marker = fixtures.path().join("pipeline.hung-once");
    let deadline = Instant::now() + Duration::from_secs(60);
    while !marker.exists() {
        assert!(Instant::now() < deadline, "the step's CLI never started");
        std::thread::sleep(Duration::from_millis(20));
    }
    h.shut_down();
    let interrupted = h.view(&run);
    assert_eq!(
        interrupted.status,
        PipelineRunStatus::Interrupted,
        "{interrupted:#?}"
    );
    assert_eq!(
        interrupted.step(0).unwrap().status,
        StepRunStatus::Interrupted
    );
    // The next attempt answers.
    h.boot();
    h.service
        .resume_custom_pipeline_run(&h.project, &run)
        .unwrap();
    let done = h.wait_status(&run, PipelineRunStatus::Done);
    let retry = done.step(0).unwrap();
    assert_eq!((retry.attempt, retry.status), (2, StepRunStatus::Done));
    h.shut_down();
}

#[test]
fn the_main_lane_admits_the_ikore_pipeline_while_a_step_runs() {
    let _fixtures = Fixtures::install();
    // The Ikore pipeline, on the scripted API provider, reaches its first
    // checkpoint while the automation lane holds a step of the same project.
    let env = IsolatedTestEnvironment::new();
    std::fs::create_dir_all(env.workspace_dir().join("src")).unwrap();
    std::fs::write(
        env.workspace_dir().join("src/auth.rs"),
        "pub fn validate_token() {}\n",
    )
    .unwrap();
    let composition = composition_support::Harness::start_prepared(
        env,
        vec![expiry_task()],
        CompositionSettings {
            enabled: true,
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
            let created = match harness
                .service
                .save_custom_pipeline(SaveCustomPipelineRequest {
                    project_id: harness.project.clone(),
                    pipeline_id: None,
                    expected_version: None,
                    name: "Busy lane".into(),
                    description: String::new(),
                    definition: CustomPipelineDefinitionV1 {
                        schema_version: 1,
                        trigger: PipelineTrigger::Manual,
                        steps: vec![agent("busy", "claude", "hang", StepOutput::Text)],
                    },
                })
                .unwrap()
            {
                SaveCustomPipelineOutcome::Saved { pipeline } => *pipeline,
                SaveCustomPipelineOutcome::Invalid { errors } => panic!("{errors:?}"),
            };
            harness
                .service
                .start_custom_pipeline_run(&harness.project, &created.id)
                .unwrap();
        },
    );
    wait_for_checkpoint(&composition.service, &composition.pulse, "SPECIFICATION");
    let db = composition.db();
    let held: u32 = db
        .query_row(
            "SELECT count(*) FROM automation_slots WHERE project_id=?1",
            [&composition.project],
            |row| row.get(0),
        )
        .unwrap();
    let running: u32 = db
        .query_row(
            "SELECT count(*) FROM custom_pipeline_step_runs WHERE status='running'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        (held, running),
        (1, 1),
        "the step kept running while the pipeline advanced"
    );
    let main_slots: u32 = db
        .query_row(
            "SELECT count(*) FROM runtime_slots WHERE job_class='main'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(main_slots <= 1);
    composition.stop();
}

#[test]
fn an_api_step_runs_through_the_route_it_names() {
    let server = ScriptedHttpServer::respond_with(|request| {
        let body: serde_json::Value = serde_json::from_str(&request.body).unwrap_or_default();
        let system = body["messages"][0]["content"].as_str().unwrap_or_default();
        if !system.contains("a step of the developer-defined pipeline `API checks`") {
            return ScriptedResponse::json(400, r#"{"error":"unexpected request"}"#);
        }
        // A step names its route, so it always routes (and streams).
        ScriptedResponse::sse([
            format!(
                "data: {}

",
                json!({"model":"gpt-4.1-mini","choices":[{"delta":{"content":"API step answer."}}]})
            ),
            format!(
                "data: {}

data: [DONE]

",
                json!({"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":3,"total_tokens":13}})
            ),
        ])
    });
    let mut h = Harness::start();
    configure_loopback(&h.service, &server);
    // The route the legacy configuration seeds; its first target is the
    // loopback provider.
    let route = "legacy-default".to_owned();
    let PipelineStep::Agent(mut step) = agent("api", "claude", "text", StepOutput::Text) else {
        unreachable!();
    };
    step.agent.transport = StepTransport::Api;
    step.agent.backend = None;
    step.agent.model = None;
    step.agent.api_route_id = Some(route.clone());
    let pipeline = h.create("API checks", vec![PipelineStep::Agent(step)]);
    let run = h.run(&pipeline);
    let done = h.wait_status(&run, PipelineRunStatus::Done);
    let step = done.step(0).unwrap();
    assert_eq!(step.output_text.as_deref(), Some("API step answer."));
    let execution = step.llm_execution_id.clone().unwrap();
    let (route_id, slot): (String, Option<String>) = h
        .db()
        .query_row(
            "SELECT route_id,automation_slot_id FROM llm_route_executions WHERE id=?1",
            [&execution],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(route_id, route);
    assert!(slot.unwrap().ends_with(&format!("{}:api", pipeline.id)));
    h.shut_down();
}
