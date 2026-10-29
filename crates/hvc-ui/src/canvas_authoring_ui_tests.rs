use super::*;
use crate::AppView;
use gpui::TestAppContext;
use hvc_core::chat_service::{ChatConversation, ChatMessage, ChatTransportKind};
use hvc_core::companion_drawings::semantics::CanvasProfileV1;
use hvc_core::terminals::CliModelSelectionSource;
use std::sync::Mutex;

fn scope() -> AuthoringScope {
    AuthoringScope {
        project_id: "project-a".into(),
        conversation_id: "conversation-a".into(),
    }
}

fn request(state: CanvasAuthoringRequestStateV1) -> CanvasAuthoringRequestV1 {
    CanvasAuthoringRequestV1 {
        request_id: "request-a".into(),
        project_id: "project-a".into(),
        conversation_id: "conversation-a".into(),
        assistant_message_id: "answer-a".into(),
        source_sha256: "a".repeat(64),
        profile: CanvasProfileV1::Architecture,
        language: "en".into(),
        title: "System architecture".into(),
        request_text: "May I explain this architecture in Canvas?".into(),
        state,
        request_revision: 1,
        job_id: None,
        ready: None,
        failure: None,
        created_at: 1,
        updated_at: 1,
        source_available: true,
        drawing_available: false,
    }
}

fn ready_request() -> CanvasAuthoringRequestV1 {
    let mut row = request(CanvasAuthoringRequestStateV1::Ready);
    row.job_id = Some("job-a".into());
    row.drawing_available = true;
    row.ready = Some(CanvasAuthoringReadyReceiptV1 {
        project_id: row.project_id.clone(),
        conversation_id: row.conversation_id.clone(),
        request_id: row.request_id.clone(),
        job_id: "job-a".into(),
        drawing_id: "drawing-a".into(),
        drawing_revision: 1,
    });
    row
}

#[test]
fn canvas_offer_actions_follow_durable_state_and_availability() {
    use CanvasAuthoringRequestStateV1 as State;
    assert_eq!(
        available_actions(&request(State::AwaitingAuthorization), false),
        [
            CanvasAuthoringAction::Approve,
            CanvasAuthoringAction::Decline
        ]
    );
    let mut running = request(State::Running);
    running.job_id = Some("job-a".into());
    assert_eq!(
        available_actions(&running, false),
        [CanvasAuthoringAction::Stop]
    );
    assert_eq!(
        available_actions(&ready_request(), false),
        [CanvasAuthoringAction::Open]
    );
    for state in [
        State::Declined,
        State::Failed,
        State::Cancelled,
        State::Interrupted,
    ] {
        assert!(available_actions(&request(state), false).is_empty());
    }
    assert!(available_actions(&ready_request(), true).is_empty());
    let mut unavailable = request(State::AwaitingAuthorization);
    unavailable.source_available = false;
    assert_eq!(
        available_actions(&unavailable, false),
        [CanvasAuthoringAction::Decline]
    );
    let mut deleted = ready_request();
    deleted.drawing_available = false;
    assert!(available_actions(&deleted, false).is_empty());
}

#[test]
fn canvas_offer_loads_are_scoped_and_stale_callbacks_cannot_replace_newer_state() {
    let mut ui = CanvasAuthoringUi::default();
    let original = ui.begin_load(scope());
    let newer = ui.begin_load(scope());
    assert!(!ui.apply_load(
        &original,
        Ok(vec![request(
            CanvasAuthoringRequestStateV1::AwaitingAuthorization
        )])
    ));
    assert!(ui.apply_load(&newer, Ok(vec![ready_request()])));
    let other = AuthoringScope {
        project_id: "project-b".into(),
        ..scope()
    };
    let token = ui.begin_load(other.clone());
    assert!(ui.apply_load(&token, Ok(vec![ready_request()])));
    assert!(ui.conversations[&other].rows.is_empty());
    assert!(ui.conversations[&other].load_error.is_some());
    assert_eq!(
        ui.conversations[&scope()].rows[0].state,
        CanvasAuthoringRequestStateV1::Ready
    );
}

#[test]
fn canvas_offer_actions_invalidate_preapproval_loads_and_deduplicate_clicks() {
    let mut ui = CanvasAuthoringUi {
        active_scope: Some(scope()),
        ..CanvasAuthoringUi::default()
    };
    let load = ui.begin_load(scope());
    ui.apply_load(
        &load,
        Ok(vec![request(
            CanvasAuthoringRequestStateV1::AwaitingAuthorization,
        )]),
    );
    let stale = ui.begin_load(scope());
    let (token, _) = ui
        .begin_action(&scope(), "request-a", CanvasAuthoringAction::Approve)
        .unwrap();
    assert!(
        ui.begin_action(&scope(), "request-a", CanvasAuthoringAction::Approve)
            .is_none()
    );
    assert!(!ui.apply_load(
        &stale,
        Ok(vec![request(
            CanvasAuthoringRequestStateV1::AwaitingAuthorization
        )])
    ));
    let mut running = request(CanvasAuthoringRequestStateV1::Running);
    running.request_revision = 2;
    running.job_id = Some("job-a".into());
    assert!(ui.finish_action(&token, Ok(Some(running))));
    assert!(!ui.finish_action(&token, Ok(Some(ready_request()))));
    assert_eq!(ui.conversations[&scope()].rows[0].request_revision, 2);
}

#[test]
fn canvas_ready_receipt_must_match_the_source_request_and_job() {
    let row = ready_request();
    let receipt = row.ready.as_ref().unwrap();
    assert!(receipt_matches(&scope(), &row, receipt));
    for field in ["project", "conversation", "request", "job", "drawing"] {
        let mut forged = receipt.clone();
        match field {
            "project" => forged.project_id = "project-b".into(),
            "conversation" => forged.conversation_id = "conversation-b".into(),
            "request" => forged.request_id = "request-b".into(),
            "job" => forged.job_id = "job-b".into(),
            _ => forged.drawing_id = "drawing-b".into(),
        }
        assert!(!receipt_matches(&scope(), &row, &forged));
    }
}

type RecordedCanvasAction = (
    CanvasAuthoringAction,
    String,
    u64,
    Option<ChatExecutionTargetRequest>,
);

#[derive(Default)]
struct RecordingAuthoringBackend {
    rows: Mutex<Vec<CanvasAuthoringRequestV1>>,
    calls: Mutex<Vec<RecordedCanvasAction>>,
    resolve_error: Mutex<bool>,
    resolve_wait: Mutex<Option<tokio::sync::oneshot::Receiver<CanvasAuthoringReadyReceiptV1>>>,
}

impl CanvasAuthoringBackend for RecordingAuthoringBackend {
    fn list(&self, _: AuthoringScope) -> ChatFuture<Vec<CanvasAuthoringRequestV1>> {
        let rows = self.rows.lock().unwrap().clone();
        Box::pin(async move { Ok(rows) })
    }

    fn decide(
        &self,
        row: CanvasAuthoringRequestV1,
        action: CanvasAuthoringAction,
        expected_target: Option<ChatExecutionTargetRequest>,
    ) -> ChatFuture<CanvasAuthoringRequestV1> {
        self.calls.lock().unwrap().push((
            action,
            row.request_id.clone(),
            row.request_revision,
            expected_target,
        ));
        let mut changed = row;
        changed.request_revision += 1;
        changed.state = match action {
            CanvasAuthoringAction::Approve => CanvasAuthoringRequestStateV1::Running,
            CanvasAuthoringAction::Decline => CanvasAuthoringRequestStateV1::Declined,
            CanvasAuthoringAction::Stop => CanvasAuthoringRequestStateV1::Cancelled,
            CanvasAuthoringAction::Open => unreachable!(),
        };
        if action == CanvasAuthoringAction::Approve {
            changed.job_id = Some("job-a".into());
        }
        *self.rows.lock().unwrap() = vec![changed.clone()];
        Box::pin(async move { Ok(changed) })
    }

    fn resolve(&self, row: CanvasAuthoringRequestV1) -> ChatFuture<CanvasAuthoringReadyReceiptV1> {
        let missing = *self.resolve_error.lock().unwrap();
        let wait = self.resolve_wait.lock().unwrap().take();
        Box::pin(async move {
            if let Some(wait) = wait {
                return wait
                    .await
                    .map_err(|_| UiCommandError::worker("Resolution cancelled"));
            }
            if missing {
                Err(UiCommandError::from_chat_parts(
                    "drawing_missing",
                    "The drawing is no longer available.",
                ))
            } else {
                row.ready
                    .ok_or_else(|| UiCommandError::worker("Ready receipt unavailable"))
            }
        })
    }
}

fn install(
    model: &mut AppModel,
    backend: Arc<RecordingAuthoringBackend>,
    cx: &mut Context<AppModel>,
) {
    model.workspace.update(cx, |workspace, _| {
        workspace.project_id = Some("project-a".into())
    });
    model.chat.update(cx, |chat, _| {
        chat.project_id = Some("project-a".into());
        chat.active_conversation = Some("conversation-a".into());
        chat.selected_connection_id = Some("connection-a".into());
        chat.conversations = vec![ChatConversation {
            id: "conversation-a".into(),
            project_id: "project-a".into(),
            connection_id: Some("connection-a".into()),
            provider_id: "openai".into(),
            model_id: "model-a".into(),
            transport: ChatTransportKind::Api,
            cli_backend: None,
            model_profile: None,
            model_source: CliModelSelectionSource::Catalog,
            title: "Architecture".into(),
            connection_available: true,
            created_at: "now".into(),
            updated_at: "now".into(),
        }];
        chat.messages_by_conversation.insert(
            "conversation-a".into(),
            vec![ChatMessage {
                id: "answer-a".into(),
                conversation_id: "conversation-a".into(),
                ordinal: 1,
                role: "assistant".into(),
                content: "The UI calls the core service.".into(),
                status: "completed".into(),
                execution_id: None,
                generation_id: None,
                last_sequence: 1,
                input_tokens: None,
                output_tokens: None,
                estimated_cost_usd: None,
                error_code: None,
                error_message: None,
                target_label: None,
                created_at: "now".into(),
                updated_at: "now".into(),
            }],
        );
        chat.config_collapsed = true;
    });
    model.canvas_authoring.backend = Some(backend);
    model.refresh_canvas_authoring(cx);
}

#[gpui::test]
fn canvas_ui_approval_pins_the_selected_target_once_and_never_navigates(cx: &mut TestAppContext) {
    let backend = Arc::new(RecordingAuthoringBackend::default());
    *backend.rows.lock().unwrap() = vec![request(
        CanvasAuthoringRequestStateV1::AwaitingAuthorization,
    )];
    let model = cx.new(AppModel::new_for_test);
    model.update(cx, |model, cx| install(model, backend.clone(), cx));
    cx.run_until_parked();
    model.update(cx, |model, cx| {
        model.canvas_authoring_action("request-a".into(), CanvasAuthoringAction::Approve, cx);
        model.canvas_authoring_action("request-a".into(), CanvasAuthoringAction::Approve, cx);
        model.chat.update(cx, |chat, _| {
            chat.selected_connection_id = Some("connection-b".into());
        });
    });
    cx.run_until_parked();
    let calls = backend.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].2, 1);
    assert_eq!(
        calls[0].3,
        Some(ChatExecutionTargetRequest::Api {
            connection_id: "connection-a".into()
        })
    );
    model.update(cx, |model, _| {
        assert_eq!(model.active_view(), AppView::Home)
    });
}

#[gpui::test]
fn canvas_ui_cli_approval_waits_for_target_change_and_pins_model_and_profile(
    cx: &mut TestAppContext,
) {
    let backend = Arc::new(RecordingAuthoringBackend::default());
    *backend.rows.lock().unwrap() = vec![request(
        CanvasAuthoringRequestStateV1::AwaitingAuthorization,
    )];
    let model = cx.new(AppModel::new_for_test);
    model.update(cx, |model, cx| install(model, backend.clone(), cx));
    cx.run_until_parked();
    model.update(cx, |model, cx| {
        model.chat.update(cx, |chat, _| {
            chat.selected_transport = ChatTransportKind::Cli;
            chat.selected_cli_backend = hvc_core::terminals::CliBackend::Claude;
            chat.selected_cli_model = Some("sonnet".into());
            chat.selected_cli_model_profile = Some("high".into());
            chat.target_change_pending = true;
        });
        model.canvas_authoring_action("request-a".into(), CanvasAuthoringAction::Approve, cx);
    });
    cx.run_until_parked();
    assert!(backend.calls.lock().unwrap().is_empty());
    model.update(cx, |model, cx| {
        model
            .chat
            .update(cx, |chat, _| chat.target_change_pending = false);
        model.canvas_authoring_action("request-a".into(), CanvasAuthoringAction::Approve, cx);
        model.chat.update(cx, |chat, _| {
            chat.selected_cli_model = Some("other-model".into());
            chat.selected_cli_model_profile = None;
        });
    });
    cx.run_until_parked();
    let calls = backend.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].3,
        Some(ChatExecutionTargetRequest::Cli {
            backend: hvc_core::terminals::CliBackend::Claude,
            model: Some("sonnet".into()),
            model_profile: Some("high".into()),
        })
    );
}

#[gpui::test]
fn canvas_ui_restart_hydration_and_completion_event_keep_chat_selected(cx: &mut TestAppContext) {
    let backend = Arc::new(RecordingAuthoringBackend::default());
    *backend.rows.lock().unwrap() = vec![ready_request()];
    let model = cx.new(AppModel::new_for_test);
    model.update(cx, |model, cx| {
        model.select_view(AppView::Chat, cx);
        install(model, backend.clone(), cx);
    });
    cx.run_until_parked();
    model.update(cx, |model, cx| {
        model.handle_canvas_authoring_changed("project-a".into(), "conversation-a".into(), cx);
    });
    cx.run_until_parked();
    model.update(cx, |model, _| {
        assert_eq!(model.active_view(), AppView::Chat);
        assert_eq!(
            model.canvas_authoring.conversations[&scope()].rows[0]
                .ready
                .as_ref()
                .unwrap()
                .drawing_id,
            "drawing-a"
        );
    });
}

#[gpui::test]
fn canvas_ui_wrong_project_or_deleted_drawing_cannot_open_canvas(cx: &mut TestAppContext) {
    let backend = Arc::new(RecordingAuthoringBackend::default());
    *backend.rows.lock().unwrap() = vec![ready_request()];
    *backend.resolve_error.lock().unwrap() = true;
    let model = cx.new(AppModel::new_for_test);
    model.update(cx, |model, cx| install(model, backend.clone(), cx));
    cx.run_until_parked();
    model.update(cx, |model, cx| {
        model.canvas_authoring_action("request-a".into(), CanvasAuthoringAction::Open, cx)
    });
    cx.run_until_parked();
    model.update(cx, |model, cx| {
        assert_eq!(model.active_view(), AppView::Home);
        assert!(
            model.canvas_authoring.conversations[&scope()]
                .errors
                .contains_key("request-a")
        );
        model.workspace.update(cx, |workspace, _| {
            workspace.project_id = Some("project-b".into())
        });
        model.canvas_authoring_action("request-a".into(), CanvasAuthoringAction::Open, cx);
        assert_eq!(model.active_view(), AppView::Home);
    });
}

#[gpui::test]
fn canvas_ui_late_open_callback_cannot_navigate_after_the_project_changes(cx: &mut TestAppContext) {
    let backend = Arc::new(RecordingAuthoringBackend::default());
    let ready = ready_request();
    *backend.rows.lock().unwrap() = vec![ready.clone()];
    let (send, receive) = tokio::sync::oneshot::channel();
    *backend.resolve_wait.lock().unwrap() = Some(receive);
    let model = cx.new(AppModel::new_for_test);
    model.update(cx, |model, cx| install(model, backend, cx));
    cx.run_until_parked();
    model.update(cx, |model, cx| {
        model.canvas_authoring_action("request-a".into(), CanvasAuthoringAction::Open, cx);
    });
    cx.run_until_parked();
    model.update(cx, |model, cx| {
        model.workspace.update(cx, |workspace, _| {
            workspace.project_id = Some("project-b".into())
        });
    });
    send.send(ready.ready.unwrap()).unwrap();
    cx.run_until_parked();
    model.update(cx, |model, _| {
        assert_eq!(model.active_view(), AppView::Home)
    });
}

#[gpui::test]
fn canvas_ui_late_open_callback_respects_newer_drawing_availability(cx: &mut TestAppContext) {
    let backend = Arc::new(RecordingAuthoringBackend::default());
    let ready = ready_request();
    *backend.rows.lock().unwrap() = vec![ready.clone()];
    let (send, receive) = tokio::sync::oneshot::channel();
    *backend.resolve_wait.lock().unwrap() = Some(receive);
    let model = cx.new(AppModel::new_for_test);
    model.update(cx, |model, cx| install(model, backend.clone(), cx));
    cx.run_until_parked();
    model.update(cx, |model, cx| {
        model.canvas_authoring_action("request-a".into(), CanvasAuthoringAction::Open, cx);
    });
    cx.run_until_parked();
    let mut deleted = ready.clone();
    deleted.drawing_available = false;
    deleted.request_revision += 1;
    *backend.rows.lock().unwrap() = vec![deleted];
    model.update(cx, |model, cx| {
        model.handle_canvas_authoring_changed("project-a".into(), "conversation-a".into(), cx);
    });
    cx.run_until_parked();
    send.send(ready.ready.unwrap()).unwrap();
    cx.run_until_parked();
    model.update(cx, |model, _| {
        assert_eq!(model.active_view(), AppView::Home);
    });
}

#[gpui::test]
fn canvas_ui_late_open_callback_does_not_override_subsequent_navigation(cx: &mut TestAppContext) {
    let backend = Arc::new(RecordingAuthoringBackend::default());
    let ready = ready_request();
    *backend.rows.lock().unwrap() = vec![ready.clone()];
    let (send, receive) = tokio::sync::oneshot::channel();
    *backend.resolve_wait.lock().unwrap() = Some(receive);
    let model = cx.new(AppModel::new_for_test);
    model.update(cx, |model, cx| install(model, backend, cx));
    cx.run_until_parked();
    model.update(cx, |model, cx| {
        model.canvas_authoring_action("request-a".into(), CanvasAuthoringAction::Open, cx);
    });
    cx.run_until_parked();
    model.update(cx, |model, cx| model.select_view(AppView::Workspace, cx));
    send.send(ready.ready.unwrap()).unwrap();
    cx.run_until_parked();
    model.update(cx, |model, _| {
        assert_eq!(model.active_view(), AppView::Workspace);
    });
}

#[gpui::test]
fn canvas_ui_decline_and_stop_preserve_the_explanation(cx: &mut TestAppContext) {
    let backend = Arc::new(RecordingAuthoringBackend::default());
    *backend.rows.lock().unwrap() = vec![request(
        CanvasAuthoringRequestStateV1::AwaitingAuthorization,
    )];
    let model = cx.new(AppModel::new_for_test);
    model.update(cx, |model, cx| install(model, backend.clone(), cx));
    cx.run_until_parked();
    model.update(cx, |model, cx| {
        model.canvas_authoring_action("request-a".into(), CanvasAuthoringAction::Decline, cx);
    });
    cx.run_until_parked();
    assert_eq!(
        backend.calls.lock().unwrap()[0].0,
        CanvasAuthoringAction::Decline
    );
    let mut running = request(CanvasAuthoringRequestStateV1::Running);
    running.request_revision = 3;
    running.job_id = Some("job-a".into());
    *backend.rows.lock().unwrap() = vec![running];
    model.update(cx, |model, cx| model.refresh_canvas_authoring(cx));
    cx.run_until_parked();
    model.update(cx, |model, cx| {
        model.canvas_authoring_action("request-a".into(), CanvasAuthoringAction::Stop, cx);
    });
    cx.run_until_parked();
    assert_eq!(
        backend.calls.lock().unwrap()[1].0,
        CanvasAuthoringAction::Stop
    );
    model.update(cx, |model, cx| {
        assert_eq!(model.active_view(), AppView::Home);
        assert_eq!(
            model.chat.read(cx).messages_by_conversation["conversation-a"][0].content,
            "The UI calls the core service."
        );
    });
}

#[gpui::test]
fn canvas_native_ready_button_requires_a_click_before_navigation(cx: &mut TestAppContext) {
    let backend = Arc::new(RecordingAuthoringBackend::default());
    *backend.rows.lock().unwrap() = vec![ready_request()];
    let (model, visual) = cx.add_window_view(|_, cx| crate::layout_tests::fixture(cx));
    model.update(visual, |model, cx| {
        model.select_view(AppView::Chat, cx);
        install(model, backend, cx);
        model.canvas.project_id = Some("project-a".into());
        model.canvas.busy = true;
    });
    visual.run_until_parked();
    model.update(visual, |model, _| {
        assert_eq!(model.active_view(), AppView::Chat)
    });
    let bounds = visual
        .debug_bounds("canvas-offer-open:request-a")
        .expect("the published drawing has an explicit open button");
    visual.simulate_click(bounds.center(), gpui::Modifiers::default());
    visual.run_until_parked();
    model.update(visual, |model, _| {
        assert_eq!(model.active_view(), AppView::Canvas);
    });
}

#[gpui::test]
fn canvas_native_approve_button_starts_one_job_without_changing_the_chat_answer(
    cx: &mut TestAppContext,
) {
    let backend = Arc::new(RecordingAuthoringBackend::default());
    *backend.rows.lock().unwrap() = vec![request(
        CanvasAuthoringRequestStateV1::AwaitingAuthorization,
    )];
    let (model, visual) = cx.add_window_view(|_, cx| crate::layout_tests::fixture(cx));
    model.update(visual, |model, cx| {
        model.select_view(AppView::Chat, cx);
        install(model, backend.clone(), cx);
    });
    visual.run_until_parked();
    assert!(backend.calls.lock().unwrap().is_empty());
    let bounds = visual
        .debug_bounds("canvas-offer-approve:request-a")
        .expect("the completed answer shows its approval button");
    visual.simulate_click(bounds.center(), gpui::Modifiers::default());
    visual.run_until_parked();
    assert_eq!(backend.calls.lock().unwrap().len(), 1);
    assert!(visual.debug_bounds("canvas-offer-stop:request-a").is_some());
    model.update(visual, |model, cx| {
        assert_eq!(model.active_view(), AppView::Chat);
        assert_eq!(
            model.chat.read(cx).messages_by_conversation["conversation-a"][0].content,
            "The UI calls the core service."
        );
    });
}
