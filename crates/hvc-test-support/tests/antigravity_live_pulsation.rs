#![allow(
    clippy::unwrap_used,
    reason = "Test-only integration helpers fail immediately when fixture setup is invalid"
)]
//! ADR-010/022 live qualification: a pipeline v4 pulsation whose canonical
//! roles, Learning included, all run on Antigravity's CLI (`agy`), driven by
//! the same job worker the desktop app runs. Opt-in, it spends real quota:
//!   cargo test -p hvc-test-support --test `antigravity_live_pulsation` -- --ignored --nocapture --test-threads=1
//! `HVC_LIVE_DB_OUT=<directory>` keeps a copy of each database for
//! `analyze_pulsation --db <copy>`.
mod composition_support;

use composition_support::*;
use hvc_core::llm::AgentRole;
use hvc_core::orchestration::PreparationSettings;
use hvc_core::orchestration::composition::settings::CompositionSettings;
use hvc_core::terminals::{
    AgentExecutionMode, CliBackend, CliModelSelectionSource, TerminalService, ThinkingLevel,
    UpdateCliAgentConfigRequest,
};
use hvc_test_support::IsolatedTestEnvironment;
use hvc_test_support::pulsation_analysis::{Selector, analyze, render_text};
use rusqlite::OptionalExtension;
use std::sync::Arc;
use std::time::{Duration, Instant};

const FLASH: (&str, &str) = ("gemini-3.8-flash", "low");
const SONNET: (&str, &str) = ("claude-sonnet-5-5", "low");

/// Every canonical role but the Feature agent, which no pulsation runs.
const ROLES: [AgentRole; 7] = [
    AgentRole::IntakeRouter,
    AgentRole::TaskSpecifier,
    AgentRole::ContractAgent,
    AgentRole::BackendAgent,
    AgentRole::TestAgent,
    AgentRole::Integrator,
    AgentRole::LearningAgent,
];

fn start(selection: impl Fn(AgentRole) -> (&'static str, &'static str)) -> Harness {
    let env = IsolatedTestEnvironment::new();
    std::fs::create_dir_all(env.workspace_dir().join("src")).unwrap();
    std::fs::write(env.workspace_dir().join("src/auth.rs"), ORIGINAL_SOURCE).unwrap();
    Harness::start_prepared(
        env,
        vec![expiry_task()],
        CompositionSettings {
            enabled: true,
            ..CompositionSettings::default()
        }
        .with_all_recipes_qualified(),
        // Every role runs on the CLI; an API call would be a routing bug.
        |facts, _, _| {
            eprintln!("unexpected API call from {}", facts.executor);
            provider_reply(serde_json::json!({ "error": "API transport must not be used" }))
        },
        |harness| {
            harness
                .service
                .update_preparation_settings(PreparationSettings {
                    preparation_enabled: false,
                    learning_enabled: true,
                    ..PreparationSettings::default()
                })
                .unwrap();
            let runtime = harness.runtime_handle();
            let terminals = Arc::new(
                TerminalService::new(
                    harness.service.database_path(),
                    runtime.clone(),
                    Arc::new(SilentEvents),
                )
                .unwrap(),
            );
            terminals
                .repository()
                .ensure_project_configs(&harness.project, false)
                .unwrap();
            harness.service.attach_terminal_service(terminals.clone());
            runtime.block_on(terminals.refresh_discovery());
            runtime
                .block_on(terminals.model_catalog(&harness.project, CliBackend::Antigravity, true))
                .unwrap();
            for role in ROLES {
                let (model, profile) = selection(role);
                let current = terminals
                    .repository()
                    .get_config(&harness.project, role)
                    .unwrap();
                terminals
                    .update_config(UpdateCliAgentConfigRequest {
                        project_id: harness.project.clone(),
                        agent_role: role,
                        execution_mode: AgentExecutionMode::CliProcess,
                        backend: CliBackend::Antigravity,
                        model: Some(model.into()),
                        model_profile: Some(profile.into()),
                        model_source: CliModelSelectionSource::Catalog,
                        thinking_level: ThinkingLevel::Default,
                        custom_executable: None,
                        custom_args: Vec::new(),
                        custom_risk_ack: false,
                        expected_version: current.version,
                    })
                    .unwrap_or_else(|error| panic!("{}: {error}", role.as_str()));
            }
        },
    )
}

/// Approves every artifact review until the pulsation ends, stops at a stage
/// failure, or stops once `stop_at` asks for review. Returns the final
/// `(stage, status)`.
fn drive(harness: &Harness, stop_at: Option<&str>, limit: Duration) -> (String, String) {
    let started = Instant::now();
    let db = harness.db();
    loop {
        let (stage, status): (String, String) = db
            .query_row(
                "SELECT stage,status FROM pulsations WHERE id=?1",
                [&harness.pulse],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        let waiting: Option<String> = db
            .query_row(
                "SELECT kind FROM checkpoints WHERE pulsation_id=?1 AND status='waiting' ORDER BY created_at DESC LIMIT 1",
                [&harness.pulse],
                |r| r.get(0),
            )
            .optional()
            .unwrap();
        let elapsed = started.elapsed().as_secs_f64();
        match (status.as_str(), waiting.as_deref()) {
            ("checkpoint", Some("artifact_review")) if stop_at == Some(stage.as_str()) => {
                eprintln!("[{elapsed:>7.1} s] {stage} reached review; stopping here");
                return (stage, status);
            }
            ("checkpoint", Some("artifact_review")) => {
                eprintln!("[{elapsed:>7.1} s] approving {stage}");
                harness.approve();
            }
            ("checkpoint", Some(kind)) => {
                eprintln!("[{elapsed:>7.1} s] {stage} parked at a `{kind}` checkpoint");
                return (stage, status);
            }
            ("done" | "failed" | "cancelled", _) => {
                eprintln!("[{elapsed:>7.1} s] pulsation {status} at {stage}");
                return (stage, status);
            }
            _ => {}
        }
        if started.elapsed() > limit {
            eprintln!("[{elapsed:>7.1} s] time limit while {stage} is {status}");
            return (stage, status);
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn wait_for_learning(harness: &Harness, limit: Duration) -> Option<String> {
    let started = Instant::now();
    let db = harness.db();
    loop {
        let state: Option<String> = db
            .query_row(
                "SELECT state FROM learning_runs WHERE pulsation_id=?1 ORDER BY created_at DESC LIMIT 1",
                [&harness.pulse],
                |r| r.get(0),
            )
            .optional()
            .unwrap();
        if let Some(state) = &state
            && !matches!(state.as_str(), "pending" | "running" | "publishing")
        {
            return Some(state.clone());
        }
        if started.elapsed() > limit {
            return state;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// One line per CLI run: role, outcome, wall time, tokens and the model `init` named.
fn report(harness: &Harness, label: &str) {
    let db = harness.db();
    let mut statement = db
        .prepare(
            "SELECT t.agent_role,t.status,COALESCE(t.error_code,''),t.requested_model_profile,
                    COALESCE(t.observed_model,''),
                    (julianday(t.finished_at)-julianday(t.started_at))*86400.0,
                    COALESCE((SELECT SUM(input_tokens) FROM token_usage_records u WHERE u.source_kind='terminal_run' AND u.source_id=t.id),0),
                    COALESCE((SELECT SUM(output_tokens) FROM token_usage_records u WHERE u.source_kind='terminal_run' AND u.source_id=t.id),0),
                    COALESCE((SELECT SUM(cache_read_tokens) FROM token_usage_records u WHERE u.source_kind='terminal_run' AND u.source_id=t.id),0),
                    COALESCE((SELECT SUM(reasoning_tokens) FROM token_usage_records u WHERE u.source_kind='terminal_run' AND u.source_id=t.id),0)
             FROM terminal_runs t WHERE t.project_id=?1 ORDER BY t.created_at",
        )
        .unwrap();
    let rows = statement
        .query_map([&harness.project], |r| {
            Ok(format!(
                "{:<15} {:<10} {:<28} {:<7} {:<18} {:>7.1} s  in {:>7}  out {:>6}  cache {:>7}  thinking {:>6}",
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                r.get::<_, String>(4)?,
                r.get::<_, Option<f64>>(5)?.unwrap_or_default(),
                r.get::<_, i64>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, i64>(8)?,
                r.get::<_, i64>(9)?,
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect::<Vec<_>>();
    eprintln!("--- {label}: {} CLI runs", rows.len());
    rows.iter().for_each(|row| eprintln!("{row}"));
    let mut attempts = db
        .prepare(
            "SELECT a.executor_kind,a.state,COALESCE(a.error_code,''),substr(COALESCE(a.diagnostic,''),1,240)
             FROM stage_work_unit_attempts a JOIN stage_executions e ON e.id=a.execution_id
             WHERE e.pulsation_id=?1 ORDER BY a.created_at",
        )
        .unwrap();
    attempts
        .query_map([&harness.pulse], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .for_each(|(kind, state, code, diagnostic)| {
            eprintln!("attempt {kind:<22} {state:<10} {code} {diagnostic}")
        });
    match analyze(
        harness.service.database_path(),
        &Selector::Id(harness.pulse.clone()),
    ) {
        Ok(analysis) => eprintln!("{}", render_text(&analysis)),
        Err(error) => eprintln!("analysis unavailable: {error}"),
    }
    if let Ok(directory) = std::env::var("HVC_LIVE_DB_OUT") {
        let target = std::path::Path::new(&directory).join(format!("{label}.sqlite"));
        std::fs::create_dir_all(&directory).unwrap();
        db.execute("VACUUM INTO ?1", [target.to_string_lossy()])
            .unwrap();
        eprintln!("database copied to {}", target.display());
    }
}

#[test]
#[ignore = "Requires local Antigravity (agy) authentication and network; runs a whole pulsation on Gemini Flash"]
fn authenticated_antigravity_runs_a_whole_pulsation_on_gemini_flash() {
    let harness = start(|_| FLASH);
    let (stage, status) = drive(&harness, None, Duration::from_secs(60 * 60));
    let learning = (status == "done")
        .then(|| wait_for_learning(&harness, Duration::from_secs(15 * 60)))
        .flatten();
    eprintln!("learning run: {learning:?}");
    report(&harness, "antigravity-gemini-flash-pulsation");
    assert!(
        harness.calls().is_empty(),
        "API calls: {:?}",
        harness.calls()
    );
    assert_eq!(status, "done", "stopped at {stage}");
}

#[test]
#[ignore = "Requires local Antigravity (agy) authentication, network and Claude/GPT quota; runs Intake and Specification"]
fn authenticated_antigravity_runs_the_specification_stage_on_claude_sonnet() {
    let harness = start(|role| {
        if role == AgentRole::TaskSpecifier {
            SONNET
        } else {
            FLASH
        }
    });
    let (stage, status) = drive(
        &harness,
        Some("SPECIFICATION"),
        Duration::from_secs(30 * 60),
    );
    report(&harness, "antigravity-sonnet-specification");
    assert!(
        harness.calls().is_empty(),
        "API calls: {:?}",
        harness.calls()
    );
    assert_eq!(
        (stage.as_str(), status.as_str()),
        ("SPECIFICATION", "checkpoint")
    );
}
