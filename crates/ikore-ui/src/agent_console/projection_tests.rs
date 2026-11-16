use std::rc::Rc;
use std::time::Instant;

use ikore_core::terminals::{
    ActionStatus, EventDetailV1, TerminalEvent, TerminalEventKind, ToolKind, WithheldReason,
};

use super::projection::{
    ActionState, BlockBody, ConsoleFilter, NoticeLevel, OutputStream, Projector,
};

fn event(
    sequence: u64,
    kind: TerminalEventKind,
    stream: Option<&str>,
    content: &str,
) -> TerminalEvent {
    TerminalEvent {
        id: format!("event-{sequence}"),
        run_id: "run-1".into(),
        sequence,
        kind,
        stream: stream.map(str::to_owned),
        content: content.into(),
        redaction_count: 0,
        redaction_categories: Vec::new(),
        created_at: "2026-11-15T00:00:00Z".into(),
        detail: None,
        observed_at: None,
    }
}

fn call(sequence: u64, ordinal: u32, name: &str, target: &str) -> TerminalEvent {
    TerminalEvent {
        detail: Some(EventDetailV1::ActionCall {
            v: 1,
            ordinal,
            kind: ToolKind::Read,
            name: name.into(),
        }),
        ..event(
            sequence,
            TerminalEventKind::ToolUse,
            None,
            &format!("{name} · {target}"),
        )
    }
}

fn result(
    sequence: u64,
    ordinal: u32,
    status: ActionStatus,
    cli_text: Option<&str>,
) -> TerminalEvent {
    let content = cli_text.map_or_else(|| format!("Read · {}", status.as_str()), str::to_owned);
    TerminalEvent {
        detail: Some(EventDetailV1::ActionResult {
            v: 1,
            ordinal,
            status,
            exit_code: None,
            lines: None,
            bytes: None,
            duration_ms: Some(230),
        }),
        ..event(
            sequence,
            TerminalEventKind::ToolResult,
            cli_text.map(|_| "stdout"),
            &content,
        )
    }
}

fn project(events: &[TerminalEvent]) -> Projector {
    let mut projector = Projector::default();
    projector.rebuild(events);
    projector
}

#[test]
fn calls_and_results_group_into_one_action_by_ordinal() {
    let events = [
        event(1, TerminalEventKind::System, None, "Run starting."),
        call(2, 1, "Read", "src/a.rs"),
        call(3, 2, "Read", "src/b.rs"),
        result(4, 2, ActionStatus::Ok, Some("fn b() {}")),
        result(5, 1, ActionStatus::Error, None),
    ];
    let projector = project(&events);
    let blocks = projector.blocks();
    assert_eq!(blocks.len(), 3);
    assert_eq!(blocks[0].body, BlockBody::System("Run starting.".into()));
    let BlockBody::Action(first) = &blocks[1].body else {
        panic!("{:?}", blocks[1]);
    };
    assert_eq!(first.target.as_deref(), Some("src/a.rs"));
    assert!(matches!(
        first.state,
        ActionState::Done {
            status: ActionStatus::Error,
            ..
        }
    ));
    // A host summary is not result text.
    assert_eq!(first.result, None);
    let BlockBody::Action(second) = &blocks[2].body else {
        panic!("{:?}", blocks[2]);
    };
    assert_eq!(second.result.as_deref(), Some("fn b() {}"));
    assert_eq!(blocks[1].key, "event-2");
}

#[test]
fn legacy_events_pair_by_adjacency_and_count_protocol_lines() {
    let events = [
        event(
            1,
            TerminalEventKind::Message,
            Some("stdout"),
            "CLI message received.",
        ),
        event(
            2,
            TerminalEventKind::ToolUse,
            Some("stdout"),
            "CLI tool_use received.",
        ),
        event(
            3,
            TerminalEventKind::ToolUse,
            Some("stdout"),
            "read src/a.rs",
        ),
        event(
            4,
            TerminalEventKind::ToolResult,
            Some("stdout"),
            "fn a() {}",
        ),
        event(
            5,
            TerminalEventKind::Completed,
            Some("stdout"),
            "CLI completed received.",
        ),
    ];
    let blocks = project(&events).blocks().to_vec();
    assert_eq!(blocks.len(), 3, "{blocks:#?}");
    assert_eq!(blocks[0].body, BlockBody::ProtocolCount(2));
    let BlockBody::Action(action) = &blocks[1].body else {
        panic!("{:?}", blocks[1]);
    };
    assert_eq!(action.ordinal, None);
    assert_eq!(action.result.as_deref(), Some("read src/a.rs\nfn a() {}"));
    assert_eq!(blocks[2].body, BlockBody::ProtocolCount(1));
    // Only All shows protocol counts.
    assert!(ConsoleFilter::All.accepts(&blocks[0].body));
    assert!(!ConsoleFilter::Actions.accepts(&blocks[0].body));
}

#[test]
fn prose_joins_deltas_and_a_repeated_message_does_not_duplicate() {
    let events = [
        event(
            1,
            TerminalEventKind::Message,
            Some("stdout"),
            "The second line",
        ),
        event(
            2,
            TerminalEventKind::Message,
            Some("stdout"),
            " is the answer.",
        ),
        event(
            3,
            TerminalEventKind::Message,
            Some("stdout"),
            "The second line is the answer.",
        ),
        call(4, 1, "Read", "notes.txt"),
        event(5, TerminalEventKind::Message, Some("stdout"), "Done."),
    ];
    let blocks = project(&events).blocks().to_vec();
    assert_eq!(blocks.len(), 3);
    assert_eq!(
        blocks[0].body,
        BlockBody::Prose("The second line is the answer.".into())
    );
    assert_eq!(blocks[2].body, BlockBody::Prose("Done.".into()));
}

#[test]
fn output_groups_by_stream_and_notices_carry_their_level() {
    let events = [
        event(1, TerminalEventKind::Stdout, Some("stdout"), "line 1"),
        event(2, TerminalEventKind::Stdout, Some("stdout"), "line 2"),
        event(3, TerminalEventKind::Stderr, Some("stderr"), "warning: x"),
        event(4, TerminalEventKind::Error, None, "The CLI failed."),
        TerminalEvent {
            detail: Some(EventDetailV1::Withheld {
                v: 1,
                reason: WithheldReason::Cancelled,
            }),
            ..event(
                5,
                TerminalEventKind::System,
                None,
                "Output withheld: run cancelled.",
            )
        },
        event(6, TerminalEventKind::System, None, "CLI timing: fixture"),
    ];
    let blocks = project(&events).blocks().to_vec();
    assert_eq!(blocks.len(), 4, "{blocks:#?}");
    assert_eq!(
        blocks[0].body,
        BlockBody::Output {
            stream: OutputStream::Stdout,
            text: "line 1\nline 2".into()
        }
    );
    assert!(matches!(
        blocks[1].body,
        BlockBody::Output {
            stream: OutputStream::Stderr,
            ..
        }
    ));
    assert!(matches!(
        blocks[3].body,
        BlockBody::Notice {
            level: NoticeLevel::Warning,
            ..
        }
    ));
    // Errors shows stderr, notices and failed actions; never prose.
    let errors = blocks
        .iter()
        .filter(|block| ConsoleFilter::Errors.accepts(&block.body))
        .count();
    assert_eq!(errors, 3);
}

#[test]
fn filters_apply_after_grouping() {
    let events = [
        call(1, 1, "Read", "a.rs"),
        result(2, 1, ActionStatus::Ok, None),
        event(3, TerminalEventKind::Message, Some("stdout"), "text"),
        call(4, 2, "Bash", "cargo test"),
        result(5, 2, ActionStatus::Error, Some("1 failed")),
    ];
    let projector = project(&events);
    let count = |filter: ConsoleFilter| {
        projector
            .blocks()
            .iter()
            .filter(|block| filter.accepts(&block.body))
            .count()
    };
    assert_eq!(count(ConsoleFilter::All), 3);
    assert_eq!(count(ConsoleFilter::Actions), 2);
    assert_eq!(count(ConsoleFilter::Messages), 1);
    assert_eq!(count(ConsoleFilter::Output), 0);
    assert_eq!(count(ConsoleFilter::Errors), 1);
}

#[test]
fn search_matches_block_text_ignoring_case() {
    let events = [
        call(1, 1, "Read", "src/Main.rs"),
        result(2, 1, ActionStatus::Ok, Some("fn MAIN() {}")),
        event(
            3,
            TerminalEventKind::Message,
            Some("stdout"),
            "nothing here",
        ),
    ];
    let projector = project(&events);
    let rows = projector
        .blocks()
        .iter()
        .cloned()
        .map(super::model::ConsoleRow::Block)
        .collect::<Vec<_>>();
    assert_eq!(super::model::search_matches(&rows, Some("main")), vec![0]);
    assert_eq!(
        super::model::search_matches(&rows, Some("NOTHING")),
        vec![1]
    );
    assert!(super::model::search_matches(&rows, Some("  ")).is_empty());
}

#[test]
fn an_appended_event_changes_only_the_last_blocks() {
    let mut projector = project(&[
        event(1, TerminalEventKind::System, None, "Run starting."),
        call(2, 1, "Read", "a.rs"),
    ]);
    let before = projector.blocks().to_vec();
    let revision = projector.revision();
    projector.extend([&result(3, 1, ActionStatus::Ok, None)]);
    assert!(projector.revision() > revision);
    // The untouched block is the same allocation; the closed action is new.
    assert!(Rc::ptr_eq(&before[0], &projector.blocks()[0]));
    assert!(!Rc::ptr_eq(&before[1], &projector.blocks()[1]));
    // An event already seen changes nothing.
    let revision = projector.revision();
    projector.extend([&call(2, 1, "Read", "a.rs")]);
    assert_eq!(projector.revision(), revision);
}

#[test]
fn a_long_output_previews_four_hundred_lines() {
    let text = (1..=450)
        .map(|line| format!("line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let blocks = project(&[event(1, TerminalEventKind::Stdout, Some("stdout"), &text)])
        .blocks()
        .to_vec();
    let BlockBody::Output { text, .. } = &blocks[0].body else {
        panic!();
    };
    let shown = text
        .lines()
        .take(super::projection::OUTPUT_PREVIEW_LINES)
        .count();
    assert_eq!(shown, 400);
    assert_eq!(text.lines().count() - shown, 50);
}

/// Specification 13: the projection of a new event costs at most 1 ms with
/// 500 in the buffer. Debug build; a generous ceiling here, the release
/// number goes to the validation record.
#[test]
fn incremental_projection_stays_under_a_millisecond_per_event() {
    let mut events = Vec::new();
    for ordinal in 1..=250_u32 {
        let sequence = u64::from(ordinal) * 2;
        events.push(call(
            sequence - 1,
            ordinal,
            "Read",
            &format!("src/file_{ordinal}.rs"),
        ));
        events.push(result(
            sequence,
            ordinal,
            ActionStatus::Ok,
            Some("fn x() {}\n"),
        ));
    }
    let mut projector = project(&events[..498]);
    let started = Instant::now();
    projector.extend(&events[498..]);
    let per_event = started.elapsed() / 2;
    assert!(per_event.as_micros() < 1_000, "{per_event:?} per event");
}
