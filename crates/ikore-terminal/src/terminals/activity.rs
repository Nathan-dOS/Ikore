//! The live activity pulse of a CLI run (ADR-040, M5; specification 5):
//! what the agent is doing now (its phase, the actions it starts and ends,
//! a few counters), published while the run goes on. Every string field
//! follows a closed grammar: free text never travels in a pulse. A pulse is
//! ephemeral: it is never persisted and never enters the exchange log.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use ikore_contracts::AgentRole;
use serde::{Deserialize, Serialize};

use super::actions::{ActionFrame, ActionStatus, ToolKind};
use super::{TerminalEventEnvelope, TerminalEventSink};

/// The newest version of the pulse's shape.
pub const ACTIVITY_PROTOCOL_VERSION: u16 = 1;
/// Pulses a run emits at most every this long.
pub(crate) const PULSE_INTERVAL: Duration = Duration::from_millis(250);
/// Action deltas one pulse carries.
const MAX_DELTAS_PER_PULSE: usize = 32;
/// Deltas held between pulses before the oldest are dropped.
const MAX_PENDING_DELTAS: usize = 256;
/// Actions a snapshot of a live run keeps.
pub(crate) const SNAPSHOT_ACTIONS: usize = 64;
/// Characters a live workspace path keeps, the middle elided.
const MAX_PATH_CHARS: usize = 160;

/// What a run is doing (specification 1.3, with the host's own phases of
/// PD5: queued, waiting on the developer, retrying, falling back).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityPhase {
    /// Waiting for the agent's slot before the process starts.
    Queued,
    Starting,
    Thinking,
    RunningTool,
    Writing,
    /// A rate limit or a backoff the CLI reported.
    Waiting,
    /// The pulsation waits on a decision from the developer.
    WaitingOnUser,
    /// The host tries the run again.
    Retrying,
    /// The host moves the run to its fallback route.
    FallingBack,
    Finishing,
}

impl ActivityPhase {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Starting => "Starting",
            Self::Thinking => "Thinking",
            Self::RunningTool => "Running tool",
            Self::Writing => "Writing",
            Self::Waiting => "Waiting",
            Self::WaitingOnUser => "Waiting on you",
            Self::Retrying => "Retrying",
            Self::FallingBack => "Falling back",
            Self::Finishing => "Finishing",
        }
    }
}

/// A live action's target, each from its own grammar (specification 5.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum PulseTarget {
    /// Relative to the run's working directory, with `/`.
    WorkspacePath(String),
    /// A command's program, as a basename.
    Program(String),
    /// A URL's host, in lowercase ASCII.
    Host(String),
    /// A target failed its grammar, or the redactor found something in it.
    Withheld,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PulseActionDelta {
    Started {
        ordinal: u32,
        kind: ToolKind,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<PulseTarget>,
    },
    Finished {
        ordinal: u32,
        status: ActionStatus,
        duration_ms: u64,
    },
}

impl PulseActionDelta {
    pub fn ordinal(&self) -> u32 {
        match self {
            Self::Started { ordinal, .. } | Self::Finished { ordinal, .. } => *ordinal,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityCounters {
    pub tool_calls: u32,
    pub files_read: u32,
    pub files_changed: u32,
    pub searches: u32,
    pub commands: u32,
    /// Times the model went into reasoning (ADR-040 M6: phase and counts).
    pub reasoning_blocks: u32,
    /// The reasoning tokens the CLI reported so far, when it reports them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentActivityPulseV1 {
    pub protocol_version: u16,
    pub project_id: String,
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_role: Option<AgentRole>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot_id: Option<String>,
    /// Per run, monotonic; the UI drops an older one.
    pub pulse_sequence: u64,
    /// RFC 3339, milliseconds.
    pub emitted_at: String,
    pub phase: ActivityPhase,
    /// Relative, so the clocks of host and UI never meet.
    pub phase_since_ms: u64,
    pub last_activity_ms_ago: u64,
    pub actions: Vec<PulseActionDelta>,
    pub actions_dropped: u32,
    pub counters: ActivityCounters,
}

/// The latest pulse of a live run and its recent actions, for a snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentActivitySnapshotV1 {
    pub latest: AgentActivityPulseV1,
    pub recent_actions: Vec<PulseActionDelta>,
}

/// Directories and names that hold credentials (the ADR-030 catalog): a live
/// path inside one is withheld, whatever the redactor says.
fn is_protected(relative: &str) -> bool {
    relative.split('/').any(|segment| {
        let segment = segment.to_ascii_lowercase();
        matches!(
            segment.as_str(),
            ".ssh"
                | ".aws"
                | ".gnupg"
                | ".azure"
                | ".kube"
                | ".docker"
                | ".git"
                | ".npmrc"
                | ".pypirc"
                | ".netrc"
                | ".git-credentials"
        ) || segment.starts_with(".env")
            || segment.starts_with("credentials")
            || segment.starts_with("secrets")
            || segment.starts_with("id_rsa")
            || segment.starts_with("id_ed25519")
            || segment.starts_with("id_ecdsa")
            || [".pem", ".key", ".p12", ".pfx", ".keystore", ".jks"]
                .iter()
                .any(|suffix| segment.ends_with(suffix))
    })
}

/// A path as the pulse may show it: inside one of the run's working
/// directories, relative, with `/`, no control characters, not a credential
/// path, at most 160 characters. The caller still runs the redactor.
pub(crate) fn workspace_path(raw: &str, roots: &[PathBuf]) -> Option<String> {
    if raw.chars().any(char::is_control) || raw.trim().is_empty() {
        return None;
    }
    let raw = raw.trim();
    let candidate = Path::new(raw);
    let relative = if candidate.is_absolute() || raw.starts_with('/') || raw.starts_with('\\') {
        roots.iter().find_map(|root| relative_to(candidate, root))?
    } else {
        lexical(candidate)?
    };
    if relative.is_empty() || is_protected(&relative) {
        return None;
    }
    Some(elide_middle(&relative, MAX_PATH_CHARS))
}

fn relative_to(path: &Path, root: &Path) -> Option<String> {
    let strip = |text: &str| text.strip_prefix(r"\\?\").unwrap_or(text).to_owned();
    let path = strip(&path.to_string_lossy()).replace('\\', "/");
    let root = strip(&root.to_string_lossy()).replace('\\', "/");
    let root = root.trim_end_matches('/');
    if root.is_empty() {
        return None;
    }
    let (head, tail) = (path.get(..root.len())?, path.get(root.len()..)?);
    if !head.eq_ignore_ascii_case(root) || !(tail.is_empty() || tail.starts_with('/')) {
        return None;
    }
    lexical(Path::new(tail.trim_start_matches('/')))
}

/// Normal components only; `..` that would climb out fails.
fn lexical(path: &Path) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    let text = path.to_string_lossy().replace('\\', "/");
    for component in Path::new(&text).components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(parts.join("/"))
}

fn elide_middle(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_owned();
    }
    let head = (max - 1) / 2;
    let tail = max - 1 - head;
    let mut cut: String = text.chars().take(head).collect();
    cut.push('…');
    cut.extend(text.chars().skip(count - tail));
    cut
}

/// Splits a command line into words, honouring simple quotes.
fn words(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for character in command.chars() {
        match (quote, character) {
            (Some(open), close) if open == close => quote = None,
            (None, '"' | '\'') => quote = Some(character),
            (None, space) if space.is_whitespace() => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            (_, other) => current.push(other),
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// A command's program: the first word as a basename, without `.exe`,
/// `.cmd`, `.bat` or `.ps1`; through a shell wrapper, the inner command's.
pub(crate) fn program(command: &str) -> Option<String> {
    let words = words(command);
    let first = words.first()?;
    let name = basename(first);
    let lower = name.to_ascii_lowercase();
    let inner = match lower.as_str() {
        "bash" | "sh" | "zsh" => words
            .iter()
            .position(|word| word == "-c" || word == "-lc")
            .and_then(|index| words.get(index + 1)),
        "pwsh" | "powershell" => words
            .iter()
            .position(|word| {
                let flag = word.to_ascii_lowercase();
                flag == "-command" || flag == "-c"
            })
            .and_then(|index| words.get(index + 1)),
        "cmd" => words
            .iter()
            .position(|word| word.eq_ignore_ascii_case("/c"))
            .and_then(|index| words.get(index + 1)),
        _ => None,
    };
    if let Some(inner) = inner {
        return program(inner);
    }
    let fits = (1..=40).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-'));
    fits.then_some(name)
}

fn basename(word: &str) -> String {
    let name = word.rsplit(['/', '\\']).next().unwrap_or(word);
    let lower = name.to_ascii_lowercase();
    for suffix in [".exe", ".cmd", ".bat", ".ps1"] {
        if lower.ends_with(suffix) {
            return name[..name.len() - suffix.len()].to_owned();
        }
    }
    name.to_owned()
}

/// A URL's host for `http` and `https`, lowercase ASCII, at most 253
/// characters, without user, port, path or query.
pub(crate) fn url_host(url: &str) -> Option<String> {
    let lower = url.trim().to_ascii_lowercase();
    let rest = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host_port = authority.rsplit('@').next()?;
    let host = match host_port.strip_prefix('[') {
        // An IPv6 literal is shown as written, without its brackets' port.
        Some(literal) => literal.split(']').next()?.to_owned(),
        None => host_port.split(':').next()?.to_owned(),
    };
    let fits = (1..=253).contains(&host.len())
        && host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b':'));
    fits.then_some(host)
}

/// The live target of a call, by kind; `None` for kinds without one
/// (`Search`, `Task`, `Mcp`, `Plan`, `Other`). `clean` is the redactor: a
/// value it finds anything in is withheld.
pub(crate) fn pulse_target(
    action: &ActionFrame,
    roots: &[PathBuf],
    clean: impl Fn(&str) -> bool,
) -> Option<PulseTarget> {
    let checked = |value: Option<String>, wrap: fn(String) -> PulseTarget| match value {
        Some(value) if clean(&value) => wrap(value),
        _ => PulseTarget::Withheld,
    };
    match action.kind {
        ToolKind::Read | ToolKind::Write | ToolKind::Edit | ToolKind::List => {
            let raw = action.target_path.as_deref()?;
            Some(checked(
                workspace_path(raw, roots),
                PulseTarget::WorkspacePath,
            ))
        }
        ToolKind::Shell => {
            let raw = action.target_text.as_deref()?;
            Some(checked(program(raw), PulseTarget::Program))
        }
        ToolKind::Web => {
            let raw = action.target_url.as_deref()?;
            Some(checked(url_host(raw), PulseTarget::Host))
        }
        ToolKind::Search | ToolKind::Task | ToolKind::Mcp | ToolKind::Plan | ToolKind::Other => {
            None
        }
    }
}

/// Who the run is, as every pulse names it.
#[derive(Debug, Clone)]
pub(crate) struct PulseIdentity {
    pub(crate) project_id: String,
    pub(crate) run_id: String,
    pub(crate) agent_role: Option<AgentRole>,
    pub(crate) slot_id: Option<String>,
}

/// One run's pulse: fed by the coordinator with what each frame showed,
/// polled for a pulse at most every 250 ms (specification 5.3 and 5.4).
pub(crate) struct ActivityTracker {
    identity: PulseIdentity,
    started: Instant,
    phase: ActivityPhase,
    phase_since: Instant,
    last_activity: Instant,
    open: Vec<u32>,
    pending: Vec<PulseActionDelta>,
    dropped: u32,
    counters: ActivityCounters,
    sequence: u64,
    last_emitted: Option<Instant>,
    changed: bool,
}

impl ActivityTracker {
    /// The process started: the first pulse goes out on the next poll.
    pub(crate) fn start(identity: PulseIdentity, now: Instant) -> Self {
        Self {
            identity,
            started: now,
            phase: ActivityPhase::Starting,
            phase_since: now,
            last_activity: now,
            open: Vec::new(),
            pending: Vec::new(),
            dropped: 0,
            counters: ActivityCounters::default(),
            sequence: 0,
            last_emitted: None,
            changed: true,
        }
    }

    #[cfg(test)]
    pub(crate) fn phase(&self) -> ActivityPhase {
        self.phase
    }

    fn enter(&mut self, phase: ActivityPhase, now: Instant) {
        if self.phase != phase {
            if phase == ActivityPhase::Thinking && self.phase != ActivityPhase::Thinking {
                self.counters.reasoning_blocks = self.counters.reasoning_blocks.saturating_add(1);
            }
            self.phase = phase;
            self.phase_since = now;
            self.changed = true;
        }
    }

    fn touch(&mut self, now: Instant) {
        self.last_activity = now;
    }

    fn push(&mut self, delta: PulseActionDelta) {
        self.pending.push(delta);
        if self.pending.len() > MAX_PENDING_DELTAS {
            let excess = self.pending.len() - MAX_PENDING_DELTAS;
            self.pending.drain(..excess);
            self.dropped = self
                .dropped
                .saturating_add(u32::try_from(excess).unwrap_or(u32::MAX));
        }
        self.changed = true;
    }

    /// A frame said the model is reasoning.
    pub(crate) fn reasoning(&mut self, now: Instant) {
        self.touch(now);
        if self.open.is_empty() {
            // The thinking that opens the turn counts as a block, but entering
            // from `Starting` is the same episode as the first frame's.
            self.enter(ActivityPhase::Thinking, now);
        }
    }

    /// Visible model text arrived.
    pub(crate) fn writing(&mut self, now: Instant) {
        self.touch(now);
        if self.open.is_empty() {
            self.enter(ActivityPhase::Writing, now);
        }
    }

    /// The CLI reported a rate limit or a backoff.
    pub(crate) fn waiting(&mut self, now: Instant) {
        self.touch(now);
        self.enter(ActivityPhase::Waiting, now);
    }

    /// The CLI's completion frame arrived.
    pub(crate) fn completed(&mut self, now: Instant) {
        self.touch(now);
        self.enter(ActivityPhase::Finishing, now);
    }

    /// Any other frame: activity, without a phase of its own.
    pub(crate) fn activity(&mut self, now: Instant) {
        self.touch(now);
    }

    pub(crate) fn call(
        &mut self,
        ordinal: u32,
        kind: ToolKind,
        name: &str,
        target: Option<PulseTarget>,
        now: Instant,
    ) {
        self.touch(now);
        self.open.push(ordinal);
        self.counters.tool_calls = self.counters.tool_calls.saturating_add(1);
        let counter = match kind {
            ToolKind::Read | ToolKind::List => Some(&mut self.counters.files_read),
            ToolKind::Write | ToolKind::Edit => Some(&mut self.counters.files_changed),
            ToolKind::Search => Some(&mut self.counters.searches),
            ToolKind::Shell => Some(&mut self.counters.commands),
            _ => None,
        };
        if let Some(counter) = counter {
            *counter = counter.saturating_add(1);
        }
        self.push(PulseActionDelta::Started {
            ordinal,
            kind,
            name: name.to_owned(),
            target,
        });
        self.enter(ActivityPhase::RunningTool, now);
    }

    pub(crate) fn result(
        &mut self,
        ordinal: u32,
        status: ActionStatus,
        duration_ms: u64,
        now: Instant,
    ) {
        self.touch(now);
        let was_last = self.open.last() == Some(&ordinal);
        self.open.retain(|open| *open != ordinal);
        self.push(PulseActionDelta::Finished {
            ordinal,
            status,
            duration_ms,
        });
        if was_last && self.open.is_empty() {
            self.enter(ActivityPhase::Thinking, now);
        }
    }

    pub(crate) fn tokens(&mut self, output: Option<u64>, reasoning: Option<u64>) {
        if output.is_some() && output != self.counters.output_tokens {
            self.counters.output_tokens = output;
            self.changed = true;
        }
        if reasoning.is_some() && reasoning != self.counters.reasoning_tokens {
            self.counters.reasoning_tokens = reasoning;
            self.changed = true;
        }
    }

    /// Whether a pulse is waiting for its interval.
    pub(crate) fn pending(&self) -> bool {
        self.changed
    }

    /// The next pulse, when something changed and the interval passed.
    pub(crate) fn poll(&mut self, now: Instant) -> Option<AgentActivityPulseV1> {
        let due = self
            .last_emitted
            .is_none_or(|last| now.saturating_duration_since(last) >= PULSE_INTERVAL);
        (self.changed && due).then(|| self.emit(now))
    }

    /// The last pulse, before the run's terminal status.
    pub(crate) fn finish(&mut self, now: Instant) -> AgentActivityPulseV1 {
        self.enter(ActivityPhase::Finishing, now);
        self.emit(now)
    }

    fn emit(&mut self, now: Instant) -> AgentActivityPulseV1 {
        let take = self.pending.len().min(MAX_DELTAS_PER_PULSE);
        let actions = self.pending.drain(..take).collect::<Vec<_>>();
        // What did not fit waits for the next pulse.
        self.changed = !self.pending.is_empty();
        self.sequence += 1;
        self.last_emitted = Some(now);
        let millis = |duration: Duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
        self.counters.elapsed_ms = millis(now.saturating_duration_since(self.started));
        AgentActivityPulseV1 {
            protocol_version: ACTIVITY_PROTOCOL_VERSION,
            project_id: self.identity.project_id.clone(),
            run_id: self.identity.run_id.clone(),
            agent_role: self.identity.agent_role,
            slot_id: self.identity.slot_id.clone(),
            pulse_sequence: self.sequence,
            emitted_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            phase: self.phase,
            phase_since_ms: millis(now.saturating_duration_since(self.phase_since)),
            last_activity_ms_ago: millis(now.saturating_duration_since(self.last_activity)),
            actions,
            actions_dropped: std::mem::take(&mut self.dropped),
            counters: self.counters.clone(),
        }
    }
}

/// Keeps each live run's latest pulse and its recent actions, for a snapshot
/// of a UI that opens mid-run, and passes every envelope and pulse on
/// (specification 5.5). A run's entry goes once an envelope says it ended.
pub(crate) struct LiveActivityRecorder {
    inner: Arc<dyn TerminalEventSink>,
    runs: Mutex<HashMap<String, AgentActivitySnapshotV1>>,
}

impl LiveActivityRecorder {
    pub(crate) fn new(inner: Arc<dyn TerminalEventSink>) -> Self {
        Self {
            inner,
            runs: Mutex::new(HashMap::new()),
        }
    }

    /// The live runs of `project_id`.
    pub(crate) fn snapshots(&self, project_id: &str) -> Vec<AgentActivitySnapshotV1> {
        let mut snapshots = self
            .runs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .filter(|snapshot| snapshot.latest.project_id == project_id)
            .cloned()
            .collect::<Vec<_>>();
        snapshots.sort_by(|left, right| left.latest.run_id.cmp(&right.latest.run_id));
        snapshots
    }
}

impl TerminalEventSink for LiveActivityRecorder {
    fn emit(&self, envelope: &TerminalEventEnvelope) {
        if envelope.run.status.is_terminal() {
            self.runs
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&envelope.run_id);
        }
        self.inner.emit(envelope);
    }

    fn emit_activity(&self, pulse: &AgentActivityPulseV1) {
        {
            let mut runs = self.runs.lock().unwrap_or_else(PoisonError::into_inner);
            let entry =
                runs.entry(pulse.run_id.clone())
                    .or_insert_with(|| AgentActivitySnapshotV1 {
                        latest: pulse.clone(),
                        recent_actions: Vec::new(),
                    });
            if pulse.pulse_sequence >= entry.latest.pulse_sequence {
                entry.recent_actions.extend(pulse.actions.iter().cloned());
                if entry.recent_actions.len() > SNAPSHOT_ACTIONS {
                    let excess = entry.recent_actions.len() - SNAPSHOT_ACTIONS;
                    entry.recent_actions.drain(..excess);
                }
                // The actions live in `recent_actions`; the latest pulse keeps the rest.
                entry.latest = AgentActivityPulseV1 {
                    actions: Vec::new(),
                    ..pulse.clone()
                };
            }
        }
        self.inner.emit_activity(pulse);
    }
}
