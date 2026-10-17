//! M5: real supervisor/router/SQLite, loopback provider and isolated, identical repositories.
//! No cfg(test) stage fallback, real credentials, network services or SQL workflow transitions.
use crate::{
    FixedEmbeddingGateway, IsolatedTestEnvironment, MemoryCredentialStore, RecordedRequest,
    ScriptedHttpServer, ScriptedResponse,
};
use hvc_core::{
    AppCoreDependencies,
    llm::{LlmEventEnvelope, LlmEventSink},
    orchestration::*,
    persistence_service::*,
};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

pub const FIXTURE_VERSION: &str = "m5-128-sources-v1";
pub const IDLE_WINDOW_MS: u64 = 1500;
pub const PROVIDER_DELAY_MS: u64 = 10;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Scenario {
    NoQueue,
    QueueAbc,
    EditWhileWaiting,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Sample {
    pub scenario: Scenario,
    pub preparation_enabled: bool,
    pub iteration: usize,
    pub warmup: bool,
    pub fixture_sha256: String,
    /// Monotonic observations of committed base epoch -> accepted Specification checkpoint.
    pub base_to_spec_ms: Vec<f64>,
    pub approval_to_base_ms: Vec<f64>,
    pub hitl_hold_ms: Vec<f64>,
    pub preparation_ready_before_release: Vec<bool>,
    pub context_metrics: Vec<Value>,
    pub provider_calls: u64,
    pub simulated_input_tokens: u64,
    pub simulated_output_tokens: u64,
    pub preemptions: u64,
    pub publication_conflicts: u64,
}

struct SilentEvents;
impl LlmEventSink for SilentEvents {
    fn emit(&self, _: &LlmEventEnvelope) {}
}

/// Used by benchmarks and the UI fixture; intentionally no arbitrary model-generated writes.
pub fn stage_response(request: &RecordedRequest) -> ScriptedResponse {
    let body: Value = serde_json::from_str(&request.body).expect("provider request JSON");
    let model = body["model"]
        .as_str()
        .expect("provider request model identity");
    let prompt = body["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .filter_map(|m| m["content"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let stage = prompt
        .lines()
        .find_map(|l| l.strip_prefix("- stage: "))
        .expect("stage prompt");
    let kind = match stage {
        "SPECIFICATION" => "specification",
        "Contract" => "contract",
        _ => "stage_note",
    };
    let artifacts = if stage == "Intake" {
        json!([])
    } else {
        json!([{"name":stage,"kind":kind,"relativePath":null,"content":format!("# {stage}\nFixture: reject an expired token with status 401. No-op; do not run Git or tests.")}])
    };
    let mut output = json!({"outputLanguage":"en","summary":format!("{stage} fixture"),"statusUpdate":if stage=="Intake" {"advance"}else{"checkpoint_ready"},"contextUsage":[],"notes":[],"artifacts":artifacts});
    if stage == "SPECIFICATION" {
        output["componentProposals"] = json!([]);
    }
    // Simulated usage, not a tokenizer or a price estimate.
    ScriptedResponse::json(200, json!({"model":model,"choices":[{"message":{"content":output.to_string()},"finish_reason":"stop"}],"usage":{"prompt_tokens":prompt.chars().count().div_ceil(4),"completion_tokens":80}}).to_string())
}

fn source_fixture(root: &std::path::Path) -> String {
    let mut revisions = Vec::new();
    for index in 0..128 {
        let (path, content) = if index == 0 {
            (
                "auth.rs".to_string(),
                "pub fn validate_token() -> bool { false } // M5_CURRENT\n".to_string(),
            )
        } else {
            (
                format!("module_{index:03}.rs"),
                format!(
                    "pub fn unrelated_{index}() {{\n{}\n}}\n",
                    "let padding = 42;\n".repeat(200)
                ),
            )
        };
        fs::write(root.join(&path), &content).expect("source fixture");
        revisions.push((path, ContentSha256::of(content.as_bytes())));
    }
    ContentSha256::of(&serde_json::to_vec(&revisions).unwrap())
        .as_str()
        .into()
}

pub fn configure_loopback(service: &PersistenceService, server: &ScriptedHttpServer) {
    // Configuration fixture only. All user actions and transitions below use the typed facade.
    let db = Connection::open(service.database_path()).unwrap();
    db.execute("UPDATE provider_connections SET provider_id='ollama',endpoint_url=?1 WHERE id='legacy-openai'", [server.url("")]).unwrap();
    db.execute(
        "UPDATE runtime_settings SET value='off' WHERE key='runtime.routing_v2_mode'",
        [],
    )
    .unwrap();
}

/// Pins every agent of a project to API transport.
///
/// Benchmarks that drive a full `AppCore` own a `TerminalService`, so a stage
/// resolves its transport through `ensure_project_configs`, whose default is
/// `cli_process` whenever a Codex binary is discovered on the host. On a
/// developer machine with Codex installed the benchmark would therefore launch
/// the real CLI instead of the scripted loopback, fail, and open a
/// `cli_fallback` HITL checkpoint that stalls the pulsation at Intake. Pinning
/// the transport keeps the fixture deterministic regardless of what is
/// installed on the machine running the suite.
///
/// Call after the project exists; `configure_loopback` runs before that.
pub fn force_api_direct(service: &PersistenceService, project_id: &str) {
    // Configuration fixture only, like `configure_loopback` above.
    let db = Connection::open(service.database_path()).unwrap();
    let now = chrono::Utc::now().to_rfc3339();
    for role in [
        "intake_router",
        "task_specifier",
        "contract_agent",
        "backend_agent",
        "test_agent",
        "integrator",
        "learning_agent",
        "visualizer",
    ] {
        db.execute(
            "INSERT INTO cli_agent_configs(
               project_id,agent_role,execution_mode,backend,thinking_level,
               custom_args_json,custom_risk_ack,version,created_at,updated_at
             ) VALUES(?1,?2,'api_direct','codex','default','[]',0,1,?3,?3)
             ON CONFLICT(project_id,agent_role)
             DO UPDATE SET execution_mode='api_direct',updated_at=?3",
            params![project_id, role, now],
        )
        .unwrap();
    }
}

fn wait_until(mut predicate: impl FnMut() -> bool) -> Instant {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if predicate() {
            return Instant::now();
        }
        assert!(
            Instant::now() < deadline,
            "M5 fixture timed out; inspect worker diagnostics"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn checkpoint(service: &PersistenceService, pulse: &str, stage: &str) -> Instant {
    let db = Connection::open(service.database_path()).unwrap();
    let started = Instant::now();
    wait_until(|| {
        if started.elapsed() > Duration::from_secs(14) {
            let jobs: Vec<_> = db
                .prepare("SELECT kind,status,last_error FROM jobs WHERE pulsation_id=?1")
                .unwrap()
                .query_map([pulse], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                    ))
                })
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
            panic!("waiting {stage}, state={state:?}, jobs={jobs:?}");
        }
        db.query_row("SELECT EXISTS(SELECT 1 FROM pulsations p JOIN checkpoints c ON c.pulsation_id=p.id WHERE p.id=?1 AND p.stage=?2 AND p.status='checkpoint' AND c.status='waiting' AND c.kind='artifact_review')", [pulse, stage], |r| r.get(0)).unwrap()
    })
}

fn approve(service: &PersistenceService, pulse: &str) {
    let c = service.get_hitl_checkpoint(pulse).unwrap();
    service
        .apply_hitl_decision(ApplyHitlDecisionRequest {
            pulsation_id: pulse.into(),
            checkpoint_id: c.checkpoint_id,
            expected_state_version: c.state_version,
            feedback: None,
            decision: HitlDecision::Approve,
        })
        .unwrap();
}

fn to_integrator(service: &PersistenceService, pulse: &str) {
    for stage in ["SPECIFICATION", "Contract", "Test"] {
        checkpoint(service, pulse, stage);
        approve(service, pulse);
    }
    checkpoint(service, pulse, "Integrator");
}

fn create(service: &PersistenceService, project: &str, label: &str) -> String {
    service
        .create_pulsation(CreatePulsationRequest {
            project_id: project.into(),
            feature_name: format!(
                "crie validação em auth.rs quando token expirado deve retornar 401 ({label})"
            ),
        })
        .unwrap()
        .pulsation
        .id
}

fn release_base(
    service: &PersistenceService,
    pulse: &str,
    epoch: u64,
    sample: &mut Sample,
) -> Instant {
    let start = Instant::now();
    approve(service, pulse);
    let db = Connection::open(service.database_path()).unwrap();
    let observed = wait_until(|| {
        db.query_row(
            "SELECT EXISTS(SELECT 1 FROM project_pipeline_state WHERE base_epoch>=?1)",
            [epoch],
            |r| r.get(0),
        )
        .unwrap()
    });
    sample
        .approval_to_base_ms
        .push(observed.duration_since(start).as_secs_f64() * 1000.0);
    observed
}

fn idle_window(service: &PersistenceService, successor: &str, sample: &mut Sample) {
    let start = Instant::now();
    std::thread::sleep(Duration::from_millis(IDLE_WINDOW_MS));
    sample
        .hitl_hold_ms
        .push(start.elapsed().as_secs_f64() * 1000.0);
    {
        let db = Connection::open(service.database_path()).unwrap();
        let state: String = db
            .query_row(
                "SELECT prespec_state FROM pulsation_preparation WHERE pulsation_id=?1",
                [successor],
                |r| r.get(0),
            )
            .unwrap();
        // Keep misses in the denominator; never extend only the enabled variant's idle window.
        sample
            .preparation_ready_before_release
            .push(state == "ready");
    }
}

pub fn run_sample(scenario: Scenario, enabled: bool, iteration: usize, warmup: bool) -> Sample {
    eprintln!("fixture {scenario:?} preparation={enabled}");
    let env = IsolatedTestEnvironment::new();
    let mut sample = Sample {
        scenario,
        preparation_enabled: enabled,
        iteration,
        warmup,
        fixture_sha256: source_fixture(env.workspace_dir()),
        base_to_spec_ms: vec![],
        approval_to_base_ms: vec![],
        hitl_hold_ms: vec![],
        preparation_ready_before_release: vec![],
        context_metrics: vec![],
        provider_calls: 0,
        simulated_input_tokens: 0,
        simulated_output_tokens: 0,
        preemptions: 0,
        publication_conflicts: 0,
    };
    let server = ScriptedHttpServer::respond_with(|request| {
        std::thread::sleep(Duration::from_millis(PROVIDER_DELAY_MS));
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
            preparation_enabled: enabled,
            learning_enabled: false,
            ..PreparationSettings::default()
        })
        .unwrap();
    let project = service
        .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
            workspace_path: env.workspace_dir().to_str().unwrap().into(),
            preferred_name: Some("M5 isolated fixture".into()),
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
    let worker = runtime.spawn(async move { worker_service.run_job_worker_until(stop).await });
    if scenario == Scenario::NoQueue {
        // Empty project is already ready: origin immediately before typed creation, includes TaskRefine.
        let ready = Instant::now();
        let a = create(&service, &project, "A");
        sample.base_to_spec_ms.push(
            checkpoint(&service, &a, "SPECIFICATION")
                .duration_since(ready)
                .as_secs_f64()
                * 1000.0,
        );
    } else {
        let a = create(&service, &project, "A");
        to_integrator(&service, &a);
        let b = create(&service, &project, "B");
        let c = create(&service, &project, "C");
        idle_window(&service, &b, &mut sample);
        let db = Connection::open(service.database_path()).unwrap();
        let untouched: bool = db
            .query_row(
                "SELECT stage='Intake' AND specification_started=0 FROM pulsations WHERE id=?1",
                [&c],
                |r| r.get(0),
            )
            .unwrap();
        assert!(untouched, "C cannot anticipate while A owns the project");
        if scenario == Scenario::EditWhileWaiting {
            let path = env.workspace_dir().join("auth.rs");
            let metadata = fs::metadata(&path).unwrap();
            fs::write(
                &path,
                "pub fn validate_token() -> bool { true  } // M5_CURRENT\n",
            )
            .unwrap();
            fs::OpenOptions::new()
                .write(true)
                .open(path)
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(metadata.modified().unwrap()))
                .unwrap();
        }
        let base = release_base(&service, &a, 1, &mut sample);
        sample.base_to_spec_ms.push(
            checkpoint(&service, &b, "SPECIFICATION")
                .duration_since(base)
                .as_secs_f64()
                * 1000.0,
        );
        to_integrator(&service, &b);
        idle_window(&service, &c, &mut sample);
        let base = release_base(&service, &b, 2, &mut sample);
        sample.base_to_spec_ms.push(
            checkpoint(&service, &c, "SPECIFICATION")
                .duration_since(base)
                .as_secs_f64()
                * 1000.0,
        );
        let disposition: String = db
            .query_row(
                "SELECT context_disposition FROM pulsation_preparation WHERE pulsation_id=?1",
                [&b],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            disposition,
            if !sample.preparation_ready_before_release[0] {
                "on_demand"
            } else if scenario == Scenario::EditWhileWaiting {
                "stale_on_demand"
            } else {
                "reused"
            }
        );
        if scenario == Scenario::EditWhileWaiting {
            let handoff: String = db.query_row("SELECT handoff_json FROM stage_acceptances s JOIN jobs j ON j.id=s.job_id WHERE j.pulsation_id=?1 AND j.kind='run_stage::SPECIFICATION'", [&b], |r| r.get(0)).unwrap();
            assert!(handoff.contains("true  }"), "must use edited body");
            assert!(!handoff.contains("false }"), "must discard stale body");
        }
    }
    drop(stop_on_drop);
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(15), worker)
            .await
            .unwrap()
            .unwrap()
    });
    let db = Connection::open(service.database_path()).unwrap();
    sample.context_metrics = db.prepare("SELECT e.message,substr(p.feature_name,-2,1) FROM pulse_events e JOIN pulsations p ON p.id=e.pulsation_id WHERE e.event_type='context_collected' ORDER BY e.created_at,e.id").unwrap().query_map([], |r| Ok((r.get::<_, String>(0)?,r.get::<_,String>(1)?))).unwrap().map(|row| {
        let (message,label) = row.unwrap();
        let mut v: Value = serde_json::from_str(&message).unwrap();
        v.as_object_mut().unwrap().remove("job_id");
        v["pulse_label"] = json!(label);
        v
    }).collect();
    (sample.provider_calls,sample.simulated_input_tokens,sample.simulated_output_tokens) = db.query_row("SELECT COUNT(*),COALESCE(SUM(input_tokens),0),COALESCE(SUM(output_tokens),0) FROM llm_attempts", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    sample.preemptions = db
        .query_row(
            "SELECT COUNT(*) FROM learning_runs WHERE state='preempted'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    sample.publication_conflicts = db
        .query_row(
            "SELECT COUNT(*) FROM artifact_publications WHERE state='conflict'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        sample.provider_calls,
        if scenario == Scenario::NoQueue { 2 } else { 14 }
    );
    assert_eq!(sample.publication_conflicts, 0);
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM runtime_slots", [], |r| r
            .get::<_, usize>(0))
            .unwrap(),
        0
    );
    assert!(
        !env.workspace_dir().join("knowledge").exists(),
        "benchmark never publishes"
    );
    sample
}

pub fn percentile(values: &[f64], percentile: usize) -> f64 {
    assert!(!values.is_empty() && (1..=100).contains(&percentile));
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted[(sorted.len() * percentile).div_ceil(100) - 1]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nearest_rank_percentiles_do_not_sum_parallel_jobs() {
        assert_eq!(percentile(&[4.0, 1.0, 3.0, 2.0], 50), 2.0);
        assert_eq!(percentile(&[4.0, 1.0, 3.0, 2.0], 95), 4.0);
    }
}
