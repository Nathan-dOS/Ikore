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
    AgentExecutionMode, CliBackend, CreateTerminalRunRequest, ExecutionOrigin, TerminalEvent,
    TerminalEventKind, TerminalRepository, TerminalRun, TerminalRunStatus,
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

/// Writes stdout events `from..=to` of a run in one transaction. The M2
/// measurements read and follow events; they do not need the per-append
/// commit `exchange_baseline_terminal_events` measures.
fn bulk_events(service: &PersistenceService, run_id: &str, from: u64, to: u64) -> Vec<TerminalEvent> {
    let line = "   Compiling hvc-core v0.1.0 (C:/workspace/crates/hvc-core) build step";
    let mut connection = service.connection().expect("connection");
    let transaction = connection.transaction().expect("transaction");
    let mut events = Vec::with_capacity((to + 1 - from) as usize);
    {
        let mut statement = transaction
            .prepare(
                "INSERT INTO terminal_events(id,run_id,sequence,kind,stream,content,created_at,redaction_count,redaction_categories_json)
                 VALUES(?1,?2,?3,'stdout','stdout',?4,?5,0,'[]')",
            )
            .expect("statement");
        for sequence in from..=to {
            let event = TerminalEvent {
                id: Uuid::new_v4().to_string(),
                run_id: run_id.to_owned(),
                sequence,
                kind: TerminalEventKind::Stdout,
                stream: Some("stdout".into()),
                content: format!("{line} {sequence}"),
                redaction_count: 0,
                redaction_categories: Vec::new(),
                created_at: Utc::now().to_rfc3339(),
            };
            statement
                .execute(params![event.id, run_id, sequence, event.content, event.created_at])
                .expect("event");
            events.push(event);
        }
    }
    transaction.commit().expect("commit");
    events
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

/// ADR-037 M2: a ranged re-read of a run recorded under a known policy, one
/// page of 500 at spread cursors of a 100,000-event run. Acceptance: p95 at
/// most 50 ms.
#[test]
#[ignore = "ADR-037 M2 measurement; run explicitly with --ignored --nocapture"]
fn exchange_m2_terminal_reread() {
    let events: u64 = 100_000;
    let data = tempfile::tempdir().expect("data dir");
    let workspace = tempfile::tempdir().expect("workspace");
    let service = PersistenceService::initialize(data.path()).expect("service");
    let project_id = project_id(&service, workspace.path());
    let repository = TerminalRepository::new(service.database_path());
    let run = running_cli_run(&repository, &project_id);
    record_launch_receipt(&service, &project_id, &run);
    bulk_events(&service, &run.id, 1, events);
    // A fixed, spread set of cursors (no RNG dependency), each read twice.
    let mut samples = Vec::new();
    for step in 0..50u64 {
        let after = (step * 7_919) % (events - 500);
        for _ in 0..2 {
            let started = Instant::now();
            let page = repository.list_events(&run.id, after, 500).expect("page");
            samples.push(started.elapsed());
            assert_eq!(page.len(), 500);
            assert_eq!(page[0].sequence, after + 1);
        }
    }
    report(
        "m2_terminal_reread",
        json!({ "events": events, "page_500": summary(samples) }),
    );
}

/// What the load rehearsal's subscriber saw.
#[derive(Debug, Default)]
struct SubscriberTally {
    cursor: i64,
    last_terminal: u64,
    lagged: u64,
    gaps: u64,
    reread_events: u64,
    resets: u64,
    transitions: u64,
    /// The run's final status arrived; its tail must be read once.
    run_settled: bool,
    tail_read: bool,
}

/// Reads the log from `after` until the follower is current; false when
/// the log asks for a snapshot.
fn follow_log(
    service: &PersistenceService,
    project_id: &str,
    follower: &mut crate::exchange::log::ExchangeFollower,
    mut after: i64,
    tally: &mut SubscriberTally,
) -> bool {
    use crate::exchange::log::{PageAction, StreamKind};
    loop {
        let page = service
            .exchange_read_after(project_id, after, 500)
            .expect("read");
        tally.transitions += page
            .items
            .iter()
            .filter(|item| item.stream_kind == StreamKind::Pulsation)
            .count() as u64;
        tally.run_settled |= page
            .items
            .iter()
            .any(|item| item.stream_kind == StreamKind::TerminalRun);
        match follower.on_page(&page) {
            PageAction::Snapshot => return false,
            PageAction::Continue(next) => after = next,
            PageAction::Done => return true,
        }
    }
}

/// ADR-037 M2 load rehearsal: a slow subscriber on the default event bus
/// while a run writes 100,000 terminal events and a pulsation makes 1,000
/// transitions. The subscriber follows the exchange log and repairs every
/// terminal gap by ranged re-read; acceptance is no full reload (no reset),
/// every event reached and every transition seen once.
#[test]
#[ignore = "ADR-037 M2 load rehearsal; run explicitly with --ignored --nocapture"]
fn exchange_m2_load_slow_subscriber() {
    use crate::events::{AppEvent, AppEventBus};
    use crate::exchange::log::{DoorbellAction, ExchangeFollower, StreamKind};
    use crate::terminals::TerminalEventEnvelope;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::broadcast::error::RecvError;

    let events: u64 = 100_000;
    let data = tempfile::tempdir().expect("data dir");
    let workspace = tempfile::tempdir().expect("workspace");
    let service = PersistenceService::initialize(data.path()).expect("service");
    let project_id = project_id(&service, workspace.path());
    let repository = TerminalRepository::new(service.database_path());
    let run = running_cli_run(&repository, &project_id);
    record_launch_receipt(&service, &project_id, &run);
    let pulsation = service
        .create_pulsation(CreatePulsationRequest {
            project_id: project_id.clone(),
            feature_name: "Exchange load".into(),
        })
        .expect("pulsation")
        .pulsation;
    let mut connection = service.connection().expect("connection");
    let mut current = to_running(&mut connection, pulsation);

    let bus = Arc::new(AppEventBus::default());
    let stop = Arc::new(AtomicBool::new(false));
    let start_cursor = service.exchange_cursor(&project_id).expect("cursor");
    crate::exchange::log::spawn_notifier(
        service.database_path().to_path_buf(),
        bus.clone(),
        stop.clone(),
        Duration::from_millis(25),
    )
    .expect("notifier");
    let mut receiver = bus.subscribe();

    // The subscriber is slow on purpose: it sleeps every 50 events.
    let subscriber_service = service.clone();
    let subscriber_project = project_id.clone();
    let subscriber_run = run.id.clone();
    let subscriber = std::thread::spawn(move || {
        let repository = TerminalRepository::new(subscriber_service.database_path());
        let mut follower = ExchangeFollower::new(subscriber_project.clone(), start_cursor);
        let mut tally = SubscriberTally::default();
        let mut handled = 0u64;
        loop {
            match receiver.blocking_recv() {
                Ok(AppEvent::Terminal(envelope)) => {
                    if envelope.sequence > tally.last_terminal + 1 {
                        tally.gaps += 1;
                        let mut after = tally.last_terminal;
                        while after + 1 < envelope.sequence {
                            let page = repository
                                .list_events(&subscriber_run, after, 500)
                                .expect("re-read");
                            let Some(last) = page.last().map(|event| event.sequence) else {
                                break;
                            };
                            tally.reread_events += page
                                .iter()
                                .filter(|event| event.sequence < envelope.sequence)
                                .count() as u64;
                            after = last;
                        }
                    }
                    tally.last_terminal = tally.last_terminal.max(envelope.sequence);
                }
                Ok(AppEvent::Exchange(doorbell)) => match follower.on_doorbell(&doorbell) {
                    DoorbellAction::Apply => match doorbell.stream_kind {
                        StreamKind::Pulsation => tally.transitions += 1,
                        StreamKind::TerminalRun => tally.run_settled = true,
                        _ => {}
                    },
                    DoorbellAction::CatchUp(after) => {
                        if !follow_log(
                            &subscriber_service,
                            &subscriber_project,
                            &mut follower,
                            after,
                            &mut tally,
                        ) {
                            tally.resets += 1;
                        }
                    }
                    DoorbellAction::Ignore => {}
                },
                Ok(_) => {}
                Err(RecvError::Lagged(_)) => {
                    tally.lagged += 1;
                    if let Some(after) = follower.on_lagged()
                        && !follow_log(
                            &subscriber_service,
                            &subscriber_project,
                            &mut follower,
                            after,
                            &mut tally,
                        )
                    {
                        tally.resets += 1;
                    }
                }
                Err(RecvError::Closed) => break,
            }
            // The run settled: read whatever its last events were, as the
            // Agents panel's refresh would.
            if tally.run_settled && !tally.tail_read {
                tally.tail_read = true;
                loop {
                    let page = repository
                        .list_events(&subscriber_run, tally.last_terminal, 500)
                        .expect("tail");
                    let Some(last) = page.last().map(|event| event.sequence) else {
                        break;
                    };
                    tally.reread_events += page.len() as u64;
                    tally.last_terminal = last;
                }
            }
            handled += 1;
            if handled.is_multiple_of(50) {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        tally.cursor = follower.cursor();
        tally
    });

    // The producer: batches of 2,000 events published in one burst, twice
    // the bus capacity, then 20 pulsation transitions per batch.
    let mut produced_transitions = 0u64;
    let wall = Instant::now();
    let batch = 2_000u64;
    let mut start = 1u64;
    while start <= events {
        let end = (start + batch - 1).min(events);
        for event in bulk_events(&service, &run.id, start, end) {
            bus.publish(AppEvent::Terminal(Box::new(TerminalEventEnvelope {
                protocol_version: 2,
                project_id: project_id.clone(),
                agent_role: run.agent_role,
                run_id: run.id.clone(),
                sequence: event.sequence,
                run: run.clone(),
                event,
            })));
        }
        for _ in 0..((end + 1 - start) / 100) {
            if produced_transitions == 1_000 {
                break;
            }
            let next = if current.status == PulsationStatus::Running {
                PulsationStatus::Checkpoint
            } else {
                PulsationStatus::Running
            };
            current = timed_transition(&mut connection, &current, next).0;
            produced_transitions += 1;
        }
        start = end + 1;
    }
    repository
        .set_run_status(&run.id, TerminalRunStatus::Completed, Some(0), None, None)
        .expect("run completed");
    let produce_wall = wall.elapsed();
    // Let the notifier announce the last items, then close the bus.
    let final_cursor = service.exchange_cursor(&project_id).expect("cursor");
    std::thread::sleep(Duration::from_millis(500));
    stop.store(true, Ordering::Relaxed);
    std::thread::sleep(Duration::from_millis(100));
    drop(bus);
    let tally = subscriber.join().expect("subscriber");
    report(
        "m2_load_slow_subscriber",
        json!({
            "terminal_events": events,
            "pulsation_transitions": produced_transitions,
            "produce_wall_ms": micros(produce_wall) / 1_000,
            "lagged": tally.lagged,
            "terminal_gaps": tally.gaps,
            "reread_events": tally.reread_events,
            "resets": tally.resets,
            "transitions_seen": tally.transitions,
        }),
    );
    assert!(tally.lagged > 0 && tally.gaps > 0, "the rehearsal lagged");
    assert_eq!(tally.resets, 0, "no full reload");
    assert_eq!(
        tally.last_terminal, events,
        "the subscriber reached the last event"
    );
    assert_eq!(
        tally.cursor, final_cursor,
        "the follower reached the last item"
    );
    assert_eq!(
        tally.transitions, produced_transitions,
        "every transition seen once"
    );
}
