//! The durable exchange log (ADR-037 M2).
//!
//! Every domain write another part of the app may need to notice appends
//! one item here, in the same transaction. An item is a reference (the
//! domain row holds the content), ordered twice: by `seq` inside its stream
//! and by `global_seq` across the database. A doorbell on the event bus
//! announces each item after its commit; a subscriber that missed some
//! reads them back from its own cursor instead of reloading everything.

use std::collections::HashMap;

use ikore_contracts::exchange::Producer;
/// The trust class of a log item, for crates that read the log.
pub use ikore_contracts::exchange::TrustClass;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::orchestration::ExecutionFence;

/// Largest page a read returns.
pub const PAGE_LIMIT: usize = 500;
/// A follower further behind than this many full pages takes a snapshot.
pub const MAX_CATCH_UP_PAGES: usize = 20;
/// Longest idempotency key, in bytes.
pub const MAX_KEY_BYTES: usize = 200;

/// What a stream follows. One stream per subject.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamKind {
    Pulsation,
    StageExecution,
    TerminalRun,
    ChatConversation,
    Forge,
    Notes,
    Drawings,
    Docs,
}

impl StreamKind {
    pub const ALL: [Self; 8] = [
        Self::Pulsation,
        Self::StageExecution,
        Self::TerminalRun,
        Self::ChatConversation,
        Self::Forge,
        Self::Notes,
        Self::Drawings,
        Self::Docs,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pulsation => "pulsation",
            Self::StageExecution => "stage_execution",
            Self::TerminalRun => "terminal_run",
            Self::ChatConversation => "chat_conversation",
            Self::Forge => "forge",
            Self::Notes => "notes",
            Self::Drawings => "drawings",
            Self::Docs => "docs",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == value)
    }
}

/// The subject of project-wide streams (Forge backlog, notes list, docs).
pub const PROJECT_SUBJECT: &str = "project";

/// What one log item records. Kept apart from the prompt-side
/// `ExchangeKind`: the log describes domain writes, not model input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogRecordKind {
    PulsationTransition,
    CheckpointOpened,
    CheckpointResolved,
    StageFailureResolved,
    PublicationApplied,
    StageExecutionTransition,
    UnitAttempt,
    TerminalRunStatus,
    ChatTurnFinished,
    CanvasRequestChanged,
    ForgeChanged,
    NoteChanged,
    DrawingChanged,
    DocProposalChanged,
    /// Reserved for M4.
    PulsationDigestReady,
}

impl LogRecordKind {
    pub const ALL: [Self; 15] = [
        Self::PulsationTransition,
        Self::CheckpointOpened,
        Self::CheckpointResolved,
        Self::StageFailureResolved,
        Self::PublicationApplied,
        Self::StageExecutionTransition,
        Self::UnitAttempt,
        Self::TerminalRunStatus,
        Self::ChatTurnFinished,
        Self::CanvasRequestChanged,
        Self::ForgeChanged,
        Self::NoteChanged,
        Self::DrawingChanged,
        Self::DocProposalChanged,
        Self::PulsationDigestReady,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PulsationTransition => "pulsation_transition",
            Self::CheckpointOpened => "checkpoint_opened",
            Self::CheckpointResolved => "checkpoint_resolved",
            Self::StageFailureResolved => "stage_failure_resolved",
            Self::PublicationApplied => "publication_applied",
            Self::StageExecutionTransition => "stage_execution_transition",
            Self::UnitAttempt => "unit_attempt",
            Self::TerminalRunStatus => "terminal_run_status",
            Self::ChatTurnFinished => "chat_turn_finished",
            Self::CanvasRequestChanged => "canvas_request_changed",
            Self::ForgeChanged => "forge_changed",
            Self::NoteChanged => "note_changed",
            Self::DrawingChanged => "drawing_changed",
            Self::DocProposalChanged => "doc_proposal_changed",
            Self::PulsationDigestReady => "pulsation_digest_ready",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == value)
    }

    /// The only stream kind a record of this kind may be appended to.
    pub const fn stream_kind(self) -> StreamKind {
        match self {
            Self::PulsationTransition
            | Self::CheckpointOpened
            | Self::CheckpointResolved
            | Self::StageFailureResolved
            | Self::PublicationApplied
            | Self::PulsationDigestReady => StreamKind::Pulsation,
            Self::StageExecutionTransition | Self::UnitAttempt => StreamKind::StageExecution,
            Self::TerminalRunStatus => StreamKind::TerminalRun,
            Self::ChatTurnFinished | Self::CanvasRequestChanged => StreamKind::ChatConversation,
            Self::ForgeChanged => StreamKind::Forge,
            Self::NoteChanged => StreamKind::Notes,
            Self::DrawingChanged => StreamKind::Drawings,
            Self::DocProposalChanged => StreamKind::Docs,
        }
    }
}

/// The stream an item belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StreamRef {
    pub project_id: String,
    pub kind: StreamKind,
    pub subject_id: String,
}

impl StreamRef {
    pub fn new(
        project_id: impl Into<String>,
        kind: StreamKind,
        subject_id: impl Into<String>,
    ) -> Self {
        Self {
            project_id: project_id.into(),
            kind,
            subject_id: subject_id.into(),
        }
    }
}

/// The domain row that holds an item's content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayloadRef {
    pub table: &'static str,
    pub id: String,
}

/// One append. `fence`, when given, must still match the job's current
/// generation, state version and input revision in the same transaction.
#[derive(Debug, Clone)]
pub struct AppendRequest {
    pub stream: StreamRef,
    pub kind: LogRecordKind,
    pub producer: Producer,
    pub content_sha256: Option<String>,
    pub payload: Option<PayloadRef>,
    pub idempotency_key: String,
    pub fence: Option<ExecutionFence>,
}

impl AppendRequest {
    /// An append by the host, with no content digest and no fence.
    pub fn host(
        stream: StreamRef,
        kind: LogRecordKind,
        component: ikore_contracts::exchange::HostComponent,
        payload: Option<PayloadRef>,
        idempotency_key: impl Into<String>,
    ) -> Self {
        Self {
            stream,
            kind,
            producer: Producer::Host(component),
            content_sha256: None,
            payload,
            idempotency_key: idempotency_key.into(),
            fence: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    Recorded,
    /// The key was already recorded; the receipt is the original item's.
    Duplicate,
    /// The fence moved; nothing was recorded and the caller aborts.
    Fenced,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppendReceipt {
    pub disposition: Disposition,
    pub seq: u64,
    pub next_seq: u64,
    pub global_seq: i64,
}

impl AppendReceipt {
    /// Recorded and duplicate receipts pass; a fenced one becomes an error,
    /// so `?` aborts the domain write that shares the transaction.
    pub fn admitted(self) -> Result<Self, ExchangeError> {
        match self.disposition {
            Disposition::Fenced => Err(ExchangeError::Fenced),
            Disposition::Recorded | Disposition::Duplicate => Ok(self),
        }
    }
}

#[derive(Debug)]
pub enum ExchangeError {
    /// Incoherent kind, producer or stream, or a bad key: a host bug.
    InvalidRecord(String),
    /// A stream's `next_seq` disagrees with its items; fails closed.
    SequenceGap {
        stream_id: String,
        next_seq: u64,
        recorded: u64,
    },
    /// The cursor is too far behind: take a snapshot and a fresh cursor.
    StreamReset,
    /// The append's fence moved (from [`AppendReceipt::admitted`]).
    Fenced,
    Storage(rusqlite::Error),
}

impl ExchangeError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidRecord(_) => "invalid_record",
            Self::SequenceGap { .. } => "sequence_gap",
            Self::StreamReset => "stream_reset",
            Self::Fenced => "fenced",
            Self::Storage(_) => "storage",
        }
    }

    pub const fn retryable(&self) -> bool {
        matches!(self, Self::StreamReset | Self::Storage(_))
    }
}

impl std::fmt::Display for ExchangeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRecord(message) => write!(f, "exchange log: invalid record: {message}"),
            Self::SequenceGap {
                stream_id,
                next_seq,
                recorded,
            } => write!(
                f,
                "exchange log: stream {stream_id} expects seq {next_seq} but holds {recorded}"
            ),
            Self::StreamReset => write!(f, "exchange log: the cursor needs a snapshot"),
            Self::Fenced => write!(
                f,
                "exchange log: the execution fence moved; nothing was recorded"
            ),
            Self::Storage(error) => write!(f, "exchange log: {error}"),
        }
    }
}

impl std::error::Error for ExchangeError {}

impl From<rusqlite::Error> for ExchangeError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Storage(error)
    }
}

impl From<ExchangeError> for crate::persistence_service::PersistenceServiceError {
    fn from(error: ExchangeError) -> Self {
        match error {
            ExchangeError::Storage(error) => Self::Db(error),
            ExchangeError::Fenced => Self::Validation(error.to_string()),
            ExchangeError::StreamReset => Self::Retryable(error.to_string()),
            ExchangeError::InvalidRecord(_) | ExchangeError::SequenceGap { .. } => {
                Self::Runtime(error.to_string())
            }
        }
    }
}

fn producer_kind(producer: &Producer) -> &'static str {
    match producer {
        Producer::Host(_) => "host",
        Producer::Human(_) => "human",
        Producer::Agent { .. } => "agent",
        Producer::Tool(_) => "tool",
        Producer::Repository => "repository",
    }
}

fn validate(request: &AppendRequest) -> Result<(), ExchangeError> {
    let invalid = |message: &str| Err(ExchangeError::InvalidRecord(message.into()));
    let key = &request.idempotency_key;
    if key.is_empty() || key.len() > MAX_KEY_BYTES {
        return invalid("the idempotency key is empty or longer than 200 bytes");
    }
    if !key.bytes().all(|b| b.is_ascii_graphic()) {
        return invalid("the idempotency key must be printable ASCII without spaces");
    }
    if request.kind.stream_kind() != request.stream.kind {
        return invalid("the record kind does not belong to the stream kind");
    }
    let subject = &request.stream.subject_id;
    if subject.is_empty() || subject.len() > MAX_KEY_BYTES || request.stream.project_id.is_empty() {
        return invalid("the stream needs a project and a subject of at most 200 bytes");
    }
    if let Some(sha) = &request.content_sha256
        && (sha.len() != 64 || !sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
    {
        return invalid("the content digest must be 64 lowercase hex characters");
    }
    if let Some(payload) = &request.payload
        && (payload.id.is_empty() || payload.id.len() > MAX_KEY_BYTES)
    {
        return invalid("the payload id is empty or longer than 200 bytes");
    }
    Ok(())
}

fn fence_holds(db: &Connection, fence: &ExecutionFence) -> Result<bool, ExchangeError> {
    let current = db
        .query_row(
            "SELECT p.project_id,p.id,p.input_revision,j.generation,p.state_version
             FROM jobs j JOIN pulsations p ON p.id=j.pulsation_id WHERE j.id=?1",
            [&fence.job_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, u64>(2)?,
                    row.get::<_, u64>(3)?,
                    row.get::<_, u64>(4)?,
                ))
            },
        )
        .optional()?;
    Ok(
        current.is_some_and(|(project, pulse, revision, generation, state)| {
            project == fence.project_id
                && fence.pulsation_id.as_deref().is_none_or(|id| id == pulse)
                && fence
                    .input_revision
                    .is_none_or(|expected| expected == revision)
                && generation == fence.generation
                && state == fence.state_version
        }),
    )
}

/// Appends one item in the caller's transaction. Never commits.
pub fn append(db: &Connection, request: &AppendRequest) -> Result<AppendReceipt, ExchangeError> {
    validate(request)?;
    let now = chrono::Utc::now().to_rfc3339();
    // An existing stream is only read here; its one write is the sequence
    // update after the item.
    let existing = db
        .prepare_cached(
            "SELECT id,next_seq FROM exchange_streams WHERE project_id=?1 AND kind=?2 AND subject_id=?3",
        )?
        .query_row(
            params![
                request.stream.project_id,
                request.stream.kind.as_str(),
                request.stream.subject_id
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (stream_id, next_seq): (String, u64) = match existing {
        Some(stream) => stream,
        None => db
            .prepare_cached(
                "INSERT INTO exchange_streams(id,project_id,kind,subject_id,next_seq,created_at)
                 VALUES(?1,?2,?3,?4,1,?5) RETURNING id,next_seq",
            )?
            .query_row(
                params![
                    uuid::Uuid::new_v4().to_string(),
                    request.stream.project_id,
                    request.stream.kind.as_str(),
                    request.stream.subject_id,
                    now
                ],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?,
    };
    if let Some((seq, global_seq)) = db
        .prepare_cached(
            "SELECT seq,global_seq FROM exchange_items WHERE stream_id=?1 AND idempotency_key=?2",
        )?
        .query_row(params![stream_id, request.idempotency_key], |row| {
            Ok((row.get::<_, u64>(0)?, row.get::<_, i64>(1)?))
        })
        .optional()?
    {
        return Ok(AppendReceipt {
            disposition: Disposition::Duplicate,
            seq,
            next_seq,
            global_seq,
        });
    }
    if let Some(fence) = &request.fence
        && !fence_holds(db, fence)?
    {
        return Ok(AppendReceipt {
            disposition: Disposition::Fenced,
            seq: 0,
            next_seq,
            global_seq: 0,
        });
    }
    let recorded: u64 = db
        .prepare_cached("SELECT COALESCE(MAX(seq),0) FROM exchange_items WHERE stream_id=?1")?
        .query_row([&stream_id], |row| row.get(0))?;
    if recorded + 1 != next_seq {
        return Err(ExchangeError::SequenceGap {
            stream_id,
            next_seq,
            recorded,
        });
    }
    db.prepare_cached(
        "INSERT INTO exchange_items(project_id,stream_id,seq,kind,producer_kind,producer_ref,trust,
           content_sha256,payload_table,payload_id,idempotency_key,created_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
    )?
    .execute(params![
        request.stream.project_id,
        stream_id,
        next_seq,
        request.kind.as_str(),
        producer_kind(&request.producer),
        request.producer.label(),
        request.producer.trust().as_str(),
        request.content_sha256,
        request.payload.as_ref().map(|payload| payload.table),
        request.payload.as_ref().map(|payload| payload.id.as_str()),
        request.idempotency_key,
        now
    ])?;
    let global_seq = db.last_insert_rowid();
    db.prepare_cached("UPDATE exchange_streams SET next_seq=next_seq+1 WHERE id=?1")?
        .execute([&stream_id])?;
    Ok(AppendReceipt {
        disposition: Disposition::Recorded,
        seq: next_seq,
        next_seq: next_seq + 1,
        global_seq,
    })
}

/// [`append`], with a fenced receipt turned into an error.
pub(crate) fn record(
    db: &Connection,
    request: &AppendRequest,
) -> Result<AppendReceipt, ExchangeError> {
    append(db, request)?.admitted()
}

/// Records an item for a domain write that already committed: writers
/// whose write spans several transactions (a Forge or documentation turn,
/// a canvas request) record once, right before their broadcast. A crash
/// between the two loses only this item; a restarted app reads snapshots.
pub(crate) fn record_after_commit(
    db: &Connection,
    stream: StreamRef,
    kind: LogRecordKind,
    component: ikore_contracts::exchange::HostComponent,
    payload: Option<PayloadRef>,
    idempotency_key: String,
) {
    let request = AppendRequest::host(stream, kind, component, payload, idempotency_key);
    if let Err(error) = atomically(db, || record(db, &request)) {
        eprintln!("exchange log item skipped: {error}");
    }
}

/// The item of a CLI run status change, whichever crate changed it.
pub(crate) fn record_terminal_run_status(
    db: &Connection,
    project_id: &str,
    run_id: &str,
    status: &str,
) -> Result<AppendReceipt, ExchangeError> {
    record(
        db,
        &AppendRequest::host(
            StreamRef::new(project_id, StreamKind::TerminalRun, run_id),
            LogRecordKind::TerminalRunStatus,
            ikore_contracts::exchange::HostComponent::TerminalRuntime,
            Some(PayloadRef {
                table: "terminal_runs",
                id: run_id.to_owned(),
            }),
            format!("terminal_run:{run_id}:{status}"),
        ),
    )
}

/// Runs a domain write and its appends as one unit: a savepoint nests in
/// the caller's transaction, or is a transaction of its own when the
/// caller has none.
pub(crate) fn atomically<T, E: From<rusqlite::Error>>(
    db: &Connection,
    write: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    db.execute_batch("SAVEPOINT hvc_exchange_write")?;
    match write() {
        Ok(value) => {
            db.execute_batch("RELEASE hvc_exchange_write")?;
            Ok(value)
        }
        Err(error) => {
            let _ = db.execute_batch("ROLLBACK TO hvc_exchange_write; RELEASE hvc_exchange_write");
            Err(error)
        }
    }
}

/// One item as read back, with its stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogItem {
    pub global_seq: i64,
    pub project_id: String,
    pub stream_id: String,
    pub stream_kind: StreamKind,
    pub subject_id: String,
    pub seq: u64,
    /// The record kind as stored; unknown kinds read back as text.
    pub kind: String,
    pub trust: TrustClass,
    pub producer_ref: Option<String>,
    pub content_sha256: Option<String>,
    pub payload_table: Option<String>,
    pub payload_id: Option<String>,
    pub created_at: String,
}

impl LogItem {
    pub fn record_kind(&self) -> Option<LogRecordKind> {
        LogRecordKind::parse(&self.kind)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Page {
    pub items: Vec<LogItem>,
    /// The cursor after this page: the last item's `global_seq`, or the
    /// cursor that was read when the page is empty.
    pub next_cursor: i64,
    /// More items wait after this page.
    pub more: bool,
    /// The cursor cannot be served: take a snapshot and a fresh cursor.
    pub reset: bool,
}

const ITEM_COLUMNS: &str = "i.global_seq,i.project_id,i.stream_id,s.kind,s.subject_id,i.seq,i.kind,
    i.trust,i.producer_ref,i.content_sha256,i.payload_table,i.payload_id,i.created_at";

fn map_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<LogItem> {
    let stream_kind: String = row.get(3)?;
    let trust: String = row.get(7)?;
    let invalid = |column: usize, value: String| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Text,
            format!("unknown value `{value}`").into(),
        )
    };
    Ok(LogItem {
        global_seq: row.get(0)?,
        project_id: row.get(1)?,
        stream_id: row.get(2)?,
        stream_kind: StreamKind::parse(&stream_kind).ok_or_else(|| invalid(3, stream_kind))?,
        subject_id: row.get(4)?,
        seq: row.get(5)?,
        kind: row.get(6)?,
        trust: TrustClass::ALL
            .into_iter()
            .find(|class| class.as_str() == trust)
            .ok_or_else(|| invalid(7, trust))?,
        producer_ref: row.get(8)?,
        content_sha256: row.get(9)?,
        payload_table: row.get(10)?,
        payload_id: row.get(11)?,
        created_at: row.get(12)?,
    })
}

/// The project's newest `global_seq`; 0 when it has no item. A follower
/// starts here.
pub fn project_cursor(db: &Connection, project_id: &str) -> Result<i64, ExchangeError> {
    Ok(db
        .prepare_cached(
            "SELECT COALESCE(MAX(global_seq),0) FROM exchange_items WHERE project_id=?1",
        )?
        .query_row([project_id], |row| row.get(0))?)
}

/// Items of `project_id` after `after_global_seq`, oldest first.
///
/// `reset` is set, with no items, when the cursor is ahead of every item
/// ever written (a restored database) or more than
/// [`MAX_CATCH_UP_PAGES`] pages behind.
pub fn read_after(
    db: &Connection,
    project_id: &str,
    after_global_seq: i64,
    limit: usize,
) -> Result<Page, ExchangeError> {
    let limit = limit.clamp(1, PAGE_LIMIT);
    let reset = || Page {
        next_cursor: after_global_seq,
        reset: true,
        ..Page::default()
    };
    let newest: i64 = db
        .prepare_cached("SELECT COALESCE(MAX(global_seq),0) FROM exchange_items")?
        .query_row([], |row| row.get(0))?;
    if after_global_seq < 0 || after_global_seq > newest {
        return Ok(reset());
    }
    let window = (MAX_CATCH_UP_PAGES * limit) as i64;
    let pending: i64 = db
        .prepare_cached(
            "SELECT COUNT(*) FROM (SELECT 1 FROM exchange_items
               WHERE project_id=?1 AND global_seq>?2 LIMIT ?3)",
        )?
        .query_row(params![project_id, after_global_seq, window + 1], |row| {
            row.get(0)
        })?;
    if pending > window {
        return Ok(reset());
    }
    let mut statement = db.prepare_cached(&format!(
        "SELECT {ITEM_COLUMNS} FROM exchange_items i JOIN exchange_streams s ON s.id=i.stream_id
         WHERE i.project_id=?1 AND i.global_seq>?2 ORDER BY i.global_seq LIMIT ?3"
    ))?;
    let items = statement
        .query_map(
            params![project_id, after_global_seq, limit as i64],
            map_item,
        )?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Page {
        next_cursor: items
            .last()
            .map_or(after_global_seq, |item| item.global_seq),
        more: pending > items.len() as i64,
        reset: false,
        items,
    })
}

/// Items of one stream after `after_seq`, in stream order.
pub fn read_stream_after(
    db: &Connection,
    stream_id: &str,
    after_seq: u64,
    limit: usize,
) -> Result<Vec<LogItem>, ExchangeError> {
    let mut statement = db.prepare_cached(&format!(
        "SELECT {ITEM_COLUMNS} FROM exchange_items i JOIN exchange_streams s ON s.id=i.stream_id
         WHERE i.stream_id=?1 AND i.seq>?2 ORDER BY i.seq LIMIT ?3"
    ))?;
    Ok(statement
        .query_map(
            params![stream_id, after_seq, limit.clamp(1, PAGE_LIMIT) as i64],
            map_item,
        )?
        .collect::<Result<Vec<_>, _>>()?)
}

/// Every item after `after_global_seq`, across projects; the notifier's read.
pub(crate) fn read_all_after(
    db: &Connection,
    after_global_seq: i64,
    limit: usize,
) -> Result<Vec<LogItem>, ExchangeError> {
    let mut statement = db.prepare_cached(&format!(
        "SELECT {ITEM_COLUMNS} FROM exchange_items i JOIN exchange_streams s ON s.id=i.stream_id
         WHERE i.global_seq>?1 ORDER BY i.global_seq LIMIT ?2"
    ))?;
    Ok(statement
        .query_map(
            params![after_global_seq, limit.clamp(1, PAGE_LIMIT) as i64],
            map_item,
        )?
        .collect::<Result<Vec<_>, _>>()?)
}

/// The doorbell: one item was committed. `previous_global_seq` is the
/// project's item before it (0 for its first), so a follower knows whether
/// it missed anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExchangeDoorbellV1 {
    pub project_id: String,
    pub stream_id: String,
    pub stream_kind: StreamKind,
    pub subject_id: String,
    pub kind: String,
    pub seq: u64,
    pub global_seq: i64,
    pub previous_global_seq: i64,
}

/// What a follower does with a doorbell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoorbellAction {
    /// The next item: apply it; the cursor moved.
    Apply,
    /// Something was missed: read after this cursor.
    CatchUp(i64),
    /// Another project, an old item, or a read already in flight.
    Ignore,
}

/// What a follower does after a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageAction {
    /// Dispatch the page; the follower is current.
    Done,
    /// Dispatch the page and read again after this cursor.
    Continue(i64),
    /// Take a snapshot, then [`ExchangeFollower::restart`].
    Snapshot,
}

/// A subscriber's cursor over one project's log: applies doorbells in
/// order and turns a gap or a lagged broadcast into a read from the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExchangeFollower {
    project_id: String,
    cursor: i64,
    reading: bool,
    /// The newest `global_seq` a doorbell announced while a read was in
    /// flight; the read may have started before it committed.
    announced: i64,
}

impl ExchangeFollower {
    pub fn new(project_id: impl Into<String>, cursor: i64) -> Self {
        Self {
            project_id: project_id.into(),
            cursor,
            reading: false,
            announced: cursor,
        }
    }

    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    pub const fn cursor(&self) -> i64 {
        self.cursor
    }

    pub const fn reading(&self) -> bool {
        self.reading
    }

    pub fn on_doorbell(&mut self, doorbell: &ExchangeDoorbellV1) -> DoorbellAction {
        if doorbell.project_id != self.project_id || doorbell.global_seq <= self.cursor {
            return DoorbellAction::Ignore;
        }
        if self.reading {
            self.announced = self.announced.max(doorbell.global_seq);
            return DoorbellAction::Ignore;
        }
        if doorbell.previous_global_seq == self.cursor {
            self.cursor = doorbell.global_seq;
            self.announced = self.cursor;
            return DoorbellAction::Apply;
        }
        self.reading = true;
        self.announced = doorbell.global_seq;
        DoorbellAction::CatchUp(self.cursor)
    }

    /// The broadcast dropped events: read from the cursor, unless a read is
    /// already in flight.
    pub fn on_lagged(&mut self) -> Option<i64> {
        if self.reading {
            return None;
        }
        self.reading = true;
        Some(self.cursor)
    }

    pub fn on_page(&mut self, page: &Page) -> PageAction {
        if page.reset {
            return PageAction::Snapshot;
        }
        self.cursor = self.cursor.max(page.next_cursor);
        if page.more || self.announced > self.cursor {
            return PageAction::Continue(self.cursor);
        }
        self.reading = false;
        PageAction::Done
    }

    /// A read failed: drop the flag, so the next doorbell or lag retries.
    pub fn on_read_failed(&mut self) {
        self.reading = false;
    }

    /// After a snapshot: follow from the project's current cursor.
    pub fn restart(&mut self, cursor: i64) {
        self.cursor = cursor;
        self.announced = cursor;
        self.reading = false;
    }
}

/// Turns committed items into doorbells, in `global_seq` order, with each
/// project's previous item.
#[derive(Debug, Default)]
pub(crate) struct DoorbellCursor {
    last_global_seq: i64,
    last_by_project: HashMap<String, i64>,
}

impl DoorbellCursor {
    /// Starts after every item already in the database.
    pub(crate) fn starting_now(db: &Connection) -> Result<Self, ExchangeError> {
        let last_global_seq: i64 = db
            .prepare_cached("SELECT COALESCE(MAX(global_seq),0) FROM exchange_items")?
            .query_row([], |row| row.get(0))?;
        Ok(Self {
            last_global_seq,
            last_by_project: HashMap::new(),
        })
    }

    /// The doorbells for every item committed since the last call.
    pub(crate) fn poll(
        &mut self,
        db: &Connection,
    ) -> Result<Vec<ExchangeDoorbellV1>, ExchangeError> {
        let items = read_all_after(db, self.last_global_seq, PAGE_LIMIT)?;
        let mut doorbells = Vec::with_capacity(items.len());
        for item in items {
            let previous = match self.last_by_project.get(&item.project_id) {
                Some(previous) => *previous,
                None => db
                    .prepare_cached(
                        "SELECT COALESCE(MAX(global_seq),0) FROM exchange_items
                         WHERE project_id=?1 AND global_seq<?2",
                    )?
                    .query_row(params![item.project_id, item.global_seq], |row| row.get(0))?,
            };
            self.last_global_seq = item.global_seq;
            self.last_by_project
                .insert(item.project_id.clone(), item.global_seq);
            doorbells.push(ExchangeDoorbellV1 {
                project_id: item.project_id,
                stream_id: item.stream_id,
                stream_kind: item.stream_kind,
                subject_id: item.subject_id,
                kind: item.kind,
                seq: item.seq,
                global_seq: item.global_seq,
                previous_global_seq: previous,
            });
        }
        Ok(doorbells)
    }
}

/// Publishes a doorbell for every committed item, polling the log; stops
/// when `stop` is set. Polling keeps the writers free of the event bus and
/// guarantees that a doorbell never announces an uncommitted item.
pub(crate) fn spawn_notifier(
    database_path: std::path::PathBuf,
    events: std::sync::Arc<crate::events::AppEventBus>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    interval: std::time::Duration,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("hvc-exchange-doorbell".into())
        .spawn(move || {
            let open = || -> Result<Connection, rusqlite::Error> {
                let db = Connection::open(&database_path)?;
                db.busy_timeout(std::time::Duration::from_secs(5))?;
                Ok(db)
            };
            let mut db: Option<Connection> = None;
            let mut cursor: Option<DoorbellCursor> = None;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                if db.is_none() {
                    db = open().ok();
                }
                if let Some(connection) = db.as_ref() {
                    if cursor.is_none() {
                        cursor = DoorbellCursor::starting_now(connection).ok();
                    }
                    if let Some(cursor) = cursor.as_mut() {
                        match cursor.poll(connection) {
                            Ok(doorbells) => {
                                for doorbell in doorbells {
                                    events.publish(crate::events::AppEvent::Exchange(doorbell));
                                }
                            }
                            // Reopen on the next tick; the cursor keeps its
                            // place, so nothing committed meanwhile is skipped.
                            Err(_) => db = None,
                        }
                    }
                }
                std::thread::sleep(interval);
            }
        })
}

#[cfg(test)]
#[path = "log_tests.rs"]
mod tests;
