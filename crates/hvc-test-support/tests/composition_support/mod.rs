//! Shared harness for the pipeline v4 integration tests: a scripted
//! loopback provider whose replies are valid `UnitModelResponseV1` documents
//! built from the handles the host rendered, plus the waits and approvals
//! the legacy checkpoints need.
#![allow(dead_code)]
use hvc_core::{
    AppCoreDependencies,
    feature_forge::{
        AcceptanceCriterion, CreatePulsationFromTasksRequest, CreateTasksRequest, TaskDraft,
        TaskKind, TaskSelection, TaskService,
    },
    llm::{LlmEventEnvelope, LlmEventSink},
    orchestration::{PreparationSettings, composition::settings::CompositionSettings},
    persistence_service::*,
};
use hvc_test_support::pulsation_benchmark::{configure_loopback, force_api_direct};
use hvc_test_support::{
    FixedEmbeddingGateway, IsolatedTestEnvironment, MemoryCredentialStore, RecordedRequest,
    ScriptedHttpServer, ScriptedResponse,
};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

pub struct SilentEvents;
impl LlmEventSink for SilentEvents {
    fn emit(&self, _: &LlmEventEnvelope) {}
}
impl hvc_core::terminals::TerminalEventSink for SilentEvents {
    fn emit(&self, _: &hvc_core::terminals::TerminalEventEnvelope) {}
}

pub use hvc_test_support::composition_replies::*;

pub fn wait_until(
    service: &PersistenceService,
    pulse: &str,
    mut predicate: impl FnMut(&Connection) -> bool,
) {
    let db = Connection::open(service.database_path()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if predicate(&db) {
            return;
        }
        if Instant::now() > deadline {
            let jobs: Vec<(String, String, Option<String>)> = db
                .prepare("SELECT kind,status,last_error FROM jobs WHERE pulsation_id=?1")
                .unwrap()
                .query_map([pulse], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .unwrap()
                .map(Result::unwrap)
                .collect();
            let executions: Vec<(String, String, Option<String>)> = db
                .prepare("SELECT stage,status,blocked_reason FROM stage_executions WHERE pulsation_id=?1")
                .unwrap()
                .query_map([pulse], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .unwrap()
                .map(Result::unwrap)
                .collect();
            let state: (String, String) = db
                .query_row(
                    "SELECT stage,status FROM pulsations WHERE id=?1",
                    [pulse],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            let attempts: Vec<(String, Option<String>)> = db
                .prepare("SELECT outcome,error_kind FROM llm_attempts ORDER BY started_at")
                .unwrap()
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .map(Result::unwrap)
                .collect();
            let outputs: Vec<(Option<String>, Option<String>)> = db
                .prepare("SELECT validation_error,substr(raw_output,1,400) FROM agent_run_outputs ORDER BY created_at")
                .unwrap()
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .map(Result::unwrap)
                .collect();
            panic!(
                "timed out; state={state:?} jobs={jobs:?} executions={executions:?} attempts={attempts:?} outputs={outputs:?}"
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

pub fn wait_for_checkpoint(service: &PersistenceService, pulse: &str, stage: &str) {
    wait_until(service, pulse, |db| {
        db.query_row(
            "SELECT EXISTS(SELECT 1 FROM pulsations p JOIN checkpoints c ON c.pulsation_id=p.id WHERE p.id=?1 AND p.stage=?2 AND p.status='checkpoint' AND c.status='waiting' AND c.kind='artifact_review')",
            [pulse, stage],
            |r| r.get(0),
        )
        .unwrap()
    });
}

pub fn approve(service: &PersistenceService, pulse: &str) {
    let checkpoint = service.get_hitl_checkpoint(pulse).unwrap();
    service
        .apply_hitl_decision(ApplyHitlDecisionRequest {
            pulsation_id: pulse.into(),
            checkpoint_id: checkpoint.checkpoint_id,
            expected_state_version: checkpoint.state_version,
            feedback: None,
            decision: HitlDecision::Approve,
        })
        .unwrap();
}

/// The default single task of the fixture.
pub fn expiry_task() -> TaskDraft {
    TaskDraft {
        title: "Reject expired tokens".into(),
        kind: TaskKind::Bug,
        description: "validate_token in src/auth.rs accepts every token; add the expiry check and a unit test in tests/auth_test.rs".into(),
        criteria: vec![AcceptanceCriterion {
            id: "ac-1".into(),
            description: "An expired token is rejected by validate_token".into(),
        }],
        dependencies: vec![],
        feature_id: None,
    }
}

/// A v4 project with one source file, one task with one criterion and a
/// running job worker. `responder` builds each provider reply from the
/// parsed prompt and the 1-based ordinal of the call.
pub struct Harness {
    pub env: IsolatedTestEnvironment,
    pub service: PersistenceService,
    pub project: String,
    pub task_ids: Vec<String>,
    pub pulse: String,
    pub observed: Arc<Mutex<Vec<String>>>,
    _server: ScriptedHttpServer,
    _runtime: tokio::runtime::Runtime,
    _stop: tokio_util::sync::DropGuard,
}

pub fn completed_reply(facts: &PromptFacts) -> ScriptedResponse {
    if facts.executor == VISUALIZER {
        return provider_reply(visualizer_response(&facts.user_prompt));
    }
    provider_reply(unit_response(facts))
}

pub fn provider_reply(content: Value) -> ScriptedResponse {
    ScriptedResponse::json(
        200,
        json!({"model":"gpt-4.1-mini","choices":[{"message":{"content":content.to_string()},"finish_reason":"stop"}],"usage":{"prompt_tokens":120,"completion_tokens":60}}).to_string(),
    )
}

impl Harness {
    pub fn start(
        responder: impl Fn(&PromptFacts, usize, &RecordedRequest) -> ScriptedResponse
        + Send
        + Sync
        + 'static,
    ) -> Self {
        let env = IsolatedTestEnvironment::new();
        fs::create_dir_all(env.workspace_dir().join("src")).unwrap();
        fs::write(env.workspace_dir().join("src/auth.rs"), ORIGINAL_SOURCE).unwrap();
        Self::start_in(env, vec![expiry_task()], responder)
    }

    /// The fixture's task id when the batch has exactly one task.
    pub fn task_id(&self) -> &str {
        &self.task_ids[0]
    }

    pub fn workspace(&self) -> &Path {
        self.env.workspace_dir()
    }

    pub fn start_in(
        env: IsolatedTestEnvironment,
        tasks: Vec<TaskDraft>,
        responder: impl Fn(&PromptFacts, usize, &RecordedRequest) -> ScriptedResponse
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self::start_with(
            env,
            tasks,
            CompositionSettings {
                enabled: true,
                ..CompositionSettings::default()
            }
            .with_all_recipes_qualified(),
            responder,
        )
    }

    /// Like `start_in`, with explicit composition settings (e.g. the
    /// in-flight cap for the parallel engine).
    pub fn start_with(
        env: IsolatedTestEnvironment,
        tasks: Vec<TaskDraft>,
        settings: CompositionSettings,
        responder: impl Fn(&PromptFacts, usize, &RecordedRequest) -> ScriptedResponse
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self::start_prepared(env, tasks, settings, responder, |_| {})
    }

    /// Like `start_with`, running `prepare` after the pulsation is queued but
    /// before the job worker starts, so a role routed there is in place before
    /// its stage is planned (routing after `start_with` races the first stages).
    pub fn start_prepared(
        env: IsolatedTestEnvironment,
        tasks: Vec<TaskDraft>,
        settings: CompositionSettings,
        responder: impl Fn(&PromptFacts, usize, &RecordedRequest) -> ScriptedResponse
        + Send
        + Sync
        + 'static,
        prepare: impl FnOnce(&Self),
    ) -> Self {
        let observed: Arc<Mutex<Vec<String>>> = Arc::default();
        let seen = observed.clone();
        let server = ScriptedHttpServer::respond_with(move |request| {
            // A client that gave up (cancellation) leaves an empty request;
            // answer it without parsing so the fixture thread survives.
            if request.body.trim().is_empty() {
                return ScriptedResponse::json(400, r#"{"error":"empty request"}"#);
            }
            let facts = parse_prompt(request);
            let ordinal = {
                let mut seen = seen.lock().unwrap();
                seen.push(facts.executor.clone());
                seen.len()
            };
            responder(&facts, ordinal, request)
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
                preferred_name: Some("Composition fixture".into()),
            })
            .unwrap()
            .project
            .id;
        force_api_direct(&service, &project);
        service
            .update_composition_settings(&project, settings)
            .unwrap();

        let created = TaskService::new(service.clone())
            .create(CreateTasksRequest {
                project_id: project.clone(),
                request_id: "create-fixture-tasks".into(),
                tasks,
            })
            .unwrap();
        let pulse = service
            .create_pulsation_from_tasks(CreatePulsationFromTasksRequest {
                project_id: project.clone(),
                request_id: "dispatch-fixture".into(),
                title: "Token expiry".into(),
                tasks: created
                    .iter()
                    .map(|task| TaskSelection {
                        task_id: task.id.clone(),
                        expected_revision: task.revision,
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
        let stop = CancellationToken::new();
        let guard = stop.clone().drop_guard();
        let harness = Self {
            env,
            service,
            project,
            task_ids: created.into_iter().map(|task| task.id).collect(),
            pulse,
            observed,
            _server: server,
            _runtime: runtime,
            _stop: guard,
        };
        prepare(&harness);
        let worker_service = harness.service.clone();
        harness
            ._runtime
            .spawn(async move { worker_service.run_job_worker_until(stop).await });
        harness
    }

    pub fn db(&self) -> Connection {
        Connection::open(self.service.database_path()).unwrap()
    }

    /// The scripted provider's URL for `path`.
    pub fn server_url(&self, path: &str) -> String {
        self._server.url(path)
    }

    /// Routes one stage role through the CLI transport: the `cli_fixture`
    /// binary in bridge mode forwards the prompt the host writes to its stdin
    /// to this harness's scripted provider, so the same responder scripts
    /// both transports. Configuration fixture only, like `force_api_direct`.
    pub fn route_role_to_cli_bridge(&self, role: &str) {
        // The supervisor only reaches a CLI when a terminal service is
        // attached, as `AppCore` does at startup.
        let terminals = hvc_core::terminals::TerminalService::new(
            self.service.database_path(),
            self._runtime.handle().clone(),
            Arc::new(SilentEvents),
        )
        .unwrap();
        terminals
            .repository()
            .ensure_project_configs(&self.project, false)
            .unwrap();
        self.service.attach_terminal_service(Arc::new(terminals));
        let args = json!([
            "--mode",
            "bridge",
            "--bridge-url",
            self.server_url("/v1/chat/completions"),
            "--fixture-model",
            "gpt-4.1-mini"
        ]);
        self.db()
            .execute(
                "UPDATE cli_agent_configs SET execution_mode='cli_process',backend='custom',model='gpt-4.1-mini',custom_executable=?3,custom_args_json=?4,custom_risk_ack=1,version=version+1 WHERE project_id=?1 AND agent_role=?2",
                rusqlite::params![
                    self.project,
                    role,
                    env!("CARGO_BIN_EXE_cli_fixture"),
                    args.to_string()
                ],
            )
            .unwrap();
    }

    /// Routes one stage role through an official CLI backend found on PATH (the test puts a
    /// `cli_fixture` copy there under the backend's name). Attaches a terminal service like
    /// `route_role_to_cli_bridge` does.
    pub fn route_role_to_official_cli(
        &self,
        role: &str,
        backend: &str,
    ) -> Arc<hvc_core::terminals::TerminalService> {
        let model = match backend {
            "claude" => "haiku",
            "codex" => "gpt-5.6-luna",
            "gemini" => "gemini-2.5-flash",
            "opencode" => "fixture-opencode-model",
            other => panic!("unknown official CLI fixture backend: {other}"),
        };
        let terminals = hvc_core::terminals::TerminalService::new(
            self.service.database_path(),
            self._runtime.handle().clone(),
            Arc::new(SilentEvents),
        )
        .unwrap();
        terminals
            .repository()
            .ensure_project_configs(&self.project, false)
            .unwrap();
        let terminals = Arc::new(terminals);
        self.service.attach_terminal_service(terminals.clone());
        self.db()
            .execute(
                "UPDATE cli_agent_configs SET execution_mode='cli_process',backend=?3,model=?4,custom_executable=NULL,custom_args_json='[]',version=version+1 WHERE project_id=?1 AND agent_role=?2",
                rusqlite::params![self.project, role, backend, model],
            )
            .unwrap();
        terminals
    }

    /// The runtime the worker runs on, for tests that must await host APIs.
    pub fn runtime_handle(&self) -> tokio::runtime::Handle {
        self._runtime.handle().clone()
    }

    pub fn calls(&self) -> Vec<String> {
        self.observed.lock().unwrap().clone()
    }

    pub fn wait_for_checkpoint(&self, stage: &str) {
        wait_for_checkpoint(&self.service, &self.pulse, stage);
    }

    pub fn approve(&self) {
        approve(&self.service, &self.pulse);
    }

    /// Waits for a `stage_failure` checkpoint and returns its id.
    pub fn wait_for_failure_checkpoint(&self) -> String {
        let mut id = None;
        wait_until(&self.service, &self.pulse, |db| {
            id = db
                .query_row(
                    "SELECT c.id FROM checkpoints c JOIN pulsations p ON p.id=c.pulsation_id WHERE p.id=?1 AND p.status='checkpoint' AND c.status='waiting' AND c.kind='stage_failure'",
                    [&self.pulse],
                    |r| r.get::<_, String>(0),
                )
                .ok();
            id.is_some()
        });
        id.unwrap()
    }

    pub fn state_version(&self) -> u64 {
        self.db()
            .query_row(
                "SELECT state_version FROM pulsations WHERE id=?1",
                [&self.pulse],
                |r| r.get(0),
            )
            .unwrap()
    }
}
