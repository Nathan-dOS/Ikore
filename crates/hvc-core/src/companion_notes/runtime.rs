//! The Notes Scribe's turn lifecycle, modelled on `FeatureAgentRuntime`: one
//! running turn per note, cancellation, a restart sweep and typed failures.
use super::contract::{
    ConversationEntryV1, NoteScribeInputV1, SCHEMA_NAME, ValidatedReply, system_prompt,
    validate_reply,
};
use super::service::{history, note, running_turn};
use super::{
    CONVERSATION_WINDOW, CaptureNoteRequest, CompanionNote, HashMap, MAX_CALLS, NoteStatus,
    NoteTurnKind, NotesEvent, RAW_INPUT_MAX, REFINE_MESSAGE_MAX, reply_schema,
};
use crate::feature_forge::service::{check_project, id, invalid, json, now, text};
use crate::feature_forge::{RequestError, TurnError, TurnFailure, TurnFailureKind};
use crate::llm::AgentRole;
use crate::persistence_service::{
    PersistenceService, PersistenceServiceError as Error, StructuredRequest,
};
use crate::secret_redactor::redact_secrets;
use crate::{AppEvent, AppEventBus};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// The routing stage name for the Scribe's calls.
const ROUTING_STAGE: &str = "Notes";

pub struct NoteScribeRuntime {
    persistence: PersistenceService,
    events: Arc<AppEventBus>,
    runtime: tokio::runtime::Handle,
    active: Mutex<HashMap<String, CancellationToken>>,
    closing: AtomicBool,
    pub(crate) gateway_override: Option<Arc<dyn crate::llm::LlmGateway>>,
}

/// A turn admitted and committed, ready to be spawned.
struct AdmittedTurn {
    project: String,
    note_id: String,
    turn_id: String,
    kind: NoteTurnKind,
}

impl NoteScribeRuntime {
    /// Builds the runtime and settles what a previous run left in flight: a
    /// running turn becomes `interrupted`, and a note still being structured
    /// becomes `failed`, keeping its text for a retry.
    pub fn new(
        persistence: PersistenceService,
        events: Arc<AppEventBus>,
        runtime: tokio::runtime::Handle,
    ) -> Result<Self, Error> {
        let restarted = "Application restarted while the Notes Scribe was answering";
        let mut db = persistence.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let at = now();
        tx.execute(
            "UPDATE companion_note_turns SET status='interrupted',failure_json=?1,updated_at=?2 WHERE status='running'",
            params![
                json(&TurnFailure::new(TurnFailureKind::Interrupted, restarted))?,
                at
            ],
        )?;
        tx.execute(
            "UPDATE companion_notes SET status='failed',updated_at=?1 WHERE status='structuring'",
            [&at],
        )?;
        tx.commit()?;
        Ok(Self {
            persistence,
            events,
            runtime,
            active: Mutex::new(HashMap::new()),
            closing: AtomicBool::new(false),
            gateway_override: None,
        })
    }

    /// Stores the developer's text as a note and starts structuring it. The
    /// note exists before the Scribe is asked anything (ADR-029 I1). A
    /// repeated `request_id` returns the note it already created.
    pub fn capture(self: &Arc<Self>, request: CaptureNoteRequest) -> Result<CompanionNote, Error> {
        let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        self.admitting()?;
        text(&request.request_id, 200)?;
        let (raw_input, redactions) = redacted(&request.raw_input, RAW_INPUT_MAX, "Your note")?;
        let mut db = self.persistence.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_project(&tx, &request.project_id)?;
        let prior: Option<(String, String)> = tx
            .query_row(
                "SELECT id,raw_input FROM companion_notes WHERE project_id=?1 AND capture_request_id=?2",
                params![request.project_id, request.request_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((note_id, stored)) = prior {
            if stored != raw_input {
                return Err(invalid("Request ID was reused with another note"));
            }
            return note(&tx, &request.project_id, &note_id);
        }
        let note_id = id();
        let at = now();
        tx.execute(
            "INSERT INTO companion_notes(id,project_id,capture_request_id,source_type,raw_input,raw_redactions,created_at,updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?7)",
            params![
                note_id,
                request.project_id,
                request.request_id,
                request.source_type.as_str(),
                raw_input,
                redactions,
                at
            ],
        )?;
        let turn_id = insert_turn(
            &tx,
            &note_id,
            1,
            &request.request_id,
            NoteTurnKind::Structure,
            None,
        )?;
        let created = note(&tx, &request.project_id, &note_id)?;
        tx.commit()?;
        self.spawn(
            &mut active,
            AdmittedTurn {
                project: request.project_id,
                note_id,
                turn_id,
                kind: NoteTurnKind::Structure,
            },
        );
        Ok(created)
    }

    /// Asks the Scribe to rewrite a structured note following `message`.
    /// Returns the turn id; a repeated `request_id` returns the same turn.
    pub fn refine(
        self: &Arc<Self>,
        project: &str,
        note_id: &str,
        expected_revision: u64,
        request_id: &str,
        message: &str,
    ) -> Result<String, Error> {
        let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        self.admitting()?;
        text(request_id, 200)?;
        let (message, redactions) = redacted(message, REFINE_MESSAGE_MAX, "Your message")?;
        let mut db = self.persistence.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = note(&tx, project, note_id)?;
        if let Some(turn) = replayed(&tx, note_id, request_id, Some(&message))? {
            return Ok(turn);
        }
        if current.status != NoteStatus::Ready {
            return Err(invalid("Only a structured note can be refined"));
        }
        admissible(&tx, &current, expected_revision)?;
        let turn_id = insert_turn(
            &tx,
            note_id,
            current.revision,
            request_id,
            NoteTurnKind::Refine,
            Some((&message, redactions)),
        )?;
        tx.commit()?;
        self.spawn(
            &mut active,
            AdmittedTurn {
                project: project.into(),
                note_id: note_id.into(),
                turn_id: turn_id.clone(),
                kind: NoteTurnKind::Refine,
            },
        );
        Ok(turn_id)
    }

    /// Structures a note whose first structuring failed, from its kept text.
    pub fn retry_structure(
        self: &Arc<Self>,
        project: &str,
        note_id: &str,
        expected_revision: u64,
        request_id: &str,
    ) -> Result<String, Error> {
        let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        self.admitting()?;
        text(request_id, 200)?;
        let mut db = self.persistence.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = note(&tx, project, note_id)?;
        if let Some(turn) = replayed(&tx, note_id, request_id, None)? {
            return Ok(turn);
        }
        if current.status != NoteStatus::Failed {
            return Err(invalid(
                "Only a note that could not be structured can be retried",
            ));
        }
        admissible(&tx, &current, expected_revision)?;
        tx.execute(
            "UPDATE companion_notes SET status='structuring',updated_at=?2 WHERE id=?1",
            params![note_id, now()],
        )?;
        let turn_id = insert_turn(
            &tx,
            note_id,
            current.revision,
            request_id,
            NoteTurnKind::Structure,
            None,
        )?;
        tx.commit()?;
        self.spawn(
            &mut active,
            AdmittedTurn {
                project: project.into(),
                note_id: note_id.into(),
                turn_id: turn_id.clone(),
                kind: NoteTurnKind::Structure,
            },
        );
        Ok(turn_id)
    }

    pub fn cancel(&self, turn_id: &str) -> bool {
        self.active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(turn_id)
            .is_some_and(|token| {
                token.cancel();
                true
            })
    }

    /// Refuses new turns and cancels the running ones (application shutdown).
    pub fn cancel_all(&self) {
        let active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        self.closing.store(true, Ordering::SeqCst);
        for token in active.values() {
            token.cancel();
        }
    }

    pub async fn wait_for_idle(&self) {
        while !self
            .active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
        {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    fn admitting(&self) -> Result<(), Error> {
        if self.closing.load(Ordering::SeqCst) {
            return Err(invalid("The Notes Scribe is shutting down"));
        }
        Ok(())
    }

    /// Admission and shutdown share the `active` lock held by the caller, so
    /// no turn can escape cancellation.
    fn spawn(
        self: &Arc<Self>,
        active: &mut HashMap<String, CancellationToken>,
        turn: AdmittedTurn,
    ) {
        let token = CancellationToken::new();
        active.insert(turn.turn_id.clone(), token.clone());
        let this = self.clone();
        self.runtime.spawn(async move {
            let calls = AtomicU32::new(0);
            let outcome = this.run(&turn, &token, &calls).await;
            if let Err(error) = outcome {
                let cancelled = token.is_cancelled();
                let mut failure = error.into_failure();
                if cancelled {
                    failure.kind = TurnFailureKind::Cancelled;
                }
                if let Err(error) = this.record_failure(&turn, cancelled, &failure, &calls) {
                    eprintln!("notes scribe failure could not be recorded: {error}");
                }
            }
            this.active
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&turn.turn_id);
            this.events.publish(AppEvent::Notes(NotesEvent {
                project_id: turn.project,
                note_id: Some(turn.note_id),
            }));
        });
    }

    async fn run(
        &self,
        turn: &AdmittedTurn,
        cancellation: &CancellationToken,
        calls: &AtomicU32,
    ) -> Result<(), TurnError> {
        let (current, instruction, conversation) = {
            let db = self.persistence.connection()?;
            let current = note(&db, &turn.project, &turn.note_id)?;
            let turns = history(&db, &turn.note_id)?;
            let instruction = turns
                .iter()
                .find(|t| t.id == turn.turn_id)
                .and_then(|t| t.user_message.clone());
            let mut conversation = turns
                .into_iter()
                .filter(|t| {
                    t.kind == NoteTurnKind::Refine
                        && t.status == "completed"
                        && t.id != turn.turn_id
                })
                .filter_map(|t| {
                    Some(ConversationEntryV1 {
                        developer: t.user_message?,
                        scribe: t.assistant_message?,
                    })
                })
                .collect::<Vec<_>>();
            if conversation.len() > CONVERSATION_WINDOW {
                conversation.drain(..conversation.len() - CONVERSATION_WINDOW);
            }
            (current, instruction, conversation)
        };
        if turn.kind == NoteTurnKind::Refine && instruction.is_none() {
            return Err(invalid("A refinement turn has no message").into());
        }
        let input = NoteScribeInputV1::new(turn.kind, &current, conversation, instruction);
        let prompt = json(&input)?;
        let mut previous_rejection: Option<String> = None;
        let validated = loop {
            let call = calls.fetch_add(1, Ordering::SeqCst) + 1;
            let last_call = call >= MAX_CALLS;
            let answer = self
                .persistence
                .execute_structured_request(
                    StructuredRequest {
                        project: &turn.project,
                        job_chain: format!("note:{}", turn.note_id),
                        role: AgentRole::NotesScribe,
                        stage: ROUTING_STAGE,
                        system_prompt: system_prompt(previous_rejection.as_deref()),
                        prompt: prompt.clone(),
                        schema_name: SCHEMA_NAME.into(),
                        schema: reply_schema(),
                        role_output_floor: true,
                        context_too_large: "The note exceeds the context budget; shorten your message or increase the configured limit",
                    },
                    cancellation,
                    self.gateway_override.as_deref(),
                )
                .await;
            let reason = match answer {
                Ok(raw) => match validate_reply(&raw, turn.kind, &current.raw_input, last_call) {
                    Ok(validated) => break validated,
                    Err(reason) => reason,
                },
                Err(RequestError::Unreadable(detail)) => detail,
                Err(error) => return Err(error.into()),
            };
            if last_call || cancellation.is_cancelled() {
                return Err(TurnFailure::new(
                    TurnFailureKind::ReplyUnreadable,
                    format!("The Notes Scribe's answer was rejected twice: {reason}"),
                )
                .into());
            }
            previous_rejection = Some(reason);
        };
        self.commit(turn, &current, validated, calls, cancellation)
    }

    /// Applies an accepted answer, only if the turn is still the running one
    /// and the note has not moved since it was read (ADR-029 I3).
    fn commit(
        &self,
        turn: &AdmittedTurn,
        read: &CompanionNote,
        validated: ValidatedReply,
        calls: &AtomicU32,
        cancellation: &CancellationToken,
    ) -> Result<(), TurnError> {
        let mut db = self.persistence.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if cancellation.is_cancelled() {
            return Err(TurnFailure::new(TurnFailureKind::Cancelled, "Note turn cancelled").into());
        }
        let current = note(&tx, &turn.project, &turn.note_id)?;
        if current.revision != read.revision {
            return Err(TurnFailure::new(
                TurnFailureKind::StaleFeature,
                "The note changed while the answer was generated",
            )
            .into());
        }
        if running_turn(&tx, &turn.note_id)?.as_deref() != Some(turn.turn_id.as_str()) {
            return Err(TurnFailure::new(
                TurnFailureKind::StaleFeature,
                "The note turn is no longer active",
            )
            .into());
        }
        let ValidatedReply {
            reply,
            tags,
            notices,
        } = validated;
        let at = now();
        tx.execute(
            "UPDATE companion_notes SET status='ready',revision=revision+1,title=?2,summary=?3,
                    content_markdown=?4,tags_json=?5,output_language=?6,updated_at=?7
             WHERE id=?1",
            params![
                turn.note_id,
                reply.note.title,
                reply.note.summary,
                reply.note.content_markdown,
                json(&tags.iter().map(|tag| tag.id()).collect::<Vec<_>>())?,
                reply.output_language,
                at
            ],
        )?;
        tx.execute(
            "UPDATE companion_note_turns SET status='completed',assistant_message=?2,response_json=?3,
                    notices_json=?4,calls=?5,updated_at=?6
             WHERE id=?1",
            params![
                turn.turn_id,
                reply.message,
                json(&reply)?,
                (!notices.is_empty()).then(|| json(&notices)).transpose()?,
                calls.load(Ordering::SeqCst),
                at
            ],
        )?;
        record_turn_end(&tx, turn, "completed")?;
        tx.commit()?;
        Ok(())
    }

    fn record_failure(
        &self,
        turn: &AdmittedTurn,
        cancelled: bool,
        failure: &TurnFailure,
        calls: &AtomicU32,
    ) -> Result<(), Error> {
        let mut db = self.persistence.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let at = now();
        tx.execute(
            "UPDATE companion_note_turns SET status=?2,failure_json=?3,calls=?4,updated_at=?5
             WHERE id=?1 AND status='running'",
            params![
                turn.turn_id,
                if cancelled { "cancelled" } else { "failed" },
                json(failure)?,
                calls.load(Ordering::SeqCst).min(MAX_CALLS),
                at
            ],
        )?;
        if turn.kind == NoteTurnKind::Structure {
            // The capture stays, with its text, for a retry (ADR-029 I1).
            tx.execute(
                "UPDATE companion_notes SET status='failed',updated_at=?2 WHERE id=?1 AND status='structuring'",
                params![turn.note_id, at],
            )?;
        }
        record_turn_end(&tx, turn, if cancelled { "cancelled" } else { "failed" })?;
        tx.commit()?;
        Ok(())
    }
}

/// ADR-037 M2: the exchange log item of a finished Scribe turn, in the
/// transaction that settles it; the broadcast follows the turn.
fn record_turn_end(db: &Connection, turn: &AdmittedTurn, outcome: &str) -> Result<(), Error> {
    use crate::exchange::log::{AppendRequest, LogRecordKind, PayloadRef, StreamKind, StreamRef};
    crate::exchange::log::record(
        db,
        &AppendRequest::host(
            StreamRef::new(&turn.project, StreamKind::Notes, &turn.note_id),
            LogRecordKind::NoteChanged,
            hvc_contracts::exchange::HostComponent::CompanionState,
            Some(PayloadRef {
                table: "companion_notes",
                id: turn.note_id.clone(),
            }),
            format!("note:{}:turn:{}:{outcome}", turn.note_id, turn.turn_id),
        ),
    )?;
    Ok(())
}

/// Redacts secrets, then checks the result: redaction can lengthen the text.
fn redacted(value: &str, limit: usize, subject: &str) -> Result<(String, u32), Error> {
    if value.trim().is_empty() {
        return Err(invalid(format!("{subject} is empty")));
    }
    let result = redact_secrets(value).map_err(|_| {
        invalid("Secret redaction is unavailable, so the text was not saved. Try again.")
    })?;
    let text = result.text.trim().to_string();
    if text.chars().count() > limit {
        return Err(invalid(format!(
            "{subject} is longer than {limit} characters"
        )));
    }
    if text.contains('\0') {
        return Err(invalid(format!("{subject} contains an invalid character")));
    }
    Ok((text, u32::try_from(result.count).unwrap_or(u32::MAX)))
}

/// The turn a repeated request already created, if the input is the same.
fn replayed(
    db: &Connection,
    note_id: &str,
    request_id: &str,
    message: Option<&str>,
) -> Result<Option<String>, Error> {
    let prior: Option<(String, Option<String>)> = db
        .query_row(
            "SELECT id,user_message FROM companion_note_turns WHERE note_id=?1 AND request_id=?2",
            params![note_id, request_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match prior {
        Some((turn, stored)) if stored.as_deref() == message => Ok(Some(turn)),
        Some(_) => Err(invalid("Request ID was reused with another message")),
        None => Ok(None),
    }
}

fn admissible(db: &Connection, current: &CompanionNote, expected: u64) -> Result<(), Error> {
    if current.revision != expected {
        return Err(invalid("The note changed; reload it and try again"));
    }
    if running_turn(db, &current.id)?.is_some() {
        return Err(invalid("The Notes Scribe is already working on this note"));
    }
    Ok(())
}

fn insert_turn(
    db: &Connection,
    note_id: &str,
    note_revision: u64,
    request_id: &str,
    kind: NoteTurnKind,
    message: Option<(&str, u32)>,
) -> Result<String, Error> {
    let turn_id = id();
    db.execute(
        "INSERT INTO companion_note_turns(id,note_id,note_revision,ordinal,request_id,kind,status,user_message,user_redactions,created_at,updated_at)
         VALUES(?1,?2,?3,(SELECT COALESCE(MAX(ordinal),0)+1 FROM companion_note_turns WHERE note_id=?2),?4,?5,'running',?6,?7,?8,?8)",
        params![
            turn_id,
            note_id,
            note_revision,
            request_id,
            kind.as_str(),
            message.map(|(text, _)| text),
            message.map_or(0, |(_, redactions)| redactions),
            now()
        ],
    )?;
    Ok(turn_id)
}
