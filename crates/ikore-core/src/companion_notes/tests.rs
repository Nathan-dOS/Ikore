use super::contract::{LANGUAGE_NOTICE, validate_reply};
use super::*;
use crate::AppEventBus;
use crate::feature_forge::TurnFailureKind;
use crate::llm::{
    AgentExecutionTarget, AgentRole, ConnectionId, GatewayFuture, LlmError, LlmErrorKind,
    LlmGateway, LlmRequest, LlmResponse, LlmUsage, ProviderId, ResponseFormat, RouteId,
};
use crate::persistence_service::{EnsureProjectForWorkspaceRequest, PersistenceService};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

struct Fixture {
    _data: tempfile::TempDir,
    _workspace: tempfile::TempDir,
    persistence: PersistenceService,
    events: Arc<AppEventBus>,
    project: String,
}

impl Fixture {
    fn new() -> Self {
        let data = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let persistence = PersistenceService::initialize(data.path()).unwrap();
        let project = persistence
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: workspace.path().to_string_lossy().into(),
                preferred_name: None,
            })
            .unwrap()
            .project
            .id;
        Self {
            _data: data,
            _workspace: workspace,
            persistence,
            events: Arc::new(AppEventBus::default()),
            project,
        }
    }

    fn runtime(&self, gateway: Arc<dyn LlmGateway>) -> Arc<NoteScribeRuntime> {
        let mut runtime = NoteScribeRuntime::new(
            self.persistence.clone(),
            self.events.clone(),
            tokio::runtime::Handle::current(),
        )
        .unwrap();
        runtime.gateway_override = Some(gateway);
        Arc::new(runtime)
    }

    fn notes(&self) -> CompanionNotesService {
        CompanionNotesService::new(self.persistence.clone())
    }

    fn capture(&self, runtime: &Arc<NoteScribeRuntime>, raw: &str) -> CompanionNote {
        runtime
            .capture(CaptureNoteRequest {
                project_id: self.project.clone(),
                request_id: uuid::Uuid::new_v4().to_string(),
                raw_input: raw.into(),
                source_type: NoteSource::Text,
            })
            .unwrap()
    }

    fn note(&self, id: &str) -> CompanionNote {
        self.notes().get(&self.project, id).unwrap()
    }

    fn turns(&self, id: &str) -> Vec<NoteTurn> {
        self.notes().history(&self.project, id).unwrap()
    }

    /// The note's exchange log keys, in order (ADR-037 M2).
    fn exchange_keys(&self, id: &str) -> Vec<String> {
        self.persistence
            .connection()
            .unwrap()
            .prepare(
                "SELECT i.idempotency_key FROM exchange_items i JOIN exchange_streams s ON s.id=i.stream_id
                 WHERE s.kind='notes' AND s.subject_id=?1 ORDER BY i.seq",
            )
            .unwrap()
            .query_map([id], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }
}

fn reply(language: &str, title: &str, tags: &[&str]) -> String {
    json!({
        "outputLanguage": language,
        "message": "Wrote the note.",
        "note": {
            "title": title,
            "summary": "One line that states the point.",
            "contentMarkdown": "- Key point\n- Expected impact",
            "tags": tags,
        }
    })
    .to_string()
}

fn response(request: &LlmRequest, content: String) -> LlmResponse {
    LlmResponse {
        execution_id: request.execution_id.clone(),
        generation_id: request.generation_id.clone(),
        route_id: RouteId::new("fixture"),
        connection_id: ConnectionId::new("fixture"),
        provider_id: ProviderId::new("fixture"),
        model: "fixture".into(),
        request_payload: json!({}),
        response_payload: json!({}),
        content,
        usage: LlmUsage::default(),
        finish_reason: None,
        estimated_cost_usd: None,
    }
}

/// Answers with the next scripted reply (the last one repeats) and keeps
/// every request it was sent.
struct ScriptedGateway {
    replies: Mutex<VecDeque<String>>,
    requests: Mutex<Vec<LlmRequest>>,
}

impl ScriptedGateway {
    fn new(replies: Vec<String>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.into()),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn requests(&self) -> Vec<LlmRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl LlmGateway for ScriptedGateway {
    fn execute<'a>(
        &'a self,
        request: &'a LlmRequest,
        _cancellation: &'a CancellationToken,
    ) -> GatewayFuture<'a> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(request.clone());
            let content = {
                let mut replies = self.replies.lock().unwrap();
                if replies.len() > 1 {
                    replies.pop_front().unwrap()
                } else {
                    replies.front().cloned().unwrap_or_default()
                }
            };
            Ok(response(request, content))
        })
    }
}

/// Holds every request until it is released or cancelled.
struct GatedGateway {
    release: tokio::sync::Notify,
    reply: String,
}

impl GatedGateway {
    fn new(reply: String) -> Arc<Self> {
        Arc::new(Self {
            release: tokio::sync::Notify::new(),
            reply,
        })
    }
}

impl LlmGateway for GatedGateway {
    fn execute<'a>(
        &'a self,
        request: &'a LlmRequest,
        cancellation: &'a CancellationToken,
    ) -> GatewayFuture<'a> {
        Box::pin(async move {
            tokio::select! {
                _ = self.release.notified() => Ok(response(request, self.reply.clone())),
                _ = cancellation.cancelled() => {
                    Err(LlmError::new(LlmErrorKind::Cancelled, "cancelled by the developer"))
                }
            }
        })
    }
}

struct FailingGateway(LlmErrorKind);

impl LlmGateway for FailingGateway {
    fn execute<'a>(
        &'a self,
        _request: &'a LlmRequest,
        _cancellation: &'a CancellationToken,
    ) -> GatewayFuture<'a> {
        Box::pin(async move {
            Err(LlmError::new(self.0, "the provider refused the request")
                .with_target("fixture", "fixture", "fixture"))
        })
    }
}

/// Waits until the gated request has reached the gateway.
async fn until_running(fixture: &Fixture, note_id: &str) -> String {
    for _ in 0..200 {
        if let Some(turn) = fixture
            .notes()
            .snapshot(&fixture.project)
            .unwrap()
            .running
            .get(note_id)
        {
            return turn.clone();
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the turn never ran");
}

const PORTUGUESE: &str = "O bot\u{e3}o de salvar some no modo escuro, precisa de mais contraste.";

#[tokio::test]
async fn a_capture_is_stored_before_the_scribe_answers() {
    let f = Fixture::new();
    let gateway = GatedGateway::new(reply("pt", "Contraste do salvar", &["ui_ux"]));
    let runtime = f.runtime(gateway.clone());
    let mut events = f.events.subscribe();

    let captured = f.capture(&runtime, PORTUGUESE);
    assert_eq!(captured.status, NoteStatus::Structuring);
    assert_eq!(captured.raw_input, PORTUGUESE);
    assert_eq!(captured.display_title(), PORTUGUESE);
    let turn = until_running(&f, &captured.id).await;
    let stored = f.note(&captured.id);
    assert_eq!(
        (stored.status, stored.revision),
        (NoteStatus::Structuring, 1)
    );

    gateway.release.notify_one();
    runtime.wait_for_idle().await;
    let note = f.note(&captured.id);
    assert_eq!(note.status, NoteStatus::Ready);
    assert_eq!(note.revision, 2);
    assert_eq!(note.title.as_deref(), Some("Contraste do salvar"));
    assert_eq!(note.tags, vec![NoteTag::UiUx]);
    assert_eq!(note.output_language.as_deref(), Some("pt"));
    let turns = f.turns(&captured.id);
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].id, turn);
    assert_eq!(turns[0].status, "completed");
    assert_eq!(turns[0].calls, 1);
    assert_eq!(
        turns[0].assistant_message.as_deref(),
        Some("Wrote the note.")
    );
    let crate::AppEvent::Notes(event) = events.recv().await.unwrap() else {
        panic!("a notes event");
    };
    assert_eq!(event.note_id.as_deref(), Some(captured.id.as_str()));
}

#[tokio::test]
async fn requests_carry_the_scribe_role_stage_chain_and_contract() {
    let f = Fixture::new();
    let gateway = ScriptedGateway::new(vec![reply("en", "Faster search", &["performance"])]);
    let runtime = f.runtime(gateway.clone());
    let note = f.capture(&runtime, "Search feels slow on large projects");
    runtime.wait_for_idle().await;

    let requests = gateway.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(
        request.target,
        AgentExecutionTarget::canonical(AgentRole::NotesScribe)
    );
    assert_eq!(request.stage, "Notes");
    assert_eq!(request.job_chain, format!("note:{}", note.id));
    assert!(request.pulsation_id.is_none());
    assert!(request.max_output_tokens >= 2_400);
    assert!(matches!(
        &request.response_format,
        ResponseFormat::JsonSchema { name, .. } if name == "note_scribe_reply_v1"
    ));
    assert!(
        request
            .system_prompt
            .starts_with("You are the Notes Scribe")
    );
    let input: Value = serde_json::from_str(&request.user_prompt).unwrap();
    assert_eq!(input["mode"], "structure");
    assert_eq!(input["rawInput"], "Search feels slow on large projects");
    assert!(input["currentNote"].is_null());
    assert_eq!(
        input["tagCatalog"].as_array().unwrap().len(),
        NoteTag::ALL.len()
    );
}

#[tokio::test]
async fn a_provider_failure_keeps_the_capture_for_a_retry() {
    let f = Fixture::new();
    let failing = f.runtime(Arc::new(FailingGateway(LlmErrorKind::Authentication)));
    let captured = f.capture(&failing, PORTUGUESE);
    failing.wait_for_idle().await;

    let note = f.note(&captured.id);
    assert_eq!(note.status, NoteStatus::Failed);
    assert_eq!(note.raw_input, PORTUGUESE);
    let turns = f.turns(&captured.id);
    assert_eq!(turns[0].status, "failed");
    let failure = turns[0].failure.as_ref().unwrap();
    assert_eq!(failure.kind, TurnFailureKind::ProviderSetup);
    assert!(
        explain_note_failure(failure)
            .remedy
            .contains("Notes Scribe")
    );

    let working = f.runtime(ScriptedGateway::new(vec![reply(
        "pt",
        "Contraste do salvar",
        &["ui_ux"],
    )]));
    working
        .retry_structure(&f.project, &captured.id, note.revision, "retry-1")
        .unwrap();
    working.wait_for_idle().await;
    let note = f.note(&captured.id);
    assert_eq!(note.status, NoteStatus::Ready);
    let turns = f.turns(&captured.id);
    assert_eq!(turns.len(), 2);
    // ADR-037 M2: each finished turn is one item of the note's stream.
    let mut outcomes: Vec<String> = turns
        .iter()
        .map(|turn| format!("note:{}:turn:{}:{}", captured.id, turn.id, turn.status))
        .collect();
    outcomes.sort();
    let mut keys = f.exchange_keys(&captured.id);
    keys.sort();
    assert_eq!(keys, outcomes);
    let refused = working
        .retry_structure(&f.project, &captured.id, note.revision, "retry-2")
        .unwrap_err();
    assert!(refused.to_string().contains("could not be structured"));
}

#[tokio::test]
async fn a_rejected_answer_gets_one_corrective_call_carrying_the_reason() {
    let f = Fixture::new();
    let gateway = ScriptedGateway::new(vec![
        reply("en", &"Long title ".repeat(10), &["bug"]),
        reply("en", "Short title", &["bug"]),
    ]);
    let runtime = f.runtime(gateway.clone());
    let captured = f.capture(&runtime, "The save button crashes the app");
    runtime.wait_for_idle().await;

    let requests = gateway.requests();
    assert_eq!(requests.len(), 2);
    assert!(!requests[0].system_prompt.contains("rejected"));
    assert!(
        requests[1]
            .system_prompt
            .contains("Your previous answer was rejected: title has")
    );
    assert_eq!(f.note(&captured.id).title.as_deref(), Some("Short title"));
    assert_eq!(f.turns(&captured.id)[0].calls, 2);
}

#[tokio::test]
async fn a_second_rejection_fails_the_turn_and_keeps_the_capture() {
    let f = Fixture::new();
    let gateway = ScriptedGateway::new(vec![reply("en", "Title", &["bug", "other"])]);
    let runtime = f.runtime(gateway.clone());
    let captured = f.capture(&runtime, "The save button crashes the app");
    runtime.wait_for_idle().await;

    assert_eq!(gateway.requests().len(), 2);
    assert_eq!(f.note(&captured.id).status, NoteStatus::Failed);
    let turn = &f.turns(&captured.id)[0];
    assert_eq!(turn.calls, 2);
    let failure = turn.failure.as_ref().unwrap();
    assert_eq!(failure.kind, TurnFailureKind::ReplyUnreadable);
    assert!(failure.technical.contains("rejected twice"));
}

#[tokio::test]
async fn an_english_answer_to_another_language_is_corrected_then_accepted_with_a_notice() {
    let f = Fixture::new();
    let corrected = ScriptedGateway::new(vec![
        reply("en", "Save contrast", &["ui_ux"]),
        reply("pt", "Contraste do salvar", &["ui_ux"]),
    ]);
    let runtime = f.runtime(corrected.clone());
    let first = f.capture(&runtime, PORTUGUESE);
    runtime.wait_for_idle().await;
    assert_eq!(corrected.requests().len(), 2);
    assert!(
        corrected.requests()[1]
            .system_prompt
            .contains("language of rawInput")
    );
    assert_eq!(f.note(&first.id).output_language.as_deref(), Some("pt"));
    assert!(f.turns(&first.id)[0].notices.is_empty());

    let stubborn = f.runtime(ScriptedGateway::new(vec![reply(
        "en",
        "Save contrast",
        &["ui_ux"],
    )]));
    let second = f.capture(&stubborn, PORTUGUESE);
    stubborn.wait_for_idle().await;
    let note = f.note(&second.id);
    assert_eq!(
        note.status,
        NoteStatus::Ready,
        "a note is never lost to the guard"
    );
    assert_eq!(
        f.turns(&second.id)[0].notices,
        vec![LANGUAGE_NOTICE.to_string()]
    );
}

#[tokio::test]
async fn refinement_rewrites_only_this_note_with_a_bounded_conversation() {
    let f = Fixture::new();
    let gateway = ScriptedGateway::new(vec![reply("en", "Note", &["workflow"])]);
    let runtime = f.runtime(gateway.clone());
    let target = f.capture(&runtime, "Switching projects loses my open tabs");
    let other = f.capture(&runtime, "Search feels slow on large projects");
    runtime.wait_for_idle().await;
    let other_before = f.note(&other.id);

    for round in 1..=8 {
        let revision = f.note(&target.id).revision;
        runtime
            .refine(
                &f.project,
                &target.id,
                revision,
                &format!("refine-{round}"),
                &format!("Instruction {round}"),
            )
            .unwrap();
        runtime.wait_for_idle().await;
    }

    let last = gateway.requests().pop().unwrap();
    let input: Value = serde_json::from_str(&last.user_prompt).unwrap();
    assert_eq!(input["mode"], "refine");
    assert_eq!(input["instruction"], "Instruction 8");
    assert_eq!(input["currentNote"]["title"], "Note");
    let conversation = input["conversation"].as_array().unwrap();
    assert_eq!(conversation.len(), CONVERSATION_WINDOW);
    assert_eq!(conversation[0]["developer"], "Instruction 2");
    assert_eq!(conversation[5]["developer"], "Instruction 7");
    assert_eq!(f.note(&target.id).revision, 10);
    assert_eq!(f.note(&other.id), other_before);
}

#[tokio::test]
async fn refinement_is_refused_for_a_stale_busy_or_unstructured_note() {
    let f = Fixture::new();
    let gateway = GatedGateway::new(reply("en", "Note", &["workflow"]));
    let runtime = f.runtime(gateway.clone());
    let captured = f.capture(&runtime, "Switching projects loses my open tabs");
    until_running(&f, &captured.id).await;
    let unstructured = runtime
        .refine(&f.project, &captured.id, 1, "early", "Shorter")
        .unwrap_err();
    assert!(unstructured.to_string().contains("structured note"));
    gateway.release.notify_one();
    runtime.wait_for_idle().await;

    let stale = runtime
        .refine(&f.project, &captured.id, 1, "stale", "Shorter")
        .unwrap_err();
    assert!(stale.to_string().contains("changed"));
    runtime
        .refine(&f.project, &captured.id, 2, "first", "Shorter")
        .unwrap();
    until_running(&f, &captured.id).await;
    let busy = runtime
        .refine(&f.project, &captured.id, 2, "second", "Longer")
        .unwrap_err();
    assert!(busy.to_string().contains("already working"));
    gateway.release.notify_one();
    runtime.wait_for_idle().await;
}

#[tokio::test]
async fn repeated_requests_replay_instead_of_duplicating() {
    let f = Fixture::new();
    let runtime = f.runtime(ScriptedGateway::new(vec![reply("en", "Note", &["docs"])]));
    let request = CaptureNoteRequest {
        project_id: f.project.clone(),
        request_id: "capture-once".into(),
        raw_input: "Document the shortcuts".into(),
        source_type: NoteSource::Text,
    };
    let first = runtime.capture(request.clone()).unwrap();
    runtime.wait_for_idle().await;
    let again = runtime.capture(request.clone()).unwrap();
    assert_eq!(again.id, first.id);
    assert_eq!(f.notes().snapshot(&f.project).unwrap().notes.len(), 1);
    let reused = runtime
        .capture(CaptureNoteRequest {
            raw_input: "Something else".into(),
            ..request
        })
        .unwrap_err();
    assert!(reused.to_string().contains("reused"));

    let turn = runtime
        .refine(&f.project, &first.id, 2, "refine-once", "Shorter")
        .unwrap();
    runtime.wait_for_idle().await;
    assert_eq!(
        runtime
            .refine(&f.project, &first.id, 3, "refine-once", "Shorter")
            .unwrap(),
        turn
    );
    assert_eq!(f.turns(&first.id).len(), 2);
}

#[tokio::test]
async fn cancelling_marks_the_turn_cancelled_and_keeps_the_capture() {
    let f = Fixture::new();
    let gateway = GatedGateway::new(reply("en", "Note", &["bug"]));
    let runtime = f.runtime(gateway);
    let captured = f.capture(&runtime, "The save button crashes the app");
    let turn = until_running(&f, &captured.id).await;
    assert!(runtime.cancel(&turn));
    runtime.wait_for_idle().await;

    assert_eq!(f.note(&captured.id).status, NoteStatus::Failed);
    let turns = f.turns(&captured.id);
    assert_eq!(turns[0].status, "cancelled");
    assert_eq!(
        turns[0].failure.as_ref().map(|failure| failure.kind),
        Some(TurnFailureKind::Cancelled)
    );
    assert!(
        !runtime.cancel(&turn),
        "a finished turn has nothing to cancel"
    );
}

#[tokio::test]
async fn shutdown_cancels_running_turns_and_refuses_new_ones() {
    let f = Fixture::new();
    let runtime = f.runtime(GatedGateway::new(reply("en", "Note", &["bug"])));
    let captured = f.capture(&runtime, "The save button crashes the app");
    until_running(&f, &captured.id).await;
    runtime.cancel_all();
    runtime.wait_for_idle().await;
    assert_eq!(f.turns(&captured.id)[0].status, "cancelled");
    let refused = runtime
        .capture(CaptureNoteRequest {
            project_id: f.project.clone(),
            request_id: "late".into(),
            raw_input: "Too late".into(),
            source_type: NoteSource::Text,
        })
        .unwrap_err();
    assert!(refused.to_string().contains("shutting down"));
}

#[tokio::test]
async fn a_restart_interrupts_running_turns_and_fails_unstructured_notes() {
    let f = Fixture::new();
    let db = f.persistence.connection().unwrap();
    db.execute("INSERT INTO companion_notes(id,project_id,capture_request_id,raw_input,created_at,updated_at) VALUES('n','p-placeholder','c','Kept text','now','now')", [])
        .expect_err("the project must exist");
    db.execute("INSERT INTO companion_notes(id,project_id,capture_request_id,raw_input,created_at,updated_at) VALUES('n',?1,'c','Kept text','now','now')", [&f.project])
        .unwrap();
    db.execute("INSERT INTO companion_note_turns(id,note_id,note_revision,ordinal,request_id,kind,status,created_at,updated_at) VALUES('t','n',1,1,'c','structure','running','now','now')", [])
        .unwrap();
    drop(db);

    let _runtime = f.runtime(ScriptedGateway::new(vec![]));
    let note = f.note("n");
    assert_eq!(note.status, NoteStatus::Failed);
    assert_eq!(note.raw_input, "Kept text");
    let turn = &f.turns("n")[0];
    assert_eq!(turn.status, "interrupted");
    assert_eq!(
        turn.failure.as_ref().map(|failure| failure.kind),
        Some(TurnFailureKind::Interrupted)
    );
}

#[tokio::test]
async fn secrets_are_redacted_before_storage_and_before_the_prompt() {
    let f = Fixture::new();
    let gateway = ScriptedGateway::new(vec![reply("en", "Token in logs", &["bug"])]);
    let runtime = f.runtime(gateway.clone());
    let captured = f.capture(
        &runtime,
        "The log prints Authorization: Bearer abcdefghijklmnop on every request",
    );
    runtime.wait_for_idle().await;
    assert!(!captured.raw_input.contains("abcdefghijklmnop"));
    assert_eq!(captured.raw_redactions, 1);
    runtime
        .refine(
            &f.project,
            &captured.id,
            2,
            "refine",
            "Also OPENAI_API_KEY=fixture-secret-value shows up",
        )
        .unwrap();
    runtime.wait_for_idle().await;
    for request in gateway.requests() {
        assert!(!request.user_prompt.contains("abcdefghijklmnop"));
        assert!(!request.user_prompt.contains("fixture-secret-value"));
    }
    let refine = &f.turns(&captured.id)[1];
    assert_eq!(refine.user_redactions, 1);
    assert!(
        !refine
            .user_message
            .as_deref()
            .unwrap()
            .contains("fixture-secret-value")
    );
}

#[tokio::test]
async fn a_capture_over_the_context_budget_fails_as_too_large() {
    let f = Fixture::new();
    f.persistence
        .connection()
        .unwrap()
        .execute(
            "INSERT INTO runtime_settings(key,value,updated_at) VALUES('runtime.context_chars_limit','2000','now')
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [],
        )
        .unwrap();
    let gateway = ScriptedGateway::new(vec![reply("en", "Note", &["docs"])]);
    let runtime = f.runtime(gateway.clone());
    let captured = f.capture(&runtime, &"A long thought. ".repeat(200));
    runtime.wait_for_idle().await;
    assert!(gateway.requests().is_empty());
    assert_eq!(f.note(&captured.id).status, NoteStatus::Failed);
    assert_eq!(
        f.turns(&captured.id)[0]
            .failure
            .as_ref()
            .map(|failure| failure.kind),
        Some(TurnFailureKind::ContextTooLarge)
    );
}

#[tokio::test]
async fn delete_waits_for_the_scribe_and_removes_the_turns() {
    let f = Fixture::new();
    let gateway = GatedGateway::new(reply("en", "Note", &["bug"]));
    let runtime = f.runtime(gateway.clone());
    let captured = f.capture(&runtime, "The save button crashes the app");
    until_running(&f, &captured.id).await;
    let busy = f.notes().delete(&f.project, &captured.id, 1).unwrap_err();
    assert!(busy.to_string().contains("cancel it"));
    gateway.release.notify_one();
    runtime.wait_for_idle().await;
    f.notes()
        .delete(&f.project, &captured.id, 1)
        .expect_err("a stale revision is refused");
    f.notes().delete(&f.project, &captured.id, 2).unwrap();
    assert!(f.notes().get(&f.project, &captured.id).is_err());
    let turns: i64 = f
        .persistence
        .connection()
        .unwrap()
        .query_row("SELECT count(*) FROM companion_note_turns", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(turns, 0);
}

#[test]
fn the_validator_enforces_every_rule_the_prompt_states() {
    let good = || {
        json!({
            "outputLanguage": "en",
            "message": "Done.",
            "note": {
                "title": "Save button in dark mode",
                "summary": "The button disappears against the dark background.",
                "contentMarkdown": "### Problem\n- Low contrast\n- Fix bug #12",
                "tags": ["ui_ux", "bug"],
            }
        })
    };
    let check = |mutate: &dyn Fn(&mut Value)| {
        let mut value = good();
        mutate(&mut value);
        validate_reply(&value.to_string(), NoteTurnKind::Refine, "", false)
    };
    let accepted = check(&|_| {}).unwrap();
    assert_eq!(accepted.tags, vec![NoteTag::UiUx, NoteTag::Bug]);
    assert!(accepted.notices.is_empty());
    check(&|v| v["note"]["title"] = json!("  Padded  ")).unwrap();

    type Mutation = Box<dyn Fn(&mut Value)>;
    let rejected: Vec<(&str, Mutation)> = vec![
        (
            "title has 81",
            Box::new(|v| v["note"]["title"] = json!("x".repeat(81))),
        ),
        (
            "title is empty",
            Box::new(|v| v["note"]["title"] = json!("   ")),
        ),
        (
            "one line",
            Box::new(|v| v["note"]["title"] = json!("Two\nlines")),
        ),
        (
            "HTML",
            Box::new(|v| v["note"]["title"] = json!("<b>Bold</b>")),
        ),
        (
            "a heading",
            Box::new(|v| v["note"]["title"] = json!("# Title")),
        ),
        (
            "emphasis",
            Box::new(|v| v["note"]["title"] = json!("A **bold** title")),
        ),
        (
            "code",
            Box::new(|v| v["note"]["summary"] = json!("Run `cargo test`")),
        ),
        (
            "a link",
            Box::new(|v| v["note"]["summary"] = json!("See [docs](https://x.test)")),
        ),
        (
            "summary has 281",
            Box::new(|v| v["note"]["summary"] = json!("y".repeat(281))),
        ),
        (
            "contentMarkdown has 4001",
            Box::new(|v| v["note"]["contentMarkdown"] = json!("z".repeat(4001))),
        ),
        (
            "contains HTML",
            Box::new(|v| v["note"]["contentMarkdown"] = json!("- <script>x</script>")),
        ),
        (
            "an image",
            Box::new(|v| v["note"]["contentMarkdown"] = json!("![shot](a.png)")),
        ),
        (
            "a code block",
            Box::new(|v| v["note"]["contentMarkdown"] = json!("```rust\nfn x(){}\n```")),
        ),
        (
            "a table",
            Box::new(|v| v["note"]["contentMarkdown"] = json!("| a | b |\n|---|---|")),
        ),
        (
            "deeper than ###",
            Box::new(|v| v["note"]["contentMarkdown"] = json!("#### Deep")),
        ),
        (
            "message has 601",
            Box::new(|v| v["message"] = json!("m".repeat(601))),
        ),
        (
            "ISO 639-1",
            Box::new(|v| v["outputLanguage"] = json!("Portuguese")),
        ),
        ("ISO 639-1", Box::new(|v| v["outputLanguage"] = json!("PT"))),
        ("give 1 to 3", Box::new(|v| v["note"]["tags"] = json!([]))),
        (
            "give 1 to 3",
            Box::new(|v| v["note"]["tags"] = json!(["bug", "docs", "feature", "workflow"])),
        ),
        (
            "not an id",
            Box::new(|v| v["note"]["tags"] = json!(["urgent"])),
        ),
        (
            "appears twice",
            Box::new(|v| v["note"]["tags"] = json!(["bug", "bug"])),
        ),
        (
            "used alone",
            Box::new(|v| v["note"]["tags"] = json!(["other", "bug"])),
        ),
        (
            "does not match",
            Box::new(|v| v["note"]["extra"] = json!("field")),
        ),
        ("does not match", Box::new(|v| v["confidence"] = json!(1))),
    ];
    for (expected, mutate) in rejected {
        let reason = check(mutate.as_ref()).unwrap_err();
        assert!(reason.contains(expected), "{expected:?} not in {reason:?}");
    }
    check(&|v| v["outputLanguage"] = json!("pt-BR")).unwrap();
    check(&|v| v["note"]["tags"] = json!(["other"])).unwrap();
}

#[test]
fn the_prompt_states_every_limit_the_validator_checks() {
    let prompt = system_prompt(None);
    for rule in [
        format!("at most {TITLE_MAX} characters"),
        format!("at most {SUMMARY_MAX} characters"),
        format!("at most {CONTENT_MAX} characters"),
        format!("at most {MESSAGE_MAX} characters"),
        format!("1 to {TAGS_MAX} distinct ids"),
        "\"other\" only alone".into(),
        "no HTML, images, tables or code blocks".into(),
        "no deeper than \"###\"".into(),
        "without Markdown or HTML".into(),
        "ISO 639-1".into(),
        "language of rawInput".into(),
        "note_scribe_reply_v1".into(),
    ] {
        assert!(prompt.contains(&rule), "the prompt does not state {rule:?}");
    }
    assert!(
        system_prompt(Some("title has 90 characters; the limit is 80."))
            .ends_with("Correct exactly that and return the complete answer again.")
    );
    let schema = reply_schema();
    assert_eq!(schema["additionalProperties"], false);
    let mut required = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|key| key.as_str().unwrap())
        .collect::<Vec<_>>();
    required.sort_unstable();
    assert_eq!(required, ["message", "note", "outputLanguage"]);
    assert_eq!(
        schema["properties"]["note"]["properties"]["tags"]["items"]["enum"],
        json!(NoteTag::ALL.map(NoteTag::id))
    );
}

#[test]
fn the_language_check_is_advisory_on_the_last_call_only() {
    let english = reply("en", "Save contrast", &["ui_ux"]);
    let reason = validate_reply(&english, NoteTurnKind::Structure, PORTUGUESE, false).unwrap_err();
    assert!(reason.contains("language of rawInput"));
    let accepted = validate_reply(&english, NoteTurnKind::Structure, PORTUGUESE, true).unwrap();
    assert_eq!(accepted.notices, vec![LANGUAGE_NOTICE.to_string()]);
    // English input, or a refinement that may have asked for English, is not judged.
    validate_reply(
        &english,
        NoteTurnKind::Structure,
        "Plain English text",
        false,
    )
    .unwrap();
    validate_reply(&english, NoteTurnKind::Refine, PORTUGUESE, false).unwrap();
}

#[test]
fn tags_have_stable_ids_and_english_labels() {
    for tag in NoteTag::ALL {
        assert_eq!(NoteTag::parse(tag.id()), Some(tag));
        assert_eq!(
            serde_json::to_value(tag).unwrap(),
            json!(tag.id()),
            "the stored id and the serde name agree"
        );
        crate::agent_output_language::validate_english_text("tag label", tag.label()).unwrap();
        crate::agent_output_language::validate_english_text("tag meaning", tag.meaning()).unwrap();
    }
}
