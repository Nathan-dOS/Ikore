//! ADR-040 M5: the activity pulse (specification 5). Closed grammars, the
//! phase machine, the pulse rate, the coordinator's pulses over the recorded
//! corpus, and the recorder behind a snapshot.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use ikore_contracts::AgentRole;
use serde_json::json;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::actions::ActionFrame;
use super::activity::{
    ActivityTracker, LiveActivityRecorder, PULSE_INTERVAL, PulseIdentity, program, pulse_target,
    url_host, workspace_path,
};
use super::corpus_capture::committed_corpus;
use super::{
    ActionStatus, ActivityPhase, AgentActivityPulseV1, AgentExecutionMode, CliBackend,
    CreateTerminalRunRequest, ExecutionOrigin, ProcessStreamItem, PulseActionDelta, PulseTarget,
    StreamEnd, TerminalEventEnvelope, TerminalEventSink, TerminalRepository, TerminalRunStatus,
    ToolKind, coordinate_process_events_blocking_inner,
};
use crate::test_support::{TestDatabase, host};

fn identity() -> PulseIdentity {
    PulseIdentity {
        project_id: "project".into(),
        run_id: "run".into(),
        agent_role: Some(AgentRole::BackendAgent),
        slot_id: None,
    }
}

/// The redactor of the test host: fixture secrets and `synthetic-…` canaries.
fn clean(value: &str) -> bool {
    host()
        .redact_secrets(value)
        .is_ok_and(|result| result.count == 0)
}

#[test]
fn program_target_unwraps_shells() {
    for (command, expected) in [
        ("cargo test -p ikore-ui", Some("cargo")),
        ("C:\\tools\\rg.exe -n needle src", Some("rg")),
        ("bash -lc \"npm run build\"", Some("npm")),
        ("sh -c 'git status --short'", Some("git")),
        ("pwsh -Command \"Get-ChildItem src\"", Some("Get-ChildItem")),
        ("powershell -c ls", Some("ls")),
        ("cmd /c dir", Some("dir")),
        ("\"C:\\Program Files\\Tool\\run.cmd\" --x", Some("run")),
        ("echo$(id) x", None),
        ("a-very-long-program-name-past-the-forty-characters", None),
        ("", None),
    ] {
        assert_eq!(program(command).as_deref(), expected, "{command}");
    }
}

#[test]
fn pulse_target_canaries_are_withheld() {
    let roots = [PathBuf::from("C:\\work\\project")];
    let read = |path: &str| {
        ActionFrame::call(
            CliBackend::Claude,
            None,
            "Read",
            &json!({"file_path": path}),
        )
    };
    let shell = |command: &str| {
        ActionFrame::call(
            CliBackend::Claude,
            None,
            "Bash",
            &json!({"command": command}),
        )
    };
    let web =
        |url: &str| ActionFrame::call(CliBackend::Claude, None, "WebFetch", &json!({"url": url}));
    let target = |frame: &ActionFrame| pulse_target(frame, &roots, clean);

    assert_eq!(
        target(&read("C:\\work\\project\\src\\main.rs")),
        Some(PulseTarget::WorkspacePath("src/main.rs".into()))
    );
    assert_eq!(
        target(&read("src/lib.rs")),
        Some(PulseTarget::WorkspacePath("src/lib.rs".into()))
    );
    for withheld in [
        // Outside the run's directories, or climbing out of them.
        "C:\\Users\\someone\\notes.txt",
        "../outside.rs",
        "src/../../outside.rs",
        // Credential paths (the ADR-030 catalog).
        ".ssh/id_rsa",
        ".env.local",
        "config/credentials.json",
        "keys/server.pem",
        // What the redactor finds.
        "src/ghp_abcdefghijklmnopqrstuvwxyz0123456789.txt",
        "notes/synthetic-path-canary.md",
        // Control characters.
        "src/main.rs\nother",
    ] {
        assert_eq!(
            target(&read(withheld)),
            Some(PulseTarget::Withheld),
            "{withheld}"
        );
    }
    assert_eq!(
        target(&shell(
            "curl -H 'Authorization: synthetic-bash-canary' https://example.invalid"
        )),
        Some(PulseTarget::Program("curl".into()))
    );
    assert_eq!(
        target(&shell("synthetic-program-canary --run")),
        Some(PulseTarget::Withheld)
    );
    assert_eq!(
        target(&web(
            "https://user:password@Example.COM:8443/path?key=synthetic-web-canary"
        )),
        Some(PulseTarget::Host("example.com".into()))
    );
    assert_eq!(
        target(&web("ftp://example.com/file")),
        Some(PulseTarget::Withheld)
    );
    assert_eq!(
        target(&web("https://synthetic-host-canary.example")),
        Some(PulseTarget::Withheld)
    );
    // Kinds without a live target carry none: a search pattern is free text.
    for (name, input) in [
        ("Grep", json!({"pattern": "synthetic-grep-canary"})),
        ("Task", json!({"description": "anything"})),
        ("TodoWrite", json!({"todos": []})),
    ] {
        let frame = ActionFrame::call(CliBackend::Claude, None, name, &input);
        assert_eq!(target(&frame), None, "{name}");
    }
    assert_eq!(workspace_path("C:\\work\\project", &roots), None);
    assert_eq!(url_host("https://[::1]:8080/x").as_deref(), Some("::1"));
}

/// Specification 5.3, row by row.
#[test]
fn phase_machine_table() {
    let start = Instant::now();
    let at = |ms: u64| start + Duration::from_millis(ms);
    let mut tracker = ActivityTracker::start(identity(), at(0));
    assert_eq!(tracker.phase(), ActivityPhase::Starting);
    tracker.reasoning(at(10));
    assert_eq!(tracker.phase(), ActivityPhase::Thinking);
    tracker.call(1, ToolKind::Read, "Read", None, at(20));
    assert_eq!(tracker.phase(), ActivityPhase::RunningTool);
    // Reasoning while a tool runs keeps the tool's phase.
    tracker.reasoning(at(25));
    assert_eq!(tracker.phase(), ActivityPhase::RunningTool);
    tracker.call(2, ToolKind::Shell, "Bash", None, at(30));
    // A result that does not close the last open action changes nothing.
    tracker.result(1, ActionStatus::Ok, 10, at(40));
    assert_eq!(tracker.phase(), ActivityPhase::RunningTool);
    tracker.result(2, ActionStatus::Ok, 10, at(50));
    assert_eq!(tracker.phase(), ActivityPhase::Thinking);
    tracker.writing(at(60));
    assert_eq!(tracker.phase(), ActivityPhase::Writing);
    tracker.waiting(at(70));
    assert_eq!(tracker.phase(), ActivityPhase::Waiting);
    tracker.completed(at(80));
    assert_eq!(tracker.phase(), ActivityPhase::Finishing);
    let pulse = tracker.finish(at(90));
    assert_eq!(pulse.phase, ActivityPhase::Finishing);
    assert_eq!(pulse.counters.tool_calls, 2);
    assert_eq!(pulse.counters.files_read, 1);
    assert_eq!(pulse.counters.commands, 1);
    // Thinking at the start, and again after the tools: two episodes.
    assert_eq!(pulse.counters.reasoning_blocks, 2);
}

#[test]
fn pulse_rate_is_bounded() {
    let start = Instant::now();
    let at = |ms: u64| start + Duration::from_millis(ms);
    let mut tracker = ActivityTracker::start(identity(), at(0));
    let first = tracker
        .poll(at(0))
        .expect("the first pulse goes out at once");
    assert_eq!(first.pulse_sequence, 1);
    // Three hundred calls inside one interval: no pulse before 250 ms.
    for ordinal in 1..=300_u32 {
        tracker.call(ordinal, ToolKind::Read, "Read", None, at(1));
    }
    assert!(tracker.poll(at(100)).is_none());
    let second = tracker.poll(at(250)).expect("due");
    assert_eq!(second.actions.len(), 32);
    // 300 deltas, 256 kept: the 44 oldest counted as dropped.
    assert_eq!(second.actions_dropped, 44);
    assert_eq!(second.actions[0].ordinal(), 45);
    // The rest leave 32 at a time, at most every 250 ms: four a second.
    let mut pulses = 2;
    let mut ms = 250;
    while tracker.pending() {
        ms += 250;
        let pulse = tracker.poll(at(ms)).expect("due");
        assert!(pulse.actions.len() <= 32);
        assert!(tracker.poll(at(ms + 100)).is_none());
        pulses += 1;
    }
    assert_eq!(pulses, 2 + (256 - 32) / 32);
}

/// Specification 5.3, `Waiting`: a rate limit Claude hit, or a Gemini backoff.
#[test]
fn rate_limits_and_backoff_wait() {
    use super::adapters::normalize_frame;
    let frame = |backend, stream, text: &str| normalize_frame(backend, stream, text.as_bytes());
    let rejected =
        json!({"type":"rate_limit_event","rate_limit_info":{"status":"rejected","resetsAt":1}});
    assert!(frame(CliBackend::Claude, "stdout", &rejected.to_string()).is_waiting);
    // The corpus's own events only warn: the turn goes on.
    let warning = json!({"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning"}});
    assert!(!frame(CliBackend::Claude, "stdout", &warning.to_string()).is_waiting);
    assert!(
        frame(
            CliBackend::Gemini,
            "stderr",
            "Attempt 1 failed with status 503. Retrying with backoff..."
        )
        .is_waiting
    );
    assert!(!frame(CliBackend::Codex, "stderr", "Retrying").is_waiting);
    let mut tracker = ActivityTracker::start(identity(), Instant::now());
    tracker.waiting(Instant::now());
    assert_eq!(tracker.phase(), ActivityPhase::Waiting);
}

#[derive(Default)]
struct Ordered {
    log: Mutex<Vec<Item>>,
}

#[derive(Debug, Clone)]
enum Item {
    Envelope,
    Pulse(Box<AgentActivityPulseV1>),
}

impl TerminalEventSink for Ordered {
    fn emit(&self, _envelope: &TerminalEventEnvelope) {
        self.log
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Item::Envelope);
    }

    fn emit_activity(&self, pulse: &AgentActivityPulseV1) {
        self.log
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Item::Pulse(Box::new(pulse.clone())));
    }
}

fn is_name(name: &str) -> bool {
    (1..=70).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b':' | b'-'))
}

fn assert_closed_grammar(pulse: &AgentActivityPulseV1, label: &str) {
    assert!(
        chrono::DateTime::parse_from_rfc3339(&pulse.emitted_at).is_ok(),
        "{label}"
    );
    for delta in &pulse.actions {
        let PulseActionDelta::Started { name, target, .. } = delta else {
            continue;
        };
        assert!(is_name(name), "{label}: {name}");
        match target {
            None | Some(PulseTarget::Withheld) => {}
            Some(PulseTarget::WorkspacePath(path)) => assert!(
                path.chars().count() <= 160
                    && !path.contains('\\')
                    && !path.starts_with('/')
                    && !path.split('/').any(|part| part == "..")
                    && !path.chars().any(char::is_control)
                    && clean(path),
                "{label}: {path}"
            ),
            Some(PulseTarget::Program(program)) => assert!(
                (1..=40).contains(&program.len())
                    && program.bytes().all(|byte| byte.is_ascii_alphanumeric()
                        || matches!(byte, b'.' | b'_' | b'+' | b'-'))
                    && clean(program),
                "{label}: {program}"
            ),
            Some(PulseTarget::Host(host_name)) => assert!(
                (1..=253).contains(&host_name.len())
                    && host_name.bytes().all(|byte| byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'.' | b'-' | b':'))
                    && clean(host_name),
                "{label}: {host_name}"
            ),
        }
    }
}

/// Specification 5.2 over every recorded frame: every string a pulse
/// carries is in a closed grammar; the first pulse comes before any event
/// and the last says `Finishing`; nothing of the pulse is persisted.
#[test]
fn pulse_strings_match_closed_grammar_and_bracket_the_run() {
    let mut runs = 0;
    for (backend, version, scenario, lines) in committed_corpus() {
        let model = match backend {
            CliBackend::Claude => "claude-haiku-5-5",
            CliBackend::Antigravity => "gemini-3.8-flash",
            CliBackend::Opencode => "groq/openai/gpt-oss-20b",
            _ => continue,
        };
        let label = format!("{} {version} {scenario}", backend.as_str());
        let data = tempfile::tempdir().unwrap();
        let database = TestDatabase::initialize(data.path()).unwrap();
        let project = database.insert_project(data.path(), None).unwrap().id;
        let repository = TerminalRepository::new(database.database_path(), host());
        repository.ensure_project_configs(&project, false).unwrap();
        let run = repository
            .create_run(CreateTerminalRunRequest {
                project_id: project,
                agent_role: AgentRole::BackendAgent,
                origin: ExecutionOrigin::Manual,
                execution_mode: AgentExecutionMode::CliProcess,
                backend: Some(backend),
                model: Some(model.into()),
                pulsation_id: None,
                job_id: None,
                agent_run_id: None,
                llm_execution_id: None,
            })
            .unwrap();
        for status in [TerminalRunStatus::Starting, TerminalRunStatus::Running] {
            repository
                .set_run_status(&run.id, status, None, None, None)
                .unwrap();
        }
        let run = repository.get_run(&run.id).unwrap();
        let (sender, receiver) = mpsc::channel(lines.len().max(1));
        for text in &lines {
            sender
                .try_send(ProcessStreamItem::Frame {
                    stream: "stdout",
                    bytes: text.as_bytes().to_vec(),
                    received_at: Instant::now(),
                })
                .unwrap();
        }
        drop(sender);
        let sink = Arc::new(Ordered::default());
        let _ = coordinate_process_events_blocking_inner(
            repository.clone(),
            sink.clone(),
            run.clone(),
            backend,
            receiver,
            CancellationToken::new(),
            Arc::new(Mutex::new(None)),
            None,
            None,
            Some(Arc::new(Mutex::new(Some(StreamEnd::Succeeded)))),
            vec![PathBuf::from("<workspace>")],
        );
        let log = sink.log.lock().unwrap().clone();
        let pulses = log
            .iter()
            .filter_map(|item| match item {
                Item::Pulse(pulse) => Some(pulse.as_ref()),
                Item::Envelope => None,
            })
            .collect::<Vec<_>>();
        assert!(matches!(log.first(), Some(Item::Pulse(_))), "{label}");
        let Some(Item::Pulse(last)) = log.last() else {
            panic!("{label}: the last pulse comes after every event");
        };
        assert_eq!(last.phase, ActivityPhase::Finishing, "{label}");
        for (index, pulse) in pulses.iter().enumerate() {
            assert_eq!(pulse.pulse_sequence, index as u64 + 1, "{label}");
            assert_eq!(pulse.run_id, run.id, "{label}");
            assert_closed_grammar(pulse, &label);
        }
        // Ephemeral: no event and no detail holds a pulse.
        for event in repository.list_events(&run.id, 0, 500).unwrap() {
            assert!(
                !event.content.contains("pulseSequence"),
                "{label}: {event:?}"
            );
        }
        runs += 1;
    }
    assert!(runs >= 12, "the corpus replayed {runs} runs");
}

/// Specification 5.5: the recorder keeps a live run's latest pulse and its
/// recent actions, and forgets the run once an envelope says it ended.
#[test]
fn live_activity_snapshot_lifecycle() {
    let inner = Arc::new(Ordered::default());
    let recorder = LiveActivityRecorder::new(inner.clone());
    let start = Instant::now();
    let mut tracker = ActivityTracker::start(identity(), start);
    tracker.poll(start).unwrap();
    for ordinal in 1..=70_u32 {
        tracker.call(ordinal, ToolKind::Read, "Read", None, start);
        tracker.result(ordinal, ActionStatus::Ok, 1, start);
    }
    let mut at = start;
    while tracker.pending() {
        at += PULSE_INTERVAL;
        recorder.emit_activity(&tracker.poll(at).unwrap());
    }
    let snapshots = recorder.snapshots("project");
    assert_eq!(snapshots.len(), 1);
    assert!(snapshots[0].latest.actions.is_empty());
    assert_eq!(snapshots[0].recent_actions.len(), 64);
    assert!(recorder.snapshots("another").is_empty());
    // Every pulse went on to the inner sink.
    assert!(
        inner
            .log
            .lock()
            .unwrap()
            .iter()
            .all(|item| matches!(item, Item::Pulse(_)))
    );
    let data = tempfile::tempdir().unwrap();
    let database = TestDatabase::initialize(data.path()).unwrap();
    let project = database.insert_project(data.path(), None).unwrap().id;
    let repository = TerminalRepository::new(database.database_path(), host());
    repository.ensure_project_configs(&project, false).unwrap();
    let mut run = repository
        .create_run(CreateTerminalRunRequest {
            project_id: project.clone(),
            agent_role: AgentRole::BackendAgent,
            origin: ExecutionOrigin::Manual,
            execution_mode: AgentExecutionMode::CliProcess,
            backend: Some(CliBackend::Claude),
            model: None,
            pulsation_id: None,
            job_id: None,
            agent_run_id: None,
            llm_execution_id: None,
        })
        .unwrap();
    run.id = "run".into();
    run.status = TerminalRunStatus::Completed;
    let event = super::TerminalEvent {
        id: "event".into(),
        run_id: "run".into(),
        sequence: 1,
        kind: super::TerminalEventKind::Completed,
        stream: None,
        content: "Run completed.".into(),
        redaction_count: 0,
        redaction_categories: Vec::new(),
        created_at: "2026-11-16T10:00:00Z".into(),
        detail: None,
        observed_at: None,
    };
    recorder.emit(&TerminalEventEnvelope {
        protocol_version: 2,
        project_id: project,
        agent_role: Some(AgentRole::BackendAgent),
        run_id: "run".into(),
        sequence: 1,
        run,
        event,
    });
    assert!(recorder.snapshots("project").is_empty());
}
