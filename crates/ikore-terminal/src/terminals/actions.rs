//! What a CLI agent does, as the agent console reads it (ADR-040, M1): one
//! summary per tool call and one per result, built by the host from the
//! frames. Names come from closed allowlists and targets are normalized, so a
//! summary carries no model text except a target, which `finish()` redacts.

use std::path::{Path, PathBuf};
use std::time::Instant;

use regex::{Captures, Regex};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::CliBackend;

/// The closed vocabulary of tool kinds (specification, section 1.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    Read,
    Write,
    Edit,
    Search,
    List,
    Shell,
    Web,
    Mcp,
    Task,
    Plan,
    Other,
}

impl ToolKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Read => "Read",
            Self::Write => "Write",
            Self::Edit => "Edit",
            Self::Search => "Search",
            Self::List => "List",
            Self::Shell => "Shell",
            Self::Web => "Web",
            Self::Mcp => "MCP",
            Self::Task => "Task",
            Self::Plan => "Plan",
            Self::Other => "Tool",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionStatus {
    Ok,
    Error,
    /// Only from a structured signal; free text is never read for it.
    Denied,
    /// The stream ended before the result arrived.
    Unknown,
}

impl ActionStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
            Self::Denied => "denied",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionFramePhase {
    Call,
    Result,
    /// One frame both opens and closes the action (`OpenCode`), or closes one a
    /// previous frame opened under the same key (Codex, Antigravity).
    CallWithResult,
}

/// One tool frame as an adapter reads it. The target and output fields are
/// raw: they reach SQLite only through `finish()`'s redaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionFrame {
    /// The provider's id for the call, at most 128 bytes; memory only.
    pub call_key: Option<String>,
    pub phase: ActionFramePhase,
    pub kind: ToolKind,
    /// The CLI's own name when its allowlist knows it, `other` otherwise. A
    /// result frame may leave it empty; the ledger takes the call's.
    pub name: String,
    pub target_text: Option<String>,
    pub target_path: Option<String>,
    pub target_url: Option<String>,
    pub status: Option<ActionStatus>,
    pub exit_code: Option<i32>,
    pub output_lines: Option<u32>,
    pub output_bytes: Option<u64>,
    /// The result's visible text, where the CLI reported one: the content a
    /// `tool_result` event carries (specification, section 3.4).
    pub output: Option<String>,
}

impl ActionFrame {
    /// A call of `raw_name` with its JSON `input`, named the way `backend` does.
    pub(crate) fn call(
        backend: CliBackend,
        call_key: Option<String>,
        raw_name: &str,
        input: &Value,
    ) -> Self {
        let (kind, name) = resolve_tool(backend, raw_name);
        let (target_text, target_path, target_url) = raw_target(kind, input);
        Self {
            call_key: call_key.map(bounded_key),
            phase: ActionFramePhase::Call,
            kind,
            name,
            target_text,
            target_path,
            target_url,
            status: None,
            exit_code: None,
            output_lines: None,
            output_bytes: None,
            output: None,
        }
    }

    /// The result of the call `call_key`; the ledger supplies its name and kind.
    pub(crate) fn result(
        call_key: Option<String>,
        status: ActionStatus,
        exit_code: Option<i32>,
        output: Option<String>,
    ) -> Self {
        Self {
            call_key: call_key.map(bounded_key),
            phase: ActionFramePhase::Result,
            kind: ToolKind::Other,
            name: String::new(),
            target_text: None,
            target_path: None,
            target_url: None,
            status: None,
            exit_code: None,
            output_lines: None,
            output_bytes: None,
            output: None,
        }
        .with_outcome(status, exit_code, output)
    }

    /// A call and its outcome in one frame.
    pub(crate) fn call_with_result(
        backend: CliBackend,
        call_key: Option<String>,
        raw_name: &str,
        input: &Value,
        status: ActionStatus,
        exit_code: Option<i32>,
        output: Option<String>,
    ) -> Self {
        Self {
            phase: ActionFramePhase::CallWithResult,
            ..Self::call(backend, call_key, raw_name, input)
        }
        .with_outcome(status, exit_code, output)
    }

    /// A result that names its tool, so a result without a key still finds
    /// the call it answers.
    pub(crate) fn named(mut self, backend: CliBackend, raw_name: &str) -> Self {
        (self.kind, self.name) = resolve_tool(backend, raw_name);
        self
    }

    fn with_outcome(
        mut self,
        status: ActionStatus,
        exit_code: Option<i32>,
        output: Option<String>,
    ) -> Self {
        let output = output.filter(|text| !text.is_empty());
        self.status = Some(status);
        self.exit_code = exit_code;
        self.output_lines = output.as_deref().map(line_count);
        self.output_bytes = output.as_deref().map(|text| text.len() as u64);
        self.output = output;
        self
    }
}

fn bounded_key(key: String) -> String {
    if key.len() <= 128 {
        return key;
    }
    let mut end = 128;
    while !key.is_char_boundary(end) {
        end -= 1;
    }
    key[..end].to_owned()
}

fn line_count(text: &str) -> u32 {
    u32::try_from(text.lines().count()).unwrap_or(u32::MAX)
}

/// The structured part of a terminal event (`detail_json`). It never holds
/// free text: a target stays in the redacted `content`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventDetailV1 {
    ActionCall {
        v: u8,
        ordinal: u32,
        kind: ToolKind,
        name: String,
    },
    ActionResult {
        v: u8,
        ordinal: u32,
        status: ActionStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lines: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bytes: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
    },
    Withheld {
        v: u8,
        reason: WithheldReason,
    },
}

/// Why a run published only its action summaries (specification, section 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WithheldReason {
    OutputBudget,
    IncompleteExecution,
    UnverifiedModel,
    IncompleteStream,
    OutputTruncated,
    Cancelled,
    TimedOut,
}

impl WithheldReason {
    pub const fn text(self) -> &'static str {
        match self {
            Self::OutputBudget => "Output withheld: output budget exceeded.",
            Self::IncompleteExecution => "Output withheld: incomplete execution.",
            Self::UnverifiedModel => "Output withheld: model identity not verified.",
            Self::IncompleteStream => "Output withheld: incomplete stream.",
            Self::OutputTruncated => "Output withheld: output truncated by the provider.",
            Self::Cancelled => "Output withheld: run cancelled.",
            Self::TimedOut => "Output withheld: run timed out.",
        }
    }
}

/// The kind of `raw_name` in `backend`'s vocabulary and the name the console
/// shows: the CLI's own when the allowlist knows it, `other` otherwise.
pub(crate) fn resolve_tool(backend: CliBackend, raw_name: &str) -> (ToolKind, String) {
    if let Some(name) = mcp_name(backend, raw_name) {
        return (ToolKind::Mcp, name);
    }
    match known_tool(backend, raw_name) {
        Some(kind) if name_fits(raw_name) => (kind, raw_name.to_owned()),
        _ => (ToolKind::Other, "other".to_owned()),
    }
}

/// `mcp:<server>.<tool>`, when both parts fit `^[A-Za-z0-9_-]{1,32}$`.
pub(crate) fn mcp_display_name(server: &str, tool: &str) -> Option<String> {
    (segment_fits(server) && segment_fits(tool)).then(|| format!("mcp:{server}.{tool}"))
}

fn mcp_name(backend: CliBackend, raw: &str) -> Option<String> {
    match backend {
        // Claude names them `mcp__<server>__<tool>`.
        CliBackend::Claude => {
            let (server, tool) = raw.strip_prefix("mcp__")?.split_once("__")?;
            mcp_display_name(server, tool)
        }
        _ => raw
            .strip_prefix("mcp:")
            .and_then(|rest| rest.split_once('.'))
            .and_then(|(server, tool)| mcp_display_name(server, tool)),
    }
}

fn name_fits(name: &str) -> bool {
    (1..=48).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b':' | b'-'))
}

fn segment_fits(segment: &str) -> bool {
    (1..=32).contains(&segment.len())
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

/// The allowlist of each CLI: the names it gives its tools (section 1.2, and
/// the Etapa 0 corpus for Antigravity, which also uses older names in steps).
fn known_tool(backend: CliBackend, name: &str) -> Option<ToolKind> {
    use ToolKind::{Edit, List, Mcp, Plan, Read, Search, Shell, Task, Web, Write};
    let kind = match backend {
        CliBackend::Claude => match name {
            "Read" => Read,
            "Write" => Write,
            "Edit" | "MultiEdit" | "NotebookEdit" => Edit,
            "Grep" | "Glob" => Search,
            "LS" => List,
            "Bash" | "BashOutput" | "KillShell" => Shell,
            "WebFetch" | "WebSearch" => Web,
            "Task" | "Agent" => Task,
            "TodoWrite" => Plan,
            _ => return None,
        },
        CliBackend::Gemini => match name {
            "read_file" | "read_many_files" => Read,
            "write_file" => Write,
            "replace" => Edit,
            "glob" | "search_file_content" => Search,
            "list_directory" => List,
            "run_shell_command" => Shell,
            "web_fetch" | "google_web_search" => Web,
            _ => return None,
        },
        CliBackend::Opencode => match name {
            "read" => Read,
            "write" => Write,
            "edit" | "patch" | "apply_patch" | "multiedit" => Edit,
            "grep" | "glob" => Search,
            "list" => List,
            "bash" => Shell,
            "webfetch" | "websearch" => Web,
            "task" => Task,
            "todowrite" | "todoread" => Plan,
            _ => return None,
        },
        // Codex names actions by item type; the adapter maps them to these.
        CliBackend::Codex => match name {
            "shell" => Shell,
            "edit" => Edit,
            "web_search" => Web,
            "todo_list" => Plan,
            _ => return None,
        },
        CliBackend::Antigravity => match name {
            "read_file" | "view_file" | "read_resource" => Read,
            "write_file" | "write_to_file" => Write,
            "replace_file_content"
            | "multi_replace_file_content"
            | "sed_file"
            | "notebook_edit" => Edit,
            "grep_search" | "find_by_name" => Search,
            "list_dir" | "list_resources" => List,
            "run_command" | "send_command_input" | "command_status" => Shell,
            "read_url_content" | "search_web" | "open_browser_url" | "read_browser_page" => Web,
            "call_mcp_tool" => Mcp,
            "invoke_subagent" | "browser_subagent" | "define_subagent" | "manage_subagents" => Task,
            "manage_task" => Plan,
            _ => return None,
        },
        CliBackend::Custom => return None,
    };
    Some(kind)
}

const PATH_FIELDS: [&str; 10] = [
    "file_path",
    "path",
    "notebook_path",
    "absolute_path",
    "filePath",
    "Path",
    "AbsolutePath",
    "TargetFile",
    "DirectoryPath",
    "SearchPath",
];

fn string_field<'a>(input: &'a Value, names: &[&str]) -> Option<&'a str> {
    names
        .iter()
        .find_map(|name| input.get(*name).and_then(Value::as_str))
        .filter(|value| !value.trim().is_empty())
}

/// The raw target of a call, by kind (section 3.2): `(text, path, url)`.
pub(crate) fn raw_target(
    kind: ToolKind,
    input: &Value,
) -> (Option<String>, Option<String>, Option<String>) {
    match kind {
        ToolKind::Read | ToolKind::Write | ToolKind::Edit | ToolKind::List => {
            let path = string_field(input, &PATH_FIELDS).map(str::to_owned);
            (path.clone(), path, None)
        }
        ToolKind::Search => {
            let pattern = string_field(
                input,
                &[
                    "pattern",
                    "query",
                    "Pattern",
                    "Query",
                    "SearchPattern",
                    "regex",
                ],
            );
            let path = string_field(input, &PATH_FIELDS);
            let text = match (pattern, path) {
                (Some(pattern), Some(path)) => Some(format!("\"{pattern}\" in {path}")),
                (Some(pattern), None) => Some(format!("\"{pattern}\"")),
                (None, Some(path)) => Some(path.to_owned()),
                (None, None) => None,
            };
            (text, path.map(str::to_owned), None)
        }
        ToolKind::Shell => {
            let command = string_field(input, &["command", "CommandLine", "cmd"])
                .map(str::to_owned)
                .or_else(|| {
                    input.get("command").and_then(Value::as_array).map(|argv| {
                        argv.iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                })
                .filter(|command| !command.trim().is_empty());
            (command, None, None)
        }
        ToolKind::Web => {
            let url = string_field(input, &["url", "Url", "URL"]);
            let text = url.or_else(|| string_field(input, &["query", "Query"]));
            (text.map(str::to_owned), None, url.map(str::to_owned))
        }
        ToolKind::Task => (
            string_field(input, &["description", "Description"]).map(str::to_owned),
            None,
            None,
        ),
        ToolKind::Mcp | ToolKind::Plan | ToolKind::Other => (None, None, None),
    }
}

/// Characters a target keeps, the middle elided.
const MAX_TARGET_CHARS: usize = 240;

/// One working directory of a run, as a pattern that finds it in a target at
/// either slash and any case, followed by its separator or by the end of the
/// path. A sibling such as `repo-old` does not match `repo`.
pub(crate) fn root_pattern(root: &Path) -> Option<Regex> {
    let text = root.to_string_lossy();
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
    let parts = text
        .split(['\\', '/'])
        .filter(|part| !part.is_empty())
        .map(regex::escape)
        .collect::<Vec<_>>();
    if parts.is_empty() {
        return None;
    }
    Regex::new(&format!(
        r#"(?i)[\\/]*{}(?:[\\/]+(?P<rel>[^\s"'`*?<>|]*)|(?P<end>$|[\s"'`,;)\]]))"#,
        parts.join(r"[\\/]+")
    ))
    .ok()
}

/// The target as the console shows it: paths under a working directory of
/// the run become relative, with `/`; line breaks become spaces; at most 240
/// characters remain, the middle elided.
pub(crate) fn display_target(target: &str, roots: &[Regex]) -> String {
    let mut text = target.replace(['\r', '\n'], " ");
    for root in roots {
        if root.is_match(&text) {
            text = root
                .replace_all(&text, |captures: &Captures<'_>| {
                    match (captures.name("rel"), captures.name("end")) {
                        (Some(relative), _) if !relative.as_str().is_empty() => {
                            relative.as_str().replace('\\', "/")
                        }
                        (_, Some(end)) => format!(".{}", end.as_str()),
                        _ => ".".to_owned(),
                    }
                })
                .into_owned();
        }
    }
    let count = text.chars().count();
    if count <= MAX_TARGET_CHARS {
        return text;
    }
    let head = (MAX_TARGET_CHARS - 1) / 2;
    let tail = MAX_TARGET_CHARS - 1 - head;
    let mut cut: String = text.chars().take(head).collect();
    cut.push('…');
    cut.extend(text.chars().skip(count - tail));
    cut
}

/// An open call, waiting for its result.
struct OpenAction {
    ordinal: u32,
    key: Option<String>,
    kind: ToolKind,
    name: String,
    started: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActionCallEntry {
    pub ordinal: u32,
    pub kind: ToolKind,
    pub name: String,
    pub target: Option<String>,
}

impl ActionCallEntry {
    /// `"{name} · {target}"`, or the name alone.
    pub(crate) fn summary(&self) -> String {
        match &self.target {
            Some(target) => format!("{} \u{b7} {target}", self.name),
            None => self.name.clone(),
        }
    }

    pub(crate) fn detail(&self) -> EventDetailV1 {
        EventDetailV1::ActionCall {
            v: 1,
            ordinal: self.ordinal,
            kind: self.kind,
            name: self.name.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActionResultEntry {
    pub ordinal: u32,
    pub kind: ToolKind,
    pub name: String,
    pub status: ActionStatus,
    pub exit_code: Option<i32>,
    pub lines: Option<u32>,
    pub bytes: Option<u64>,
    pub duration_ms: Option<u64>,
}

impl ActionResultEntry {
    /// `"{name} · ok"`, `"{name} · exit 1"`, `"{name} · denied"` and so on.
    pub(crate) fn summary(&self) -> String {
        let outcome = match self.exit_code {
            Some(code) if code != 0 => format!("exit {code}"),
            _ => self.status.as_str().to_owned(),
        };
        format!("{} \u{b7} {outcome}", self.name)
    }

    pub(crate) fn detail(&self) -> EventDetailV1 {
        EventDetailV1::ActionResult {
            v: 1,
            ordinal: self.ordinal,
            status: self.status,
            exit_code: self.exit_code,
            lines: self.lines,
            bytes: self.bytes,
            duration_ms: self.duration_ms,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LedgerEntry {
    Call(ActionCallEntry),
    Result(ActionResultEntry),
}

/// The actions of one run (section 3.3): ordinals, pairing and durations.
pub(crate) struct ActionLedger {
    roots: Vec<Regex>,
    next_ordinal: u32,
    open: Vec<OpenAction>,
}

impl ActionLedger {
    /// `roots` are the run's working directories: its snapshot or workspace.
    pub(crate) fn new(roots: &[PathBuf]) -> Self {
        Self {
            roots: roots.iter().filter_map(|root| root_pattern(root)).collect(),
            next_ordinal: 0,
            open: Vec::new(),
        }
    }

    pub(crate) fn observe(&mut self, frame: &ActionFrame, at: Instant) -> Vec<LedgerEntry> {
        match frame.phase {
            ActionFramePhase::Call => vec![self.open_call(frame, at)],
            ActionFramePhase::Result => vec![self.close(frame, at)],
            // A call this run already opened only closes; otherwise the frame
            // opens and closes its own.
            ActionFramePhase::CallWithResult if self.open_index(frame).is_some() => {
                vec![self.close(frame, at)]
            }
            ActionFramePhase::CallWithResult => {
                let call = self.open_call(frame, at);
                vec![call, self.close(frame, at)]
            }
        }
    }

    /// The calls still open when the stream ended, as results of unknown status.
    pub(crate) fn close_open(&mut self) -> Vec<LedgerEntry> {
        self.open
            .drain(..)
            .map(|action| {
                LedgerEntry::Result(ActionResultEntry {
                    ordinal: action.ordinal,
                    kind: action.kind,
                    name: action.name,
                    status: ActionStatus::Unknown,
                    exit_code: None,
                    lines: None,
                    bytes: None,
                    duration_ms: None,
                })
            })
            .collect()
    }

    fn next(&mut self) -> u32 {
        self.next_ordinal = self.next_ordinal.saturating_add(1);
        self.next_ordinal
    }

    fn open_call(&mut self, frame: &ActionFrame, at: Instant) -> LedgerEntry {
        let ordinal = self.next();
        self.open.push(OpenAction {
            ordinal,
            key: frame.call_key.clone(),
            kind: frame.kind,
            name: frame.name.clone(),
            started: at,
        });
        LedgerEntry::Call(ActionCallEntry {
            ordinal,
            kind: frame.kind,
            name: frame.name.clone(),
            target: frame
                .target_text
                .as_deref()
                .map(|target| display_target(target, &self.roots)),
        })
    }

    /// The open call a result answers: by key; without one, the oldest open
    /// call with the same name, or the oldest open call when it names nothing.
    fn open_index(&self, frame: &ActionFrame) -> Option<usize> {
        match &frame.call_key {
            Some(key) => self
                .open
                .iter()
                .position(|action| action.key.as_ref() == Some(key)),
            None if !frame.name.is_empty() => self
                .open
                .iter()
                .position(|action| action.name == frame.name),
            None => (!self.open.is_empty()).then_some(0),
        }
    }

    fn close(&mut self, frame: &ActionFrame, at: Instant) -> LedgerEntry {
        let (ordinal, kind, name, duration_ms) = match self.open_index(frame) {
            Some(index) => {
                let action = self.open.remove(index);
                let elapsed = at.saturating_duration_since(action.started).as_millis();
                (
                    action.ordinal,
                    action.kind,
                    action.name,
                    Some(u64::try_from(elapsed).unwrap_or(u64::MAX)),
                )
            }
            // A result without a call this run saw: it still gets a place.
            None => (
                self.next(),
                frame.kind,
                if frame.name.is_empty() {
                    "other".to_owned()
                } else {
                    frame.name.clone()
                },
                None,
            ),
        };
        LedgerEntry::Result(ActionResultEntry {
            ordinal,
            kind,
            name,
            status: frame.status.unwrap_or(ActionStatus::Unknown),
            exit_code: frame.exit_code,
            lines: frame.output_lines,
            bytes: frame.output_bytes,
            duration_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::*;

    fn read(key: Option<&str>, path: &str) -> ActionFrame {
        ActionFrame::call(
            CliBackend::Claude,
            key.map(str::to_owned),
            "Read",
            &json!({ "file_path": path }),
        )
    }

    fn roots(paths: &[&str]) -> Vec<Regex> {
        paths
            .iter()
            .filter_map(|path| root_pattern(Path::new(path)))
            .collect()
    }

    #[test]
    fn ledger_pairs_by_call_key_and_falls_back_by_name() {
        let start = Instant::now();
        let mut ledger = ActionLedger::new(&[PathBuf::from(r"C:\work\repo")]);
        let first = ledger.observe(&read(Some("a"), r"C:\work\repo\src\a.rs"), start);
        let second = ledger.observe(&read(Some("b"), r"C:\work\repo\src\b.rs"), start);
        assert_eq!(
            first,
            [LedgerEntry::Call(ActionCallEntry {
                ordinal: 1,
                kind: ToolKind::Read,
                name: "Read".into(),
                target: Some("src/a.rs".into()),
            })]
        );
        assert!(matches!(&second[0], LedgerEntry::Call(call) if call.ordinal == 2));
        // The second call answers first, by key.
        let later = start + Duration::from_millis(40);
        let answer = ledger.observe(
            &ActionFrame::result(
                Some("b".into()),
                ActionStatus::Ok,
                None,
                Some("x\ny".into()),
            ),
            later,
        );
        assert_eq!(
            answer,
            [LedgerEntry::Result(ActionResultEntry {
                ordinal: 2,
                kind: ToolKind::Read,
                name: "Read".into(),
                status: ActionStatus::Ok,
                exit_code: None,
                lines: Some(2),
                bytes: Some(3),
                duration_ms: Some(40),
            })]
        );
        // Without a key, the oldest open call with the same name.
        let unnamed = ActionFrame::result(None, ActionStatus::Error, None, None)
            .named(CliBackend::Claude, "Read");
        let answer = ledger.observe(&unnamed, later);
        assert!(matches!(&answer[0], LedgerEntry::Result(result) if result.ordinal == 1));
        // A result nobody called still gets a place.
        let orphan = ledger.observe(
            &ActionFrame::result(Some("z".into()), ActionStatus::Ok, None, None),
            later,
        );
        assert!(
            matches!(&orphan[0], LedgerEntry::Result(result) if result.ordinal == 3 && result.name == "other")
        );
    }

    #[test]
    fn ledger_closes_open_actions_as_unknown() {
        let mut ledger = ActionLedger::new(&[]);
        ledger.observe(&read(Some("a"), "notes.txt"), Instant::now());
        let closed = ledger.close_open();
        assert!(matches!(
            &closed[..],
            [LedgerEntry::Result(ActionResultEntry {
                ordinal: 1,
                status: ActionStatus::Unknown,
                ..
            })]
        ));
        assert!(ledger.close_open().is_empty());
    }

    #[test]
    fn a_frame_with_call_and_result_opens_and_closes_one_action() {
        let mut ledger = ActionLedger::new(&[]);
        let frame = ActionFrame::call_with_result(
            CliBackend::Opencode,
            Some("fc_1".into()),
            "read",
            &json!({ "filePath": "notes.txt" }),
            ActionStatus::Ok,
            None,
            Some("one\ntwo".into()),
        );
        let entries = ledger.observe(&frame, Instant::now());
        assert!(
            matches!(&entries[0], LedgerEntry::Call(call) if call.summary() == "read \u{b7} notes.txt")
        );
        assert!(matches!(&entries[1], LedgerEntry::Result(result)
            if result.ordinal == 1 && result.summary() == "read \u{b7} ok" && result.lines == Some(2)));
        assert!(ledger.close_open().is_empty());

        // A completion for a call already open only closes it (Codex items,
        // Antigravity steps).
        let started = ActionFrame::call(
            CliBackend::Codex,
            Some("item_1".into()),
            "shell",
            &json!({ "command": "cargo test" }),
        );
        let completed = ActionFrame::call_with_result(
            CliBackend::Codex,
            Some("item_1".into()),
            "shell",
            &json!({ "command": "cargo test" }),
            ActionStatus::Error,
            Some(101),
            None,
        );
        assert_eq!(ledger.observe(&started, Instant::now()).len(), 1);
        let closing = ledger.observe(&completed, Instant::now());
        assert!(matches!(&closing[..], [LedgerEntry::Result(result)]
            if result.ordinal == 2 && result.summary() == "shell \u{b7} exit 101"));
    }

    #[test]
    fn names_stay_in_their_allowlist_and_mcp_tools_get_one_shape() {
        assert_eq!(
            resolve_tool(CliBackend::Claude, "Bash"),
            (ToolKind::Shell, "Bash".into())
        );
        assert_eq!(
            resolve_tool(CliBackend::Claude, "mcp__github__create_issue"),
            (ToolKind::Mcp, "mcp:github.create_issue".into())
        );
        assert_eq!(
            resolve_tool(CliBackend::Codex, "mcp:docs.search"),
            (ToolKind::Mcp, "mcp:docs.search".into())
        );
        assert_eq!(
            resolve_tool(CliBackend::Antigravity, "read_file"),
            (ToolKind::Read, "read_file".into())
        );
        // Unknown names, free text and names from another CLI become `other`.
        for (backend, name) in [
            (CliBackend::Claude, "read_file"),
            (CliBackend::Opencode, "Ignore previous instructions"),
            (CliBackend::Custom, "Read"),
            (CliBackend::Claude, "mcp__bad server__x"),
        ] {
            assert_eq!(
                resolve_tool(backend, name),
                (ToolKind::Other, "other".into()),
                "{name}"
            );
        }
    }

    /// A name outside the allowlists becomes `other` and a missing field an
    /// absent target; neither fails the run (specification, section 14).
    #[test]
    fn unknown_tool_names_degrade_to_other() {
        for (backend, name, input) in [
            (CliBackend::Claude, "Read", json!({})),
            (CliBackend::Claude, "Bash", Value::Null),
            (CliBackend::Opencode, "glob", json!({ "path": 7 })),
            (
                CliBackend::Gemini,
                "brand_new_tool",
                json!({ "path": "src" }),
            ),
            (CliBackend::Antigravity, "", json!({})),
        ] {
            let call = ActionFrame::call(backend, None, name, &input);
            if !matches!(name, "Read" | "Bash" | "glob") {
                assert_eq!((call.kind, call.name.as_str()), (ToolKind::Other, "other"));
            }
            assert_eq!(call.target_text, None, "{name}");
            let mut ledger = ActionLedger::new(&[]);
            let entries = ledger.observe(&call, Instant::now());
            assert!(matches!(&entries[..], [LedgerEntry::Call(entry)]
                if entry.summary() == call.name));
        }
        let long = "a".repeat(49);
        assert_eq!(
            resolve_tool(CliBackend::Claude, &long),
            (ToolKind::Other, "other".into())
        );
    }

    #[test]
    fn targets_are_relative_one_line_and_bounded() {
        let workspace = roots(&[r"C:\work\repo"]);
        assert_eq!(
            display_target(r"C:\work\repo\src\main.rs", &workspace),
            "src/main.rs"
        );
        assert_eq!(
            display_target(r"c:/WORK/repo/docs/a.md", &workspace),
            "docs/a.md"
        );
        assert_eq!(
            display_target("\"fn main\" in C:\\work\\repo", &workspace),
            "\"fn main\" in ."
        );
        assert_eq!(
            display_target(r"type C:\work\repo\notes.txt", &workspace),
            "type notes.txt"
        );
        // A sibling directory is not the workspace.
        assert_eq!(
            display_target(r"C:\work\repo-old\a.rs", &workspace),
            r"C:\work\repo-old\a.rs"
        );
        let unix = roots(&["/home/dev/repo"]);
        assert_eq!(
            display_target("/home/dev/repo/src/lib.rs", &unix),
            "src/lib.rs"
        );
        assert_eq!(display_target("cargo test\n--all", &[]), "cargo test --all");
        let long = "x".repeat(500);
        let cut = display_target(&long, &[]);
        assert_eq!(cut.chars().count(), 240);
        assert!(cut.contains('…'));
        let (text, path, url) = raw_target(
            ToolKind::Search,
            &json!({ "pattern": "TODO", "path": "src" }),
        );
        assert_eq!(
            (text.as_deref(), path.as_deref(), url),
            (Some("\"TODO\" in src"), Some("src"), None)
        );
        let (command, ..) = raw_target(ToolKind::Shell, &json!({ "command": ["cargo", "test"] }));
        assert_eq!(command.as_deref(), Some("cargo test"));
    }

    #[test]
    fn details_serialize_with_their_type_and_without_empty_fields() {
        let call = ActionCallEntry {
            ordinal: 3,
            kind: ToolKind::Shell,
            name: "Bash".into(),
            target: Some("cargo test".into()),
        };
        assert_eq!(
            serde_json::to_value(call.detail()).unwrap(),
            json!({"type":"action_call","v":1,"ordinal":3,"kind":"shell","name":"Bash"})
        );
        let result = ActionResultEntry {
            ordinal: 3,
            kind: ToolKind::Shell,
            name: "Bash".into(),
            status: ActionStatus::Error,
            exit_code: Some(1),
            lines: None,
            bytes: None,
            duration_ms: Some(1530),
        };
        assert_eq!(result.summary(), "Bash \u{b7} exit 1");
        assert_eq!(
            serde_json::to_value(result.detail()).unwrap(),
            json!({"type":"action_result","v":1,"ordinal":3,"status":"error","exit_code":1,"duration_ms":1530})
        );
        let withheld = EventDetailV1::Withheld {
            v: 1,
            reason: WithheldReason::TimedOut,
        };
        assert_eq!(
            serde_json::to_value(&withheld).unwrap(),
            json!({"type":"withheld","v":1,"reason":"timed_out"})
        );
        assert_eq!(
            WithheldReason::TimedOut.text(),
            "Output withheld: run timed out."
        );
    }
}
