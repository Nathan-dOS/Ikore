//! ADR-037 Etapa 0 baseline measurements for the Agent Exchange milestones.
//!
//! Ignored by default. Run them explicitly, one at a time:
//! `cargo test --locked -p hvc-core --lib exchange_baseline -- --ignored --nocapture --test-threads=1`
//! Each test prints one `AGENT_EXCHANGE_BASELINE` JSON line. M1 to M3 rerun the
//! same harness and compare against the numbers recorded in the Etapa 0
//! report; nothing here asserts a threshold.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use hvc_terminal::host::TerminalHost;
use rusqlite::TransactionBehavior;
use serde_json::json;

use super::*;
use crate::llm::AgentRole;
use crate::terminals::{
    AgentExecutionMode, CliBackend, CreateTerminalRunRequest, ExecutionOrigin, TerminalEventKind,
    TerminalRepository, TerminalRun, TerminalRunStatus,
};

fn micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

fn summary(mut samples: Vec<Duration>) -> serde_json::Value {
    samples.sort_unstable();
    let count = samples.len();
    let at = |percent: usize| micros(samples[(count * percent).div_ceil(100).clamp(1, count) - 1]);
    let total: Duration = samples.iter().sum();
    json!({
        "n": count,
        "p50_us": at(50),
        "p95_us": at(95),
        "p99_us": at(99),
        "max_us": micros(samples[count - 1]),
        "mean_us": micros(total) / count as u64,
        "total_ms": micros(total) / 1_000,
    })
}

fn report(measurement: &str, result: serde_json::Value) {
    println!(
        "AGENT_EXCHANGE_BASELINE {}",
        json!({ "measurement": measurement, "result": result })
    );
}

fn project_id(service: &PersistenceService, workspace: &Path) -> String {
    service
        .create_project(CreateProjectRequest {
            workspace_path: workspace.to_string_lossy().into_owned(),
            name: Some("Exchange baseline".into()),
        })
        .expect("project")
        .project
        .id
}

fn running_cli_run(repository: &TerminalRepository, project_id: &str) -> TerminalRun {
    repository
        .ensure_project_configs(project_id, true)
        .expect("agent configs");
    let run = repository
        .create_run(CreateTerminalRunRequest {
            project_id: project_id.to_owned(),
            agent_role: AgentRole::BackendAgent,
            origin: ExecutionOrigin::Manual,
            execution_mode: AgentExecutionMode::CliProcess,
            backend: Some(CliBackend::Codex),
            model: None,
            pulsation_id: None,
            job_id: None,
            agent_run_id: None,
            llm_execution_id: None,
        })
        .expect("terminal run");
    for status in [TerminalRunStatus::Starting, TerminalRunStatus::Running] {
        repository
            .set_run_status(&run.id, status, None, None, None)
            .expect("run status");
    }
    run
}

/// The receipt a real CLI launch records when it prepares the input; the safe
/// snapshot refuses a run without it.
fn record_launch_receipt(service: &PersistenceService, project_id: &str, run: &TerminalRun) {
    let policy = crate::sanitization::SanitizationStore::new(service.database_path())
        .policy_for_project(project_id)
        .expect("sanitization policy");
    service
        .connection()
        .expect("connection")
        .execute(
            "INSERT INTO secret_sanitization_receipts(call_id,project_id,policy_identity,input_redaction_count,categories_json,examined_bytes,duration_ms,created_at) VALUES(?1,?2,?3,0,'[]',0,0,?4)",
            params![run.id, project_id, policy.identity(), Utc::now().to_rfc3339()],
        )
        .expect("launch receipt");
}

/// One committed transition, the unit the M2 append will join.
fn timed_transition(
    connection: &mut Connection,
    current: &Pulsation,
    status: PulsationStatus,
) -> (Pulsation, Duration) {
    let started = Instant::now();
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .expect("transaction");
    let next = PersistenceService::transition_pulsation(
        &transaction,
        current,
        status,
        current.stage,
        "exchange_baseline",
        None,
    )
    .expect("transition");
    transaction.commit().expect("commit");
    (next, started.elapsed())
}

/// `create_pulsation` may already return a planning pulsation.
fn to_running(connection: &mut Connection, mut pulsation: Pulsation) -> Pulsation {
    while pulsation.status != PulsationStatus::Running {
        let next = match pulsation.status {
            PulsationStatus::Idle => PulsationStatus::Planning,
            PulsationStatus::Planning | PulsationStatus::Checkpoint => PulsationStatus::Running,
            other => panic!("a new pulsation cannot be {other:?}"),
        };
        pulsation = timed_transition(connection, &pulsation, next).0;
    }
    pulsation
}

#[test]
#[ignore = "ADR-037 Etapa 0 baseline; run explicitly with --ignored --nocapture"]
fn exchange_baseline_pulsation_transitions() {
    let data = tempfile::tempdir().expect("data dir");
    let workspace = tempfile::tempdir().expect("workspace");
    let service = PersistenceService::initialize(data.path()).expect("service");
    let project_id = project_id(&service, workspace.path());
    let create = |feature_name: String| {
        service
            .create_pulsation(CreatePulsationRequest {
                project_id: project_id.clone(),
                feature_name,
            })
            .expect("pulsation")
            .pulsation
    };
    let mut connection = service.connection().expect("connection");

    // 1,000 non-terminal transitions on one pulsation: Running <-> Checkpoint.
    let mut current = to_running(&mut connection, create("Exchange baseline cycles".into()));
    let mut cycles = Vec::with_capacity(1_000);
    for index in 0..1_000 {
        let next = if index % 2 == 0 {
            PulsationStatus::Checkpoint
        } else {
            PulsationStatus::Running
        };
        let (pulsation, elapsed) = timed_transition(&mut connection, &current, next);
        current = pulsation;
        cycles.push(elapsed);
    }
    timed_transition(&mut connection, &current, PulsationStatus::Done);

    // 200 terminal transitions, one active pulsation at a time: the base purge
    // and the attestation intent run here.
    let mut terminal = Vec::with_capacity(200);
    for index in 0..200 {
        let pulsation = to_running(
            &mut connection,
            create(format!("Exchange baseline terminal {index}")),
        );
        terminal.push(timed_transition(&mut connection, &pulsation, PulsationStatus::Done).1);
    }

    report(
        "pulsation_transitions",
        json!({ "non_terminal": summary(cycles), "terminal_done": summary(terminal) }),
    );
}

#[test]
#[ignore = "ADR-037 Etapa 0 baseline; run explicitly with --ignored --nocapture"]
fn exchange_baseline_terminal_events() {
    let events: u64 = std::env::var("HVC_EXCHANGE_BASELINE_EVENTS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(100_000);
    let data = tempfile::tempdir().expect("data dir");
    let workspace = tempfile::tempdir().expect("workspace");
    let service = PersistenceService::initialize(data.path()).expect("service");
    let project_id = project_id(&service, workspace.path());
    let repository = TerminalRepository::new(service.database_path());
    let run = running_cli_run(&repository, &project_id);

    // A typical CLI stdout line; 100,000 of them stay under the 32 MiB run cap.
    let line = "   Compiling hvc-core v0.1.0 (C:/workspace/crates/hvc-core) build step";
    let mut appends = Vec::with_capacity(events as usize);
    let wall = Instant::now();
    for sequence in 1..=events {
        let content = format!("{line} {sequence}");
        let started = Instant::now();
        repository
            .append_event(
                &run.id,
                sequence,
                TerminalEventKind::Stdout,
                Some("stdout"),
                &content,
            )
            .expect("append");
        appends.push(started.elapsed());
    }
    let append_wall = wall.elapsed();

    // The re-read M2 relies on: one page of 500 after three cursors.
    let mut pages = serde_json::Map::new();
    for after in [0, events / 2, events.saturating_sub(500)] {
        let mut samples = Vec::with_capacity(5);
        for _ in 0..5 {
            let started = Instant::now();
            let page = repository.list_events(&run.id, after, 500).expect("page");
            samples.push(started.elapsed());
            assert!(!page.is_empty());
        }
        pages.insert(format!("after_{after}"), summary(samples));
    }

    report(
        "terminal_events",
        json!({
            "events": events,
            "append": summary(appends),
            "append_wall_ms": micros(append_wall) / 1_000,
            "list_events_page_500": pages,
        }),
    );
}

#[test]
#[ignore = "ADR-037 Etapa 0 baseline; set HVC_EXCHANGE_BASELINE_WORKSPACE and run explicitly"]
fn exchange_baseline_snapshot_manifest() {
    let Some(workspace) = std::env::var_os("HVC_EXCHANGE_BASELINE_WORKSPACE") else {
        eprintln!("HVC_EXCHANGE_BASELINE_WORKSPACE is not set; nothing was measured.");
        return;
    };
    let workspace = PathBuf::from(workspace);
    let data = tempfile::tempdir().expect("data dir");
    let service = PersistenceService::initialize(data.path()).expect("service");
    let project_id = project_id(&service, &workspace);
    let repository = TerminalRepository::new(service.database_path());
    let run = running_cli_run(&repository, &project_id);
    record_launch_receipt(&service, &project_id, &run);

    let started = Instant::now();
    let preparation = crate::terminals_host::CoreTerminalHost
        .prepare_snapshot(
            service.database_path(),
            &workspace,
            &run,
            10_000,
            64 * 1024 * 1024,
        )
        .expect("snapshot");
    let elapsed = started.elapsed();

    // D9: hunks only for text up to 32 KiB, stored up to 1 MiB per run.
    const SMALL_TEXT: usize = 32 * 1024;
    const BLOB_BUDGET: usize = 1024 * 1024;
    let mut small = preparation
        .files
        .iter()
        .map(|file| file.projected_content.len())
        .filter(|len| *len <= SMALL_TEXT)
        .collect::<Vec<_>>();
    small.sort_unstable();
    let (mut kept, mut kept_bytes) = (0usize, 0usize);
    for len in &small {
        if kept_bytes + len > BLOB_BUDGET {
            break;
        }
        kept += 1;
        kept_bytes += len;
    }
    let paths = preparation
        .files
        .iter()
        .map(|file| file.relative_path.to_string_lossy().replace('\\', "/"))
        .collect::<Vec<_>>();
    let paths_json = serde_json::to_vec(&json!({ "files": paths }))
        .expect("json")
        .len();
    // An M3 manifest row: path, two hex digests, a length and an optional blob digest.
    let rows_bytes = paths
        .iter()
        .map(|path| path.len() + 64 + 64 + 8 + 64)
        .sum::<usize>();

    report(
        "snapshot_manifest",
        json!({
            "workspace_files": preparation.files.len(),
            "original_bytes": preparation.original_bytes,
            "projected_bytes": preparation.files.iter().map(|file| file.projected_content.len()).sum::<usize>(),
            "small_text_files": small.len(),
            "small_text_bytes": small.iter().sum::<usize>(),
            "blobs_within_1_mib": kept,
            "blob_bytes_within_1_mib": kept_bytes,
            "paths_json_bytes": paths_json,
            "per_file_rows_estimated_bytes": rows_bytes,
            "prepare_snapshot_ms": micros(elapsed) / 1_000,
        }),
    );
}
