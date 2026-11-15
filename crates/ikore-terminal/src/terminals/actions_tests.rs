//! ADR-040 M1 through the coordinator: the host's action summaries, a failed
//! run that publishes only them, and the recorded corpus end to end.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use ikore_contracts::AgentRole;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::corpus_capture::committed_corpus;
use super::{
    ActionStatus, AgentExecutionMode, CliBackend, CreateTerminalRunRequest, EventDetailV1,
    ExecutionOrigin, ProcessEventSummary, ProcessStreamItem, StreamEnd, TerminalError,
    TerminalEvent, TerminalEventEnvelope, TerminalEventKind, TerminalEventSink, TerminalRepository,
    TerminalRun, TerminalRunStatus, ToolKind, WithheldReason,
    coordinate_process_events_blocking_inner,
};
use crate::test_support::{TestDatabase, host};

const CLAUDE_MODEL: &str = "claude-haiku-5-5";

#[derive(Default)]
struct Collect {
    envelopes: Mutex<Vec<TerminalEventEnvelope>>,
}

impl TerminalEventSink for Collect {
    fn emit(&self, envelope: &TerminalEventEnvelope) {
        self.envelopes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(envelope.clone());
    }
}

struct Harness {
    _data: tempfile::TempDir,
    repository: TerminalRepository,
    run: TerminalRun,
}

fn start_run(backend: CliBackend, model: Option<&str>) -> Harness {
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
            model: model.map(str::to_owned),
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
    Harness {
        _data: data,
        repository,
        run,
    }
}

struct Outcome {
    result: Result<ProcessEventSummary, TerminalError>,
    events: Vec<TerminalEvent>,
    envelopes: Vec<TerminalEventEnvelope>,
}

fn frame(value: &Value) -> ProcessStreamItem {
    line(&value.to_string())
}

fn line(text: &str) -> ProcessStreamItem {
    ProcessStreamItem::Frame {
        stream: "stdout",
        bytes: text.as_bytes().to_vec(),
        received_at: Instant::now(),
    }
}

fn coordinate(
    harness: &Harness,
    backend: CliBackend,
    items: Vec<ProcessStreamItem>,
    end: Option<StreamEnd>,
    ceiling: Option<u64>,
) -> Outcome {
    let (sender, receiver) = mpsc::channel(items.len().max(1));
    for item in items {
        sender.try_send(item).unwrap();
    }
    drop(sender);
    let sink = Arc::new(Collect::default());
    let result = coordinate_process_events_blocking_inner(
        harness.repository.clone(),
        sink.clone(),
        harness.run.clone(),
        backend,
        receiver,
        CancellationToken::new(),
        Arc::new(Mutex::new(None)),
        None,
        ceiling,
        end.map(|end| Arc::new(Mutex::new(Some(end)))),
        vec![PathBuf::from("<workspace>")],
    );
    let events = harness
        .repository
        .list_events(&harness.run.id, 0, 500)
        .unwrap();
    let envelopes = sink.envelopes.lock().unwrap().clone();
    Outcome {
        result,
        events,
        envelopes,
    }
}

fn assert_contiguous(harness: &Harness, events: &[TerminalEvent]) {
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event.sequence, index as u64 + 1, "{events:#?}");
    }
    assert_eq!(
        harness
            .repository
            .next_event_sequence(&harness.run.id)
            .unwrap(),
        events.len() as u64 + 1
    );
}

fn contents(events: &[TerminalEvent]) -> Vec<(TerminalEventKind, String)> {
    events
        .iter()
        .map(|event| (event.kind, event.content.clone()))
        .collect()
}

/// A Claude turn that reads a file holding a synthetic secret, answers with
/// some text, then starts a command the stream never answers.
fn claude_turn() -> Vec<ProcessStreamItem> {
    vec![
        frame(&json!({"type":"system","subtype":"init","session_id":"s-1","model":CLAUDE_MODEL})),
        frame(&json!({"type":"stream_event","session_id":"s-1",
            "event":{"type":"message_start","message":{"id":"m-1","model":CLAUDE_MODEL}}})),
        frame(
            &json!({"type":"assistant","session_id":"s-1","message":{"id":"m-1","model":CLAUDE_MODEL,
            "content":[{"type":"text","text":"Let me read the configuration."},
                {"type":"tool_use","id":"toolu_1","name":"Read",
                 "input":{"file_path":"<workspace>\\config.env"}}]}}),
        ),
        frame(
            &json!({"type":"user","session_id":"s-1","message":{"role":"user",
            "content":[{"type":"tool_result","tool_use_id":"toolu_1",
                "content":"SERVICE_TOKEN=synthetic-result-body\nSECOND=line"}]}}),
        ),
        frame(
            &json!({"type":"assistant","session_id":"s-1","message":{"id":"m-2","model":CLAUDE_MODEL,
            "content":[{"type":"tool_use","id":"toolu_2","name":"Bash",
                "input":{"command":"cargo test --all"}}]}}),
        ),
        frame(&json!({"type":"stream_event","session_id":"s-1",
            "event":{"type":"content_block_delta","index":0,
                "delta":{"type":"text_delta","text":"A partial answer"}}})),
    ]
}

const CLI_TEXT: [&str; 4] = [
    "synthetic-result-body",
    "SERVICE_TOKEN",
    "Let me read the configuration.",
    "A partial answer",
];

/// What every failed `claude_turn` publishes: its actions, never a result
/// body, then why the rest was withheld.
fn assert_actions_only(harness: &Harness, outcome: &Outcome, reason: WithheldReason) {
    let published = contents(&outcome.events);
    assert_eq!(
        published[..5],
        [
            (TerminalEventKind::ToolUse, "Read · config.env".to_owned()),
            (TerminalEventKind::ToolResult, "Read · ok".to_owned()),
            (
                TerminalEventKind::ToolUse,
                "Bash · cargo test --all".to_owned()
            ),
            (TerminalEventKind::ToolResult, "Bash · unknown".to_owned()),
            (TerminalEventKind::System, reason.text().to_owned()),
        ],
        "{reason:?}"
    );
    assert_eq!(
        outcome.events[4].detail,
        Some(EventDetailV1::Withheld { v: 1, reason })
    );
    assert!(matches!(
        outcome.events[1].detail,
        Some(EventDetailV1::ActionResult {
            ordinal: 1,
            status: ActionStatus::Ok,
            lines: Some(2),
            ..
        })
    ));
    assert!(
        outcome.events[..5]
            .iter()
            .all(|event| event.stream.is_none() && event.observed_at.is_some())
    );
    for text in CLI_TEXT {
        assert!(
            outcome
                .events
                .iter()
                .all(|event| !event.content.contains(text)),
            "{reason:?} published {text}"
        );
        assert!(
            outcome
                .envelopes
                .iter()
                .all(|envelope| !envelope.event.content.contains(text)),
            "{reason:?} emitted {text}"
        );
    }
    assert_contiguous(harness, &outcome.events);
}

#[test]
fn finish_actions_only_for_each_withheld_reason() {
    let ends = [
        (Some(StreamEnd::Failed), WithheldReason::IncompleteExecution),
        (Some(StreamEnd::Cancelled), WithheldReason::Cancelled),
        (Some(StreamEnd::TimedOut), WithheldReason::TimedOut),
    ];
    for (end, reason) in ends {
        let harness = start_run(CliBackend::Claude, Some(CLAUDE_MODEL));
        let outcome = coordinate(&harness, CliBackend::Claude, claude_turn(), end, None);
        // The caller classifies an incomplete execution from the summary.
        let summary = outcome.result.as_ref().unwrap();
        assert!(summary.raw_output.is_empty());
        assert_actions_only(&harness, &outcome, reason);
        assert_eq!(outcome.events.len(), 5);
    }

    // A read error leaves the stream incomplete; for Claude it also ends the
    // stream as an error frame, which the host classifies after the line.
    let harness = start_run(CliBackend::Claude, Some(CLAUDE_MODEL));
    let mut items = claude_turn();
    items.push(ProcessStreamItem::ReadError {
        stream: "stdout",
        message: "the pipe broke".into(),
        received_at: Instant::now(),
    });
    let outcome = coordinate(
        &harness,
        CliBackend::Claude,
        items,
        Some(StreamEnd::Succeeded),
        None,
    );
    assert_eq!(
        outcome.result.as_ref().unwrap_err().code(),
        "cli_process_failed"
    );
    assert_actions_only(&harness, &outcome, WithheldReason::IncompleteStream);

    // The provider cut the output at its token budget.
    let harness = start_run(CliBackend::Claude, Some(CLAUDE_MODEL));
    let mut items = claude_turn();
    items.push(frame(
        &json!({"type":"result","subtype":"success","session_id":"s-1",
        "stop_reason":"max_tokens","result":"A partial answer"}),
    ));
    let outcome = coordinate(
        &harness,
        CliBackend::Claude,
        items,
        Some(StreamEnd::Succeeded),
        None,
    );
    assert!(matches!(
        outcome.result,
        Err(TerminalError::OutputTruncated { .. })
    ));
    assert_actions_only(&harness, &outcome, WithheldReason::OutputTruncated);

    // A generation from another model stops the stream.
    let harness = start_run(CliBackend::Claude, Some(CLAUDE_MODEL));
    let mut items = claude_turn();
    items.push(frame(
        &json!({"type":"assistant","session_id":"s-1","message":{
        "id":"m-3","model":"claude-sonnet-5","content":[{"type":"text","text":"Other model"}]}}),
    ));
    let outcome = coordinate(
        &harness,
        CliBackend::Claude,
        items,
        Some(StreamEnd::Succeeded),
        None,
    );
    assert_eq!(
        outcome.result.as_ref().unwrap_err().code(),
        "model_mismatch"
    );
    assert_actions_only(&harness, &outcome, WithheldReason::UnverifiedModel);

    // The output budget: the host stops the run at the frame that passes it.
    let harness = start_run(CliBackend::Claude, Some(CLAUDE_MODEL));
    let mut items = claude_turn();
    items.push(frame(&json!({"type":"stream_event","session_id":"s-1",
        "event":{"type":"message_delta","usage":{"output_tokens":5000}}})));
    let outcome = coordinate(
        &harness,
        CliBackend::Claude,
        items,
        Some(StreamEnd::Succeeded),
        Some(1_000),
    );
    assert!(matches!(
        outcome.result,
        Err(TerminalError::OutputBudgetExceeded { .. })
    ));
    assert_actions_only(&harness, &outcome, WithheldReason::OutputBudget);

    // A provider failure: the actions, the withheld line, then the host's
    // own classification of the error.
    let harness = start_run(CliBackend::Claude, Some(CLAUDE_MODEL));
    let mut items = claude_turn();
    items.push(frame(
        &json!({"type":"result","session_id":"s-1","is_error":true,
        "result":"upstream failure synthetic-error-detail"}),
    ));
    let outcome = coordinate(
        &harness,
        CliBackend::Claude,
        items,
        Some(StreamEnd::Succeeded),
        None,
    );
    assert!(outcome.result.is_err());
    assert_actions_only(&harness, &outcome, WithheldReason::IncompleteExecution);
    assert_eq!(outcome.events.len(), 6);
    assert_eq!(outcome.events[5].kind, TerminalEventKind::Error);
    assert!(
        outcome
            .events
            .iter()
            .all(|event| !event.content.contains("synthetic-error-detail"))
    );
}

#[test]
fn actions_only_keeps_sequence_contiguous_after_live_events() {
    let harness = start_run(CliBackend::Claude, Some(CLAUDE_MODEL));
    // The host's live lifecycle lines come first, as in a real run.
    for (sequence, text) in [(1, "Run starting."), (2, "Run in progress.")] {
        harness
            .repository
            .append_event(
                &harness.run.id,
                sequence,
                TerminalEventKind::System,
                None,
                text,
            )
            .unwrap();
    }
    let outcome = coordinate(
        &harness,
        CliBackend::Claude,
        claude_turn(),
        Some(StreamEnd::Cancelled),
        None,
    );
    assert!(outcome.result.is_ok());
    let sequences = outcome
        .events
        .iter()
        .map(|event| event.sequence)
        .collect::<Vec<_>>();
    assert_eq!(sequences, (1..=7).collect::<Vec<_>>());
    // The run's next live event follows without a gap the UI would chase.
    let next = harness
        .repository
        .next_event_sequence(&harness.run.id)
        .unwrap();
    assert_eq!(next, 8);
    harness
        .repository
        .append_event(
            &harness.run.id,
            next,
            TerminalEventKind::System,
            None,
            "CLI timing: fixture",
        )
        .unwrap();
    let envelope_sequences = outcome
        .envelopes
        .iter()
        .map(|envelope| envelope.sequence)
        .collect::<Vec<_>>();
    assert_eq!(envelope_sequences, (3..=7).collect::<Vec<_>>());
    // Every event of a live emission carries its instant too.
    assert!(
        harness
            .repository
            .list_events(&harness.run.id, 0, 50)
            .unwrap()
            .iter()
            .all(|event| event.observed_at.is_some())
    );
}

#[test]
fn a_completed_run_publishes_actions_with_the_cli_result_text() {
    let harness = start_run(CliBackend::Claude, Some(CLAUDE_MODEL));
    let mut items = claude_turn();
    items.push(frame(
        &json!({"type":"user","session_id":"s-1","message":{"role":"user",
        "content":[{"type":"tool_result","tool_use_id":"toolu_2","is_error":true,
            "content":"error: 2 tests failed"}]}}),
    ));
    items.push(frame(
        &json!({"type":"result","subtype":"success","session_id":"s-1",
        "result":"Done."}),
    ));
    let outcome = coordinate(
        &harness,
        CliBackend::Claude,
        items,
        Some(StreamEnd::Succeeded),
        None,
    );
    let summary = outcome.result.unwrap();
    assert_eq!(summary.raw_output, "Done.");
    let actions = outcome
        .events
        .iter()
        .filter(|event| {
            matches!(
                event.kind,
                TerminalEventKind::ToolUse | TerminalEventKind::ToolResult
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(actions.len(), 4, "{:#?}", outcome.events);
    // The call is the host's summary, apart from the CLI's text.
    assert_eq!(actions[0].content, "Read · config.env");
    assert_eq!(actions[0].stream, None);
    assert_eq!(
        actions[0].detail,
        Some(EventDetailV1::ActionCall {
            v: 1,
            ordinal: 1,
            kind: ToolKind::Read,
            name: "Read".into(),
        })
    );
    // A result with text keeps it, redacted, on stdout as before M1.
    assert_eq!(actions[1].stream.as_deref(), Some("stdout"));
    assert!(actions[1].content.contains("<REDACTED_SECRET:FIXTURE_"));
    assert!(!actions[1].content.contains("synthetic-result-body"));
    assert!(actions[1].content.contains("SECOND=line"));
    assert_eq!(actions[2].content, "Bash · cargo test --all");
    assert!(actions[3].content.contains("2 tests failed"));
    assert!(matches!(
        actions[3].detail,
        Some(EventDetailV1::ActionResult {
            ordinal: 2,
            status: ActionStatus::Error,
            duration_ms: Some(_),
            ..
        })
    ));
    // The message text before the call stays the model's message, first.
    let message = outcome
        .events
        .iter()
        .position(|event| event.content == "Let me read the configuration.")
        .unwrap();
    let call = outcome
        .events
        .iter()
        .position(|event| event.content == "Read · config.env")
        .unwrap();
    assert!(message < call);
    assert!(outcome.events.iter().all(|event| {
        event.content != "CLI tool_use received." && event.content != "CLI tool_result received."
    }));
    assert_contiguous(&harness, &outcome.events);
    // `observed_at` follows the order the frames reached the host.
    let observed = outcome
        .events
        .iter()
        .map(|event| event.observed_at.clone().unwrap())
        .collect::<Vec<_>>();
    assert!(
        observed.windows(2).all(|pair| pair[0] <= pair[1]),
        "{observed:#?}"
    );
    assert!(observed.iter().all(|instant| {
        chrono::DateTime::parse_from_rfc3339(instant).is_ok() && instant.ends_with('Z')
    }));
}

/// A persistence failure aborts before anything is published: the database
/// that failed is not asked to hold the summaries either.
#[test]
fn persistence_failure_publishes_nothing() {
    let harness = start_run(CliBackend::Claude, Some(CLAUDE_MODEL));
    harness
        .repository
        .connection()
        .unwrap()
        .execute("DROP TABLE token_usage_records", [])
        .unwrap();
    let outcome = coordinate(
        &harness,
        CliBackend::Claude,
        claude_turn(),
        Some(StreamEnd::Failed),
        None,
    );
    assert!(outcome.result.is_err());
    assert!(outcome.events.is_empty(), "{:#?}", outcome.events);
    assert!(outcome.envelopes.is_empty());
}

#[test]
fn host_events_skip_metadata_substitution_and_are_redacted() {
    let harness = start_run(CliBackend::Claude, Some(CLAUDE_MODEL));
    let mut items = vec![frame(&json!({"type":"stream_event","session_id":"s-1",
        "event":{"type":"message_start","message":{"id":"m-1","model":CLAUDE_MODEL}}}))];
    for (id, name, input) in [
        (
            "toolu_b",
            "Bash",
            json!({"command":"curl -H 'Authorization: synthetic-bash-canary' https://example.invalid"}),
        ),
        (
            "toolu_g",
            "Grep",
            json!({"pattern":"synthetic-grep-canary","path":"<workspace>\\src"}),
        ),
        (
            "toolu_w",
            "WebFetch",
            json!({"url":"https://example.invalid/?key=synthetic-web-canary"}),
        ),
        (
            "toolu_t",
            "Task",
            json!({"description":"Check synthetic-task-canary","prompt":"long private prompt"}),
        ),
    ] {
        items.push(frame(
            &json!({"type":"assistant","session_id":"s-1","message":{
            "id":"m-1","model":CLAUDE_MODEL,
            "content":[{"type":"tool_use","id":id,"name":name,"input":input}]}}),
        ));
        items.push(frame(
            &json!({"type":"user","session_id":"s-1","message":{"role":"user",
            "content":[{"type":"tool_result","tool_use_id":id,"content":""}]}}),
        ));
    }
    items.push(frame(
        &json!({"type":"result","subtype":"success","session_id":"s-1","result":"ok"}),
    ));
    let outcome = coordinate(
        &harness,
        CliBackend::Claude,
        items,
        Some(StreamEnd::Succeeded),
        None,
    );
    outcome.result.unwrap();
    let calls = outcome
        .events
        .iter()
        .filter(|event| event.kind == TerminalEventKind::ToolUse)
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 4);
    for (call, prefix) in calls
        .iter()
        .zip(["Bash · ", "Grep · ", "WebFetch · ", "Task · "])
    {
        assert!(call.content.starts_with(prefix), "{}", call.content);
        assert!(
            call.content.contains("<REDACTED_SECRET:FIXTURE_"),
            "{}",
            call.content
        );
        assert!(!call.content.contains("synthetic-"), "{}", call.content);
        assert!(call.redaction_count > 0);
    }
    assert!(
        calls[1].content.ends_with(" in src"),
        "{}",
        calls[1].content
    );
    // A result without text is the host's summary, never the placeholder.
    let results = outcome
        .events
        .iter()
        .filter(|event| event.kind == TerminalEventKind::ToolResult)
        .map(|event| event.content.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        results,
        ["Bash · ok", "Grep · ok", "WebFetch · ok", "Task · ok"]
    );
    assert!(outcome.envelopes.iter().all(|envelope| {
        !envelope.event.content.contains("synthetic-")
            && envelope.event.content != "CLI tool_use received."
            && envelope.event.content != "CLI tool_result received."
    }));
    // `detail_json` carries no free text: no target, no prompt, no URL.
    let stored: Vec<String> = harness
        .repository
        .connection()
        .unwrap()
        .prepare("SELECT detail_json FROM terminal_events WHERE detail_json IS NOT NULL")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(stored.len(), 8);
    for detail in &stored {
        for free_text in [
            "synthetic",
            "curl",
            "example.invalid",
            "src",
            "Check",
            "prompt",
        ] {
            assert!(!detail.contains(free_text), "{detail}");
        }
    }
}

#[test]
fn reasoning_never_reaches_the_audit() {
    let harness = start_run(CliBackend::Claude, Some(CLAUDE_MODEL));
    let items = vec![
        frame(&json!({"type":"stream_event","session_id":"s-1",
            "event":{"type":"message_start","message":{"id":"m-1","model":CLAUDE_MODEL}}})),
        frame(&json!({"type":"stream_event","session_id":"s-1",
            "event":{"type":"content_block_start","index":0,
                "content_block":{"type":"thinking","thinking":"","signature":""}}})),
        frame(&json!({"type":"stream_event","session_id":"s-1",
            "event":{"type":"content_block_delta","index":0,
                "delta":{"type":"thinking_delta","thinking":"a private chain of thought"}}})),
        frame(&json!({"type":"stream_event","session_id":"s-1",
            "event":{"type":"content_block_delta","index":0,
                "delta":{"type":"signature_delta","signature":"opaque-signature"}}})),
        frame(
            &json!({"type":"assistant","session_id":"s-1","message":{"id":"m-1","model":CLAUDE_MODEL,
            "content":[{"type":"thinking","thinking":"a private chain of thought","signature":"opaque-signature"},
                {"type":"redacted_thinking","data":"opaque-redacted-data"},
                {"type":"text","text":"The answer."}]}}),
        ),
        frame(
            &json!({"type":"result","subtype":"success","session_id":"s-1","result":"The answer."}),
        ),
    ];
    let outcome = coordinate(
        &harness,
        CliBackend::Claude,
        items,
        Some(StreamEnd::Succeeded),
        None,
    );
    assert_eq!(outcome.result.unwrap().raw_output, "The answer.");
    assert!(outcome.events.iter().all(|event| {
        !event.content.contains("private chain") && !event.content.contains("opaque-")
    }));
    assert!(
        outcome
            .events
            .iter()
            .any(|event| event.content == "The answer.")
    );
}

/// A long run's snapshot reads its last 500 events, and a secret that began
/// before the page is still redacted in the page's first event.
#[test]
fn snapshot_loads_tail_with_redaction_context() {
    let harness = start_run(CliBackend::Claude, Some(CLAUDE_MODEL));
    let mut connection = harness.repository.connection().unwrap();
    // Under a known sanitization policy a page is read by range, with context.
    connection
        .execute(
            "INSERT INTO secret_sanitization_receipts(call_id,project_id,policy_identity,
               input_redaction_count,categories_json,examined_bytes,duration_ms,created_at)
             VALUES(?1,?2,'fixture',0,'[]',0,0,'2026-11-15T00:00:00Z')",
            rusqlite::params![harness.run.id, harness.run.project_id],
        )
        .unwrap();
    let transaction = connection.transaction().unwrap();
    {
        let mut insert = transaction
            .prepare(
                "INSERT INTO terminal_events(id,run_id,sequence,kind,stream,content,created_at)
                 VALUES(?1,?2,?3,'message','stdout',?4,'2026-11-15T00:00:00Z')",
            )
            .unwrap();
        for sequence in 1..=700_u64 {
            let content = match sequence {
                200 => "the key is synth".to_owned(),
                201 => "etic-tail-secret and more".to_owned(),
                _ => format!("line {sequence}"),
            };
            insert
                .execute(rusqlite::params![
                    format!("event-{sequence}"),
                    harness.run.id,
                    sequence,
                    content
                ])
                .unwrap();
        }
    }
    transaction.commit().unwrap();
    let tail = harness
        .repository
        .list_events_tail(&harness.run.id, 500)
        .unwrap();
    assert_eq!(tail.len(), 500);
    assert_eq!(tail[0].sequence, 201);
    assert_eq!(tail[499].sequence, 700);
    assert!(
        !tail[0].content.contains("etic-tail-secret"),
        "{}",
        tail[0].content
    );
    assert!(tail[0].content.contains("and more"));
    // A short run comes whole.
    let short = start_run(CliBackend::Claude, Some(CLAUDE_MODEL));
    for sequence in 1..=3 {
        short
            .repository
            .append_event(
                &short.run.id,
                sequence,
                TerminalEventKind::System,
                None,
                "Run in progress.",
            )
            .unwrap();
    }
    assert_eq!(
        short
            .repository
            .list_events_tail(&short.run.id, 500)
            .unwrap()
            .len(),
        3
    );
}

/// The recorded corpus through the real coordinator (ADR-040, M1 acceptance):
/// no tool frame becomes `CLI tool_use received.`, every action carries its
/// detail, the synthetic canary never leaves memory unredacted, and the
/// sequence stays contiguous. `OpenCode` frames carry no generation identity,
/// so its runs exercise the actions-only path on real frames.
#[test]
fn corpus_runs_publish_actions_instead_of_protocol_lines() {
    let mut checked = 0;
    for (backend, version, scenario, lines) in committed_corpus() {
        let model = match backend {
            CliBackend::Claude => CLAUDE_MODEL,
            CliBackend::Antigravity => "gemini-3.8-flash",
            CliBackend::Opencode => "groq/openai/gpt-oss-20b",
            // Codex needs its session rollout for identity, and Gemini's
            // account was refused: neither recorded a tool frame.
            _ => continue,
        };
        let label = format!("{} {version} {scenario}", backend.as_str());
        let harness = start_run(backend, Some(model));
        let items = lines.iter().map(|text| line(text)).collect::<Vec<_>>();
        let outcome = coordinate(&harness, backend, items, Some(StreamEnd::Succeeded), None);
        for event in &outcome.events {
            assert!(
                !event.content.starts_with("CLI tool_use received")
                    && !event.content.starts_with("CLI tool_result received"),
                "{label}: {event:?}"
            );
            assert!(
                !event.content.contains("synthetic-corpus-canary"),
                "{label}: {event:?}"
            );
            assert!(event.observed_at.is_some(), "{label}: {event:?}");
            match event.kind {
                TerminalEventKind::ToolUse => assert!(
                    matches!(event.detail, Some(EventDetailV1::ActionCall { .. }))
                        && event.stream.is_none(),
                    "{label}: {event:?}"
                ),
                TerminalEventKind::ToolResult => assert!(
                    matches!(event.detail, Some(EventDetailV1::ActionResult { .. })),
                    "{label}: {event:?}"
                ),
                _ => {}
            }
        }
        assert!(
            outcome
                .envelopes
                .iter()
                .all(|envelope| !envelope.event.content.contains("synthetic-corpus-canary")),
            "{label}"
        );
        assert_contiguous(&harness, &outcome.events);
        if backend == CliBackend::Opencode && outcome.result.is_err() {
            // The withheld path on real frames never publishes a result body.
            assert!(
                outcome
                    .events
                    .iter()
                    .filter(|event| event.kind == TerminalEventKind::ToolResult)
                    .all(|event| event.stream.is_none()
                        && (event.content.starts_with("read · ")
                            || event.content.starts_with("glob · "))),
                "{label}: {:#?}",
                outcome.events
            );
            assert!(
                outcome
                    .events
                    .last()
                    .is_some_and(|event| event.content.starts_with("Output withheld: ")),
                "{label}"
            );
        }
        checked += 1;
    }
    assert!(checked >= 18, "only {checked} corpus files ran");
}
