//! Consent and durable Canvas job cards attached to completed Companion answers.
//! Loading never navigates, edits chat content, or exposes staged model output.
use crate::{
    AppModel,
    chat::ChatFuture,
    errors::UiCommandError,
    theme,
    widgets::{ButtonVariant, button_frame},
};
use gpui::{AnyElement, Context, IntoElement, SharedString, Task, div, prelude::*};
use ikore_core::chat_service::canvas_authoring::{
    ApproveCanvasRequestV1, CanvasAuthoringReadyReceiptV1, CanvasAuthoringRequestStateV1,
    CanvasAuthoringRequestV1, DecideCanvasRequestV1, GetCanvasRequestV1, ListCanvasRequestsV1,
};
use ikore_core::{AppCore, chat_service::ChatExecutionTargetRequest};
use std::{collections::BTreeMap, sync::Arc};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct AuthoringScope {
    pub project_id: String,
    pub conversation_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CanvasAuthoringAction {
    Approve,
    Decline,
    Stop,
    Open,
}

pub(crate) trait CanvasAuthoringBackend: Send + Sync {
    fn list(&self, scope: AuthoringScope) -> ChatFuture<Vec<CanvasAuthoringRequestV1>>;
    fn decide(
        &self,
        row: CanvasAuthoringRequestV1,
        action: CanvasAuthoringAction,
        expected_target: Option<ChatExecutionTargetRequest>,
    ) -> ChatFuture<CanvasAuthoringRequestV1>;
    fn resolve(&self, row: CanvasAuthoringRequestV1) -> ChatFuture<CanvasAuthoringReadyReceiptV1>;
}

pub(crate) struct CoreCanvasAuthoringBackend {
    core: Arc<AppCore>,
}

impl CoreCanvasAuthoringBackend {
    pub fn new(core: Arc<AppCore>) -> Self {
        Self { core }
    }
}

impl CanvasAuthoringBackend for CoreCanvasAuthoringBackend {
    fn list(&self, scope: AuthoringScope) -> ChatFuture<Vec<CanvasAuthoringRequestV1>> {
        let core = self.core.clone();
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    core.chat()
                        .list_canvas_requests(ListCanvasRequestsV1 {
                            project_id: scope.project_id,
                            conversation_id: scope.conversation_id,
                        })
                        .map_err(UiCommandError::from)
                })
                .await
                .map_err(|_| UiCommandError::worker("Canvas request loading was interrupted."))?
        })
    }

    fn decide(
        &self,
        row: CanvasAuthoringRequestV1,
        action: CanvasAuthoringAction,
        expected_target: Option<ChatExecutionTargetRequest>,
    ) -> ChatFuture<CanvasAuthoringRequestV1> {
        let core = self.core.clone();
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    let chat = core.chat();
                    let action_id = uuid::Uuid::new_v4().to_string();
                    if action == CanvasAuthoringAction::Approve {
                        return chat
                            .approve_canvas_request(ApproveCanvasRequestV1 {
                                project_id: row.project_id,
                                conversation_id: row.conversation_id,
                                request_id: row.request_id,
                                expected_request_revision: row.request_revision,
                                action_id,
                                expected_target: expected_target.ok_or_else(|| {
                                    UiCommandError::worker(
                                        "Choose a Companion model before approving Canvas.",
                                    )
                                })?,
                            })
                            .map_err(UiCommandError::from);
                    }
                    let request = DecideCanvasRequestV1 {
                        project_id: row.project_id,
                        conversation_id: row.conversation_id,
                        request_id: row.request_id,
                        expected_request_revision: row.request_revision,
                        action_id,
                    };
                    match action {
                        CanvasAuthoringAction::Decline => chat.decline_canvas_request(request),
                        CanvasAuthoringAction::Stop => chat.cancel_canvas_job(request),
                        _ => {
                            return Err(UiCommandError::worker(
                                "This Canvas action is unavailable.",
                            ));
                        }
                    }
                    .map_err(UiCommandError::from)
                })
                .await
                .map_err(|_| UiCommandError::worker("The Canvas action was interrupted."))?
        })
    }

    fn resolve(&self, row: CanvasAuthoringRequestV1) -> ChatFuture<CanvasAuthoringReadyReceiptV1> {
        let core = self.core.clone();
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    core.chat()
                        .resolve_canvas_ready_target(GetCanvasRequestV1 {
                            project_id: row.project_id,
                            conversation_id: row.conversation_id,
                            request_id: row.request_id,
                        })
                        .map_err(UiCommandError::from)
                })
                .await
                .map_err(|_| UiCommandError::worker("Opening the Canvas result was interrupted."))?
        })
    }
}

#[derive(Default)]
struct AuthoringConversation {
    rows: Vec<CanvasAuthoringRequestV1>,
    generation: u64,
    load_error: Option<String>,
    errors: BTreeMap<String, String>,
    pending: BTreeMap<String, u64>,
}

#[derive(Clone)]
struct LoadToken {
    scope: AuthoringScope,
    generation: u64,
}

#[derive(Clone)]
struct ActionToken {
    scope: AuthoringScope,
    request_id: String,
    generation: u64,
}

#[derive(Default)]
pub(crate) struct CanvasAuthoringUi {
    pub backend: Option<Arc<dyn CanvasAuthoringBackend>>,
    active_scope: Option<AuthoringScope>,
    conversations: BTreeMap<AuthoringScope, AuthoringConversation>,
    generation: u64,
    load_tasks: BTreeMap<AuthoringScope, Task<()>>,
    action_tasks: BTreeMap<String, Task<()>>,
}

fn row_in_scope(scope: &AuthoringScope, row: &CanvasAuthoringRequestV1) -> bool {
    row.project_id == scope.project_id
        && row.conversation_id == scope.conversation_id
        && !row.request_id.is_empty()
        && !row.assistant_message_id.is_empty()
        && row.request_revision > 0
}

fn available_actions(row: &CanvasAuthoringRequestV1, busy: bool) -> Vec<CanvasAuthoringAction> {
    use CanvasAuthoringAction as Action;
    use CanvasAuthoringRequestStateV1 as State;
    if busy {
        return Vec::new();
    }
    match row.state {
        State::AwaitingAuthorization if row.source_available => {
            vec![Action::Approve, Action::Decline]
        }
        State::AwaitingAuthorization => vec![Action::Decline],
        State::Running if row.job_id.is_some() => vec![Action::Stop],
        State::Ready if row.drawing_available && row.ready.is_some() => vec![Action::Open],
        _ => Vec::new(),
    }
}

fn receipt_matches(
    scope: &AuthoringScope,
    row: &CanvasAuthoringRequestV1,
    receipt: &CanvasAuthoringReadyReceiptV1,
) -> bool {
    row_in_scope(scope, row)
        && row.state == CanvasAuthoringRequestStateV1::Ready
        && row.drawing_available
        && receipt.project_id == scope.project_id
        && receipt.conversation_id == scope.conversation_id
        && receipt.request_id == row.request_id
        && row.job_id.as_ref() == Some(&receipt.job_id)
        && row.ready.as_ref().is_some_and(|ready| {
            ready.drawing_id == receipt.drawing_id
                && ready.job_id == receipt.job_id
                && ready.drawing_revision == receipt.drawing_revision
        })
        && !receipt.drawing_id.is_empty()
        && receipt.drawing_revision > 0
}

impl CanvasAuthoringUi {
    fn begin_load(&mut self, scope: AuthoringScope) -> LoadToken {
        self.generation = self.generation.saturating_add(1);
        let conversation = self.conversations.entry(scope.clone()).or_default();
        conversation.generation = self.generation;
        conversation.load_error = None;
        LoadToken {
            scope,
            generation: self.generation,
        }
    }

    fn apply_load(
        &mut self,
        token: &LoadToken,
        result: Result<Vec<CanvasAuthoringRequestV1>, UiCommandError>,
    ) -> bool {
        let Some(conversation) = self
            .conversations
            .get_mut(&token.scope)
            .filter(|value| value.generation == token.generation)
        else {
            return false;
        };
        match result {
            Ok(mut rows) => {
                let mut identities = std::collections::BTreeSet::new();
                if rows.iter().any(|row| {
                    !row_in_scope(&token.scope, row) || !identities.insert(row.request_id.clone())
                }) {
                    conversation.load_error =
                        Some("Canvas request context could not be verified.".into());
                    return true;
                }
                for row in &mut rows {
                    if let Some(current) = conversation.rows.iter().find(|current| {
                        current.request_id == row.request_id
                            && current.request_revision > row.request_revision
                    }) {
                        *row = current.clone();
                    }
                }
                conversation.rows = rows;
            }
            Err(error) => conversation.load_error = Some(error.presentable()),
        }
        true
    }

    fn begin_action(
        &mut self,
        scope: &AuthoringScope,
        request_id: &str,
        action: CanvasAuthoringAction,
    ) -> Option<(ActionToken, CanvasAuthoringRequestV1)> {
        if self.active_scope.as_ref() != Some(scope) {
            return None;
        }
        let conversation = self.conversations.get_mut(scope)?;
        let row = conversation
            .rows
            .iter()
            .find(|row| row.request_id == request_id)?
            .clone();
        if !available_actions(&row, conversation.pending.contains_key(request_id)).contains(&action)
        {
            return None;
        }
        self.generation = self.generation.saturating_add(1);
        conversation.generation = self.generation;
        conversation
            .pending
            .insert(request_id.into(), self.generation);
        conversation.errors.remove(request_id);
        Some((
            ActionToken {
                scope: scope.clone(),
                request_id: request_id.into(),
                generation: self.generation,
            },
            row,
        ))
    }

    fn finish_action(
        &mut self,
        token: &ActionToken,
        result: Result<Option<CanvasAuthoringRequestV1>, UiCommandError>,
    ) -> bool {
        let Some(conversation) = self.conversations.get_mut(&token.scope) else {
            return false;
        };
        if conversation.pending.get(&token.request_id) != Some(&token.generation) {
            return false;
        }
        conversation.pending.remove(&token.request_id);
        match result {
            Ok(Some(row)) => {
                if let Some(current) = conversation
                    .rows
                    .iter_mut()
                    .find(|row| row.request_id == token.request_id)
                {
                    if !row_in_scope(&token.scope, &row)
                        || row.request_id != current.request_id
                        || row.assistant_message_id != current.assistant_message_id
                        || row.source_sha256 != current.source_sha256
                    {
                        conversation.errors.insert(
                            token.request_id.clone(),
                            "Canvas action context could not be verified.".into(),
                        );
                    } else if row.request_revision >= current.request_revision {
                        *current = row;
                    }
                }
            }
            Ok(None) => {}
            Err(error) => {
                conversation
                    .errors
                    .insert(token.request_id.clone(), error.presentable());
            }
        }
        true
    }
}

impl AppModel {
    fn current_canvas_authoring_scope(&self, cx: &Context<Self>) -> Option<AuthoringScope> {
        let chat = self.chat.read(cx);
        let project_id = chat.project_id.as_ref()?.to_string();
        if self
            .workspace
            .read(cx)
            .project_id
            .as_ref()
            .map(|id| id.as_ref())
            != Some(project_id.as_str())
        {
            return None;
        }
        Some(AuthoringScope {
            project_id,
            conversation_id: chat.active_conversation.as_ref()?.to_string(),
        })
    }

    pub(crate) fn refresh_canvas_authoring(&mut self, cx: &mut Context<Self>) {
        self.canvas_authoring.active_scope = self.current_canvas_authoring_scope(cx);
        if let Some(scope) = self.canvas_authoring.active_scope.clone() {
            self.load_canvas_authoring_scope(scope, cx);
        }
    }

    pub(crate) fn canvas_authoring_load_error(&self, cx: &Context<Self>) -> Option<String> {
        self.current_canvas_authoring_scope(cx)
            .and_then(|scope| self.canvas_authoring.conversations.get(&scope))
            .and_then(|conversation| conversation.load_error.clone())
    }

    fn load_canvas_authoring_scope(&mut self, scope: AuthoringScope, cx: &mut Context<Self>) {
        let Some(backend) = self.canvas_authoring.backend.clone() else {
            return;
        };
        let token = self.canvas_authoring.begin_load(scope.clone());
        let task = cx.spawn(async move |this, cx| {
            let result = backend.list(token.scope.clone()).await;
            let _ = this.update(cx, |model, cx| {
                if model.canvas_authoring.apply_load(&token, result) {
                    model.canvas_authoring.load_tasks.remove(&token.scope);
                    cx.notify();
                }
            });
        });
        self.canvas_authoring.load_tasks.insert(scope, task);
    }

    pub(crate) fn handle_canvas_authoring_changed(
        &mut self,
        project_id: String,
        conversation_id: String,
        cx: &mut Context<Self>,
    ) {
        let scope = AuthoringScope {
            project_id,
            conversation_id,
        };
        if self.current_canvas_authoring_scope(cx).as_ref() == Some(&scope)
            || self.canvas_authoring.conversations.contains_key(&scope)
        {
            self.load_canvas_authoring_scope(scope, cx);
        }
    }

    pub(crate) fn canvas_authoring_action(
        &mut self,
        request_id: String,
        action: CanvasAuthoringAction,
        cx: &mut Context<Self>,
    ) {
        let Some(scope) = self.current_canvas_authoring_scope(cx) else {
            return;
        };
        let Some(backend) = self.canvas_authoring.backend.clone() else {
            return;
        };
        let expected_target = {
            let chat = self.chat.read(cx);
            if action == CanvasAuthoringAction::Approve
                && (chat.target_change_pending || chat.loading)
            {
                return;
            }
            chat.panel_target()
        };
        if action == CanvasAuthoringAction::Approve && expected_target.is_none() {
            return;
        }
        let Some((token, row)) = self
            .canvas_authoring
            .begin_action(&scope, &request_id, action)
        else {
            return;
        };
        let origin_view = self.active_view();
        let key = request_id;
        let task = cx.spawn(async move |this, cx| {
            if action == CanvasAuthoringAction::Open {
                let result = backend.resolve(row.clone()).await.and_then(|receipt| {
                    if receipt_matches(&token.scope, &row, &receipt) {
                        Ok(receipt)
                    } else {
                        Err(UiCommandError::worker(
                            "The Canvas result does not match this request.",
                        ))
                    }
                });
                let _ = this.update(cx, |model, cx| {
                    let finish = result.as_ref().map(|_| None).map_err(Clone::clone);
                    if !model.canvas_authoring.finish_action(&token, finish) {
                        return;
                    }
                    model
                        .canvas_authoring
                        .action_tasks
                        .remove(&token.request_id);
                    if let Ok(receipt) = result {
                        let current = model.current_canvas_authoring_scope(cx);
                        if current.as_ref() == Some(&token.scope)
                            && model.active_view() == origin_view
                            && model
                                .canvas_authoring
                                .conversations
                                .get(&token.scope)
                                .and_then(|conversation| {
                                    conversation
                                        .rows
                                        .iter()
                                        .find(|current| current.request_id == token.request_id)
                                })
                                .is_some_and(|current| {
                                    receipt_matches(&token.scope, current, &receipt)
                                })
                        {
                            model.open_authored_canvas(receipt.project_id, receipt.drawing_id, cx);
                        }
                    }
                    cx.notify();
                });
            } else {
                let result = backend.decide(row, action, expected_target).await;
                let _ = this.update(cx, |model, cx| {
                    if model
                        .canvas_authoring
                        .finish_action(&token, result.map(Some))
                    {
                        model
                            .canvas_authoring
                            .action_tasks
                            .remove(&token.request_id);
                        model.load_canvas_authoring_scope(token.scope.clone(), cx);
                        cx.notify();
                    }
                });
            }
        });
        self.canvas_authoring.action_tasks.insert(key, task);
        cx.notify();
    }
}

fn state_label(state: CanvasAuthoringRequestStateV1) -> &'static str {
    use CanvasAuthoringRequestStateV1 as State;
    match state {
        State::AwaitingAuthorization => "Waiting for your approval",
        State::Declined => "Canvas offer declined",
        State::Running => "Preparing your drawing…",
        State::Ready => "Drawing ready",
        State::Failed => "Canvas generation failed",
        State::Cancelled => "Canvas generation stopped",
        State::Interrupted => "Canvas generation was interrupted",
    }
}

pub(crate) fn render_canvas_authoring_card(
    model: &AppModel,
    message_id: &str,
    cx: &mut Context<AppModel>,
) -> Option<AnyElement> {
    let (rows, pending, errors, target_label, approve_enabled) = {
        let scope = model.current_canvas_authoring_scope(cx)?;
        let conversation = model.canvas_authoring.conversations.get(&scope)?;
        let rows: Vec<_> = conversation
            .rows
            .iter()
            .filter(|row| row.assistant_message_id == message_id)
            .cloned()
            .collect();
        if rows.is_empty() {
            return None;
        }
        let chat = model.chat.read(cx);
        let target_label = chat
            .active_conversation_record()
            .map(|target| {
                let profile = target
                    .model_profile
                    .as_deref()
                    .map(|profile| format!(" · {profile}"))
                    .unwrap_or_default();
                format!("{} · {}{profile}", target.provider_id, target.model_id)
            })
            .unwrap_or_else(|| "Current Companion selection".into());
        (
            rows,
            conversation.pending.clone(),
            conversation.errors.clone(),
            target_label,
            !chat.loading && !chat.target_change_pending && chat.panel_target().is_some(),
        )
    };
    let mut cards = Vec::new();
    for row in rows {
        let profile = match row.profile {
            ikore_core::companion_drawings::semantics::CanvasProfileV1::Architecture => {
                "Architecture"
            }
            ikore_core::companion_drawings::semantics::CanvasProfileV1::Bpmn => {
                "Process · Ikore BPMN profile"
            }
        };
        let busy = pending.contains_key(&row.request_id);
        let mut buttons = div().flex().flex_wrap().gap_2();
        for action in available_actions(&row, busy) {
            let (name, label, variant) = match action {
                CanvasAuthoringAction::Approve => ("approve", "Approve", ButtonVariant::Primary),
                CanvasAuthoringAction::Decline => ("decline", "Decline", ButtonVariant::Secondary),
                CanvasAuthoringAction::Stop => ("stop", "Stop", ButtonVariant::Danger),
                CanvasAuthoringAction::Open => ("open", "Open Canvas", ButtonVariant::Primary),
            };
            let enabled = action != CanvasAuthoringAction::Approve || approve_enabled;
            let request_id = row.request_id.clone();
            let id = format!("canvas-offer-{name}:{}", row.request_id);
            let selector = id.clone();
            let button = button_frame(
                div()
                    .id(SharedString::from(id))
                    .debug_selector(move || selector.clone()),
                variant,
                enabled,
            )
            .child(label);
            buttons = buttons.child(if enabled {
                button.on_click(cx.listener(move |model, _, _, cx| {
                    model.canvas_authoring_action(request_id.clone(), action, cx)
                }))
            } else {
                button
            });
        }
        let request_id = row.request_id.clone();
        let card = div()
            .debug_selector(move || format!("canvas-offer:{request_id}"))
            .mt_2()
            .p_3()
            .min_w_0()
            .rounded_md()
            .border_1()
            .border_color(theme::border())
            .bg(theme::surface())
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(format!("Canvas · {}", state_label(row.state))),
            )
            .child(div().text_sm().child(row.title.clone()))
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(profile),
            )
            .when(
                row.state == CanvasAuthoringRequestStateV1::AwaitingAuthorization,
                |card| {
                    card.child(div().text_sm().child(row.request_text.clone()))
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::text_muted())
                                .child(format!("Model: {target_label}")),
                        )
                },
            )
            .when(!row.source_available, |card| {
                card.child(
                    div()
                        .text_sm()
                        .text_color(theme::warning())
                        .child("The original explanation is no longer available."),
                )
            })
            .when(
                row.state == CanvasAuthoringRequestStateV1::Ready && !row.drawing_available,
                |card| {
                    card.child(
                        div()
                            .text_sm()
                            .text_color(theme::warning())
                            .child("The drawing is no longer available."),
                    )
                },
            )
            .when_some(row.failure.as_ref(), |card, failure| {
                card.child(
                    div()
                        .text_sm()
                        .text_color(theme::danger())
                        .child(failure.message.clone()),
                )
            })
            .when_some(errors.get(&row.request_id), |card, error| {
                card.child(
                    div()
                        .text_sm()
                        .text_color(theme::danger())
                        .child(error.clone()),
                )
            })
            .when(busy, |card| {
                card.child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child("Updating Canvas request…"),
                )
            })
            .child(buttons);
        cards.push(card.into_any_element());
    }
    Some(
        div()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .children(cards)
            .into_any_element(),
    )
}

#[cfg(test)]
#[path = "canvas_authoring_ui_tests.rs"]
mod tests;
