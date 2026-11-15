//! The agent console's projection (ADR-040, M2; specification 8.1 and 8.2):
//! a run's events as the blocks a developer reads (an action with its result,
//! a stretch of prose, a block of output) instead of one line per event.
//! Pure: no GPUI.

use std::collections::HashMap;
use std::rc::Rc;

use ikore_core::terminals::{
    ActionStatus, EventDetailV1, TerminalEvent, TerminalEventKind, ToolKind,
};

/// Lines an output block shows before `Show {n} more lines`.
pub(crate) const OUTPUT_PREVIEW_LINES: usize = 400;
/// Lines a collapsed result previews.
pub(crate) const RESULT_PREVIEW_LINES: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ActionState {
    Running,
    Done {
        status: ActionStatus,
        exit_code: Option<i32>,
        duration_ms: Option<u64>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActionBlock {
    pub(crate) ordinal: Option<u32>,
    pub(crate) kind: ToolKind,
    pub(crate) name: String,
    pub(crate) target: Option<String>,
    pub(crate) state: ActionState,
    /// The CLI's text for the result, when it had one.
    pub(crate) result: Option<String>,
}

impl ActionBlock {
    /// An error, a denial or a non-zero exit.
    pub(crate) fn failed(&self) -> bool {
        match &self.state {
            ActionState::Running => false,
            ActionState::Done {
                status, exit_code, ..
            } => {
                matches!(status, ActionStatus::Error | ActionStatus::Denied)
                    || exit_code.is_some_and(|code| code != 0)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutputStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoticeLevel {
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BlockBody {
    Action(ActionBlock),
    Prose(String),
    Output {
        stream: OutputStream,
        text: String,
    },
    Notice {
        level: NoticeLevel,
        text: String,
    },
    /// A lifecycle line of the host, dimmed.
    System(String),
    /// Legacy `CLI {kind} received.` lines, counted.
    ProtocolCount(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConsoleBlock {
    /// The first event's id: stable while the block grows.
    pub(crate) key: String,
    pub(crate) body: BlockBody,
}

impl ConsoleBlock {
    /// The text search and copy read: the persisted line and the result.
    pub(crate) fn text(&self) -> String {
        match &self.body {
            BlockBody::Action(action) => {
                let mut text = match &action.target {
                    Some(target) => format!("{} · {target}", action.name),
                    None => action.name.clone(),
                };
                if let Some(result) = &action.result {
                    text.push('\n');
                    text.push_str(result);
                }
                text
            }
            BlockBody::Prose(text)
            | BlockBody::Output { text, .. }
            | BlockBody::Notice { text, .. }
            | BlockBody::System(text) => text.clone(),
            BlockBody::ProtocolCount(count) => protocol_label(*count),
        }
    }
}

pub(crate) fn protocol_label(count: usize) -> String {
    if count == 1 {
        "1 protocol frame".into()
    } else {
        format!("{count} protocol frames")
    }
}

/// What the console shows (specification 8.2, rule 5), after grouping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub(crate) enum ConsoleFilter {
    #[default]
    All,
    Actions,
    Messages,
    Output,
    Errors,
}

impl ConsoleFilter {
    pub(crate) const ALL: [Self; 5] = [
        Self::All,
        Self::Actions,
        Self::Messages,
        Self::Output,
        Self::Errors,
    ];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Actions => "Actions",
            Self::Messages => "Messages",
            Self::Output => "Output",
            Self::Errors => "Errors",
        }
    }

    /// The value a choice list carries.
    pub(crate) const fn value(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Actions => "actions",
            Self::Messages => "messages",
            Self::Output => "output",
            Self::Errors => "errors",
        }
    }

    pub(crate) fn from_value(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|filter| filter.value() == value)
    }

    /// Protocol counts and lifecycle lines show only under All.
    pub(crate) fn accepts(self, body: &BlockBody) -> bool {
        match self {
            Self::All => true,
            Self::Actions => matches!(body, BlockBody::Action(_)),
            Self::Messages => matches!(body, BlockBody::Prose(_)),
            Self::Output => matches!(body, BlockBody::Output { .. }),
            Self::Errors => match body {
                BlockBody::Notice { .. } => true,
                BlockBody::Output { stream, .. } => *stream == OutputStream::Stderr,
                BlockBody::Action(action) => action.failed(),
                _ => false,
            },
        }
    }
}

/// A run's blocks, folded from its events in sequence order. A block a fold
/// changes is copied first when a console holds it, so a console finds what
/// changed by identity (`Rc::ptr_eq`).
#[derive(Debug, Default)]
pub(crate) struct Projector {
    blocks: Vec<Rc<ConsoleBlock>>,
    by_ordinal: HashMap<u32, usize>,
    /// A legacy call (no detail) that the next legacy result answers.
    legacy_open: Option<usize>,
    last_sequence: u64,
    revision: u64,
}

impl Projector {
    pub(crate) fn blocks(&self) -> &[Rc<ConsoleBlock>] {
        &self.blocks
    }

    /// Bumped on every change.
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn last_sequence(&self) -> u64 {
        self.last_sequence
    }

    /// Folds the events after the last one seen, in order.
    pub(crate) fn extend<'a>(&mut self, events: impl IntoIterator<Item = &'a TerminalEvent>) {
        for event in events {
            if event.sequence <= self.last_sequence {
                continue;
            }
            self.last_sequence = event.sequence;
            if self.fold(event) {
                self.revision += 1;
            }
        }
    }

    /// Projects `events` from scratch.
    pub(crate) fn rebuild(&mut self, events: &[TerminalEvent]) {
        *self = Self {
            revision: self.revision + 1,
            ..Self::default()
        };
        self.extend(events);
    }

    fn push(&mut self, event: &TerminalEvent, body: BlockBody) -> bool {
        self.blocks.push(Rc::new(ConsoleBlock {
            key: event.id.clone(),
            body,
        }));
        true
    }

    fn last_body(&self) -> Option<&BlockBody> {
        self.blocks.last().map(|block| &block.body)
    }

    /// The last block, copied first if a console holds it.
    fn last_mut(&mut self) -> Option<&mut ConsoleBlock> {
        self.blocks.last_mut().map(Rc::make_mut)
    }

    /// Folds one event; returns whether a block changed.
    fn fold(&mut self, event: &TerminalEvent) -> bool {
        if is_protocol_line(event) {
            if let Some(BlockBody::ProtocolCount(_)) = self.last_body()
                && let Some(ConsoleBlock {
                    body: BlockBody::ProtocolCount(count),
                    ..
                }) = self.last_mut()
            {
                *count += 1;
                return true;
            }
            return self.push(event, BlockBody::ProtocolCount(1));
        }
        match event.kind {
            TerminalEventKind::System => match &event.detail {
                Some(EventDetailV1::Withheld { .. }) => self.push(
                    event,
                    BlockBody::Notice {
                        level: NoticeLevel::Warning,
                        text: event.content.clone(),
                    },
                ),
                // Timing belongs to the footer, which reads the run.
                _ if event.content.starts_with("CLI timing:") => false,
                _ => self.push(event, BlockBody::System(event.content.clone())),
            },
            TerminalEventKind::Error => self.push(
                event,
                BlockBody::Notice {
                    level: NoticeLevel::Error,
                    text: event.content.clone(),
                },
            ),
            TerminalEventKind::Warning => self.push(
                event,
                BlockBody::Notice {
                    level: NoticeLevel::Warning,
                    text: event.content.clone(),
                },
            ),
            // The final text repeats the messages; the footer reads the run.
            TerminalEventKind::Completed => false,
            TerminalEventKind::ToolUse => self.call(event),
            TerminalEventKind::ToolResult => self.result(event),
            TerminalEventKind::Message => self.prose(event),
            TerminalEventKind::Stdout => self.output(event, OutputStream::Stdout),
            TerminalEventKind::Stderr => self.output(event, OutputStream::Stderr),
        }
    }

    fn call(&mut self, event: &TerminalEvent) -> bool {
        match &event.detail {
            Some(EventDetailV1::ActionCall {
                ordinal,
                kind,
                name,
                ..
            }) => {
                self.push(
                    event,
                    BlockBody::Action(ActionBlock {
                        ordinal: Some(*ordinal),
                        kind: *kind,
                        name: name.clone(),
                        target: target_of(&event.content, name),
                        state: ActionState::Running,
                        result: None,
                    }),
                );
                self.by_ordinal.insert(*ordinal, self.blocks.len() - 1);
                true
            }
            // Before M1 a call was the CLI's text, when it had any.
            _ => {
                self.push(
                    event,
                    BlockBody::Action(ActionBlock {
                        ordinal: None,
                        kind: ToolKind::Other,
                        name: "Tool".into(),
                        target: None,
                        state: ActionState::Running,
                        result: Some(event.content.clone()),
                    }),
                );
                self.legacy_open = Some(self.blocks.len() - 1);
                true
            }
        }
    }

    fn result(&mut self, event: &TerminalEvent) -> bool {
        let cli_text = (event.stream.as_deref() == Some("stdout")).then(|| event.content.clone());
        match &event.detail {
            Some(EventDetailV1::ActionResult {
                ordinal,
                status,
                exit_code,
                duration_ms,
                ..
            }) => {
                let state = ActionState::Done {
                    status: *status,
                    exit_code: *exit_code,
                    duration_ms: *duration_ms,
                };
                if let Some(&index) = self.by_ordinal.get(ordinal)
                    && let Some(block) = self.blocks.get_mut(index)
                    && let BlockBody::Action(action) = &mut Rc::make_mut(block).body
                {
                    action.state = state;
                    action.result = cli_text;
                    return true;
                }
                // A result whose call fell before the buffer's tail.
                let name = event
                    .content
                    .split_once(" · ")
                    .map_or("Tool", |(name, _)| name)
                    .to_owned();
                self.push(
                    event,
                    BlockBody::Action(ActionBlock {
                        ordinal: Some(*ordinal),
                        kind: ToolKind::Other,
                        name,
                        target: None,
                        state,
                        result: cli_text,
                    }),
                )
            }
            // A legacy result answers the legacy call just before it.
            _ => {
                let done = ActionState::Done {
                    status: ActionStatus::Ok,
                    exit_code: None,
                    duration_ms: None,
                };
                if let Some(index) = self.legacy_open.take()
                    && let Some(block) = self.blocks.get_mut(index)
                    && let BlockBody::Action(action) = &mut Rc::make_mut(block).body
                {
                    action.state = done;
                    action.result = Some(match action.result.take() {
                        Some(call) => format!("{call}\n{}", event.content),
                        None => event.content.clone(),
                    });
                    return true;
                }
                self.push(
                    event,
                    BlockBody::Action(ActionBlock {
                        ordinal: None,
                        kind: ToolKind::Other,
                        name: "Tool".into(),
                        target: None,
                        state: done,
                        result: Some(event.content.clone()),
                    }),
                )
            }
        }
    }

    /// Consecutive messages are one stretch of prose. Deltas continue it; a
    /// complete message that repeats the deltas replaces them.
    fn prose(&mut self, event: &TerminalEvent) -> bool {
        let text = &event.content;
        if let Some(BlockBody::Prose(current)) = self.last_body() {
            if current.ends_with(text.as_str()) && !text.starts_with(current.as_str()) {
                return false;
            }
            if let Some(ConsoleBlock {
                body: BlockBody::Prose(current),
                ..
            }) = self.last_mut()
            {
                if text.starts_with(current.as_str()) {
                    current.clone_from(text);
                } else {
                    current.push_str(text);
                }
            }
            return true;
        }
        self.push(event, BlockBody::Prose(text.clone()))
    }

    fn output(&mut self, event: &TerminalEvent, stream: OutputStream) -> bool {
        if let Some(BlockBody::Output {
            stream: current, ..
        }) = self.last_body()
            && *current == stream
            && let Some(ConsoleBlock {
                body: BlockBody::Output { text, .. },
                ..
            }) = self.last_mut()
        {
            if !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(&event.content);
            return true;
        }
        self.push(
            event,
            BlockBody::Output {
                stream,
                text: event.content.clone(),
            },
        )
    }
}

/// `"{name} · {target}"` without the name; `None` for a bare name.
fn target_of(content: &str, name: &str) -> Option<String> {
    content
        .strip_prefix(name)
        .and_then(|rest| rest.strip_prefix(" · "))
        .filter(|target| !target.is_empty())
        .map(str::to_owned)
}

/// The placeholder a protocol frame without text left (`CLI {kind}
/// received.`), and the line a history read writes for an omitted payload.
fn is_protocol_line(event: &TerminalEvent) -> bool {
    event.stream.as_deref() == Some("stdout")
        && event.content.starts_with("CLI ")
        && (event.content.ends_with(" received.")
            || event
                .content
                .ends_with("historical protocol payload omitted."))
}
