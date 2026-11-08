//! Floating presentation of project chats and host-authored decision cards.
use gpui::{AnyElement, Context, IntoElement, SharedString, Window, deferred, div, prelude::*, px};
use hvc_core::orchestration::decisions::{PendingDecision, PendingDecisionAction};

use crate::{AppModel, app_model::AppView, companion_dock::DockWindow, theme};

fn button(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    enabled: bool,
    cx: &mut Context<AppModel>,
    click: impl Fn(&mut AppModel, &mut Context<AppModel>) + 'static,
) -> AnyElement {
    let id = id.into();
    let selector = id.clone();
    div()
        .id(id)
        .debug_selector(move || selector.to_string())
        .tab_index(0)
        .px_2()
        .py_1()
        .rounded_md()
        .text_xs()
        .border_1()
        .border_color(theme::border())
        .bg(theme::surface_raised())
        .text_color(if enabled {
            theme::text()
        } else {
            theme::text_subtle()
        })
        .when(enabled, |control| {
            control
                .cursor_pointer()
                .hover(|style| style.border_color(theme::accent()))
                .on_click(cx.listener(move |model, _, _, cx| {
                    click(model, cx);
                    cx.stop_propagation();
                }))
        })
        .child(label.into())
        .into_any_element()
}

pub(crate) fn dock(model: &AppModel, window: &Window, cx: &mut Context<AppModel>) -> AnyElement {
    let dock = &model.companion_dock;
    let width = f32::from(window.viewport_size().width);
    let height = f32::from(window.viewport_size().height);
    let visible = dock.visible_windows(width);
    let has_project = dock.project_id.is_some();
    let show_history = dock.open && (dock.history_open || visible.is_empty());
    let hidden = dock.windows.len().saturating_sub(visible.len());
    let visible_keys = if show_history {
        visible
            .iter()
            .map(|item| item.key.clone())
            .collect::<std::collections::HashSet<_>>()
    } else {
        std::collections::HashSet::new()
    };
    let search = dock.history_search.to_lowercase();
    let history_input = dock.history_input.clone();
    let persistence_failed = dock.persistence_error.is_some();
    let error = match (&dock.persistence_error, &dock.error) {
        (Some(persistence), Some(other)) => Some(format!("{persistence}\n{other}")),
        (Some(persistence), None) => Some(persistence.clone()),
        (None, other) => other.clone(),
    };
    let mut stack = div()
        .id("companion-dock")
        .debug_selector(|| "companion-dock".into())
        .absolute()
        .right(px(12.0))
        .bottom(px(34.0))
        .max_w(px((width - 24.0).max(100.0)))
        .flex()
        .flex_col()
        .items_end()
        .gap_2()
        .key_context("CompanionDock");
    if dock.open {
        let mut row = div().flex().items_end().gap_2();
        for item in visible {
            row = row.child(topic_window(
                model,
                item,
                (height - 130.0).clamp(180.0, 590.0),
                cx,
            ));
        }
        if show_history {
            let conversations = model
                .chat
                .read(cx)
                .conversations
                .iter()
                .filter(|conversation| conversation.title.to_lowercase().contains(&search))
                .cloned()
                .collect::<Vec<_>>();
            let pending = dock.pending.clone();
            let mut history = div()
                .id("companion-dock-history")
                .debug_selector(|| "companion-dock-history".into())
                .w(px(300.0))
                .max_h(px((height - 150.0).max(160.0)))
                .overflow_y_scroll()
                .occlude()
                .p_3()
                .rounded_lg()
                .border_1()
                .border_color(theme::border())
                .bg(theme::surface())
                .shadow_lg()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("Companion"),
                )
                .child(button(
                    "dock-new-chat",
                    "New chat",
                    has_project,
                    cx,
                    |model, cx| model.new_companion_chat(cx),
                ));
            if let Some(input) = history_input {
                history = history.child(input);
            }
            if !has_project {
                history = history.child("Open a project to chat.");
            }
            for decision in pending {
                let id = decision.id.clone();
                history = history.child(button(
                    format!("dock-pending-{id}"),
                    format!("Decision · {}", decision.title),
                    true,
                    cx,
                    move |model, cx| model.open_companion_decision(id.clone(), cx),
                ));
            }
            for item in &dock.windows {
                if !visible_keys.contains(&item.key) {
                    let key = item.key.clone();
                    let conversation = item.conversation_id.clone();
                    let decision = item.decision_id.clone();
                    history = history.child(button(
                        format!("dock-restore-{key}"),
                        format!("Reopen · {}", title_for(model, item, cx)),
                        true,
                        cx,
                        move |model, cx| {
                            if let Some(id) = &decision {
                                model.open_companion_decision(id.clone(), cx);
                            } else if let Some(id) = &conversation {
                                model.open_companion_chat(id.clone(), cx);
                            }
                        },
                    ));
                }
            }
            for conversation in conversations {
                let id = conversation.id;
                history = history.child(button(
                    format!("dock-history-{id}"),
                    conversation.title,
                    true,
                    cx,
                    move |model, cx| model.open_companion_chat(id.clone(), cx),
                ));
            }
            row = row.child(history);
        }
        stack = stack.child(row);
    }
    if let Some(error) = error {
        stack = stack.child(
            div()
                .max_w(px(360.0))
                .p_2()
                .rounded_md()
                .bg(theme::surface())
                .border_1()
                .border_color(theme::danger())
                .text_xs()
                .text_color(theme::danger())
                .child(error)
                .when(persistence_failed, |panel| {
                    panel.child(button(
                        "dock-retry-save",
                        "Retry save",
                        has_project,
                        cx,
                        |model, cx| model.retry_companion_persistence(cx),
                    ))
                }),
        );
    }
    stack = stack.child(
        div()
            .flex()
            .gap_2()
            .when(dock.open, |row| {
                row.child(button(
                    "dock-history-toggle",
                    format!(
                        "History{suffix}",
                        suffix = if hidden > 0 {
                            format!(" · {hidden} minimized")
                        } else {
                            String::new()
                        }
                    ),
                    true,
                    cx,
                    |model, cx| {
                        model.companion_dock.history_open = !model.companion_dock.history_open;
                        cx.notify();
                    },
                ))
            })
            .child(button(
                "dock-launcher",
                format!(
                    "Companion{}",
                    if dock.pending.is_empty() {
                        String::new()
                    } else {
                        format!(" · {}", dock.pending.len())
                    }
                ),
                true,
                cx,
                |model, cx| model.toggle_companion_dock(cx),
            )),
    );
    deferred(stack).with_priority(12).into_any_element()
}

fn title_for(model: &AppModel, item: &DockWindow, cx: &Context<AppModel>) -> String {
    item.decision_id
        .as_ref()
        .and_then(|id| {
            model
                .companion_dock
                .pending
                .iter()
                .find(|decision| &decision.id == id)
                .map(|decision| decision.title.clone())
        })
        .or_else(|| {
            item.conversation_id.as_ref().and_then(|id| {
                model
                    .chat
                    .read(cx)
                    .conversations
                    .iter()
                    .find(|conversation| &conversation.id == id)
                    .map(|conversation| conversation.title.clone())
            })
        })
        .unwrap_or_else(|| "Companion".into())
}

fn topic_window(
    model: &AppModel,
    item: DockWindow,
    height: f32,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let key = item.key.clone();
    let minimize = key.clone();
    let close = key.clone();
    let title = title_for(model, &item, cx);
    let mut panel = div()
        .id(SharedString::from(format!("dock-topic-{key}")))
        .debug_selector({
            let key = key.clone();
            move || format!("dock-topic-{key}")
        })
        .w(px(304.0))
        .h(px(height))
        .min_h_0()
        .flex()
        .flex_col()
        .occlude()
        .rounded_lg()
        .border_1()
        .border_color(theme::border_strong())
        .bg(theme::surface())
        .shadow_lg()
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .p_2()
                .border_b_1()
                .border_color(theme::border())
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .text_ellipsis()
                        .child(title),
                )
                .child(button(
                    format!("dock-minimize-{key}"),
                    "−",
                    true,
                    cx,
                    move |model, cx| model.close_companion_topic(minimize.clone(), true, cx),
                ))
                .child(button(
                    format!("dock-close-{key}"),
                    "×",
                    true,
                    cx,
                    move |model, cx| model.close_companion_topic(close.clone(), false, cx),
                )),
        );
    let decision = item
        .decision_id
        .as_ref()
        .and_then(|id| {
            model
                .companion_dock
                .pending
                .iter()
                .find(|decision| &decision.id == id)
        })
        .cloned();
    let mut scroll = div()
        .id(SharedString::from(format!("dock-scroll-{key}")))
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .p_2()
        .flex()
        .flex_col()
        .gap_2();
    if let Some(decision) = decision {
        scroll = scroll.child(decision_card(
            model,
            &decision,
            item.conversation_id.as_deref(),
            cx,
        ));
    } else if item.decision_id.is_some() {
        scroll = scroll.child(
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child("This decision has been resolved. Conversation history is preserved."),
        );
    }
    if let Some(id) = &item.conversation_id {
        let chat = model.chat.read(cx);
        let messages = chat.rendered_messages_for(id).0.to_vec();
        let streaming = chat.is_streaming(id);
        let busy = chat.is_busy(id);
        let error = chat.error_for(id).map(|error| error.presentable());
        let accepted = chat.disclosure_accepted_for(id);
        let available = chat
            .conversations
            .iter()
            .find(|conversation| &conversation.id == id)
            .is_some_and(|conversation| conversation.connection_available);
        let in_full_chat = model.active_view() == AppView::Chat
            && chat.active_conversation.as_ref().map(AsRef::as_ref) == Some(id.as_str());
        for message in messages {
            scroll = scroll.child(crate::chat_view::render_message(message, cx));
        }
        if let Some(error) = error {
            scroll = scroll.child(div().text_xs().text_color(theme::danger()).child(error));
        }
        panel = panel.child(scroll);
        if !accepted {
            let id = id.clone();
            panel = panel.child(div().p_2().text_xs().child("Requested repository snippets may be sent to this conversation's provider.")
                .child(button(format!("dock-authorize-{id}"), "Authorize this chat", true, cx, move |model, cx| {
                    model.chat.update(cx, |chat, _| chat.acknowledge_disclosure_for(&id)); cx.notify();
                })));
        }
        if in_full_chat {
            let id = id.clone();
            panel = panel.child(div().p_2().child(button(
                format!("dock-continue-{id}"),
                "Continue in full Chat",
                true,
                cx,
                move |model, cx| model.expand_companion_chat(id.clone(), cx),
            )));
        } else if let Some(input) = model.chat_composer_for(id) {
            let empty = input.read(cx).value().trim().is_empty();
            let send_id = id.clone();
            let cancel_id = id.clone();
            let expand_id = id.clone();
            panel = panel.child(
                div()
                    .debug_selector({
                        let id = id.clone();
                        move || format!("dock-composer-{id}")
                    })
                    .p_2()
                    .border_t_1()
                    .border_color(theme::border())
                    .child(input.clone())
                    .child(
                        div()
                            .mt_2()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(button(
                                format!("dock-expand-{id}"),
                                "Expand",
                                true,
                                cx,
                                move |model, cx| model.expand_companion_chat(expand_id.clone(), cx),
                            ))
                            .child(if streaming {
                                button(
                                    format!("dock-cancel-{id}"),
                                    "Cancel",
                                    true,
                                    cx,
                                    move |model, cx| {
                                        model.cancel_chat_turn_for(cancel_id.clone(), cx)
                                    },
                                )
                            } else {
                                button(
                                    format!("dock-send-{id}"),
                                    if busy { "Sending…" } else { "Send" },
                                    !empty && accepted && !busy && available,
                                    cx,
                                    move |model, cx| {
                                        let content = input.read(cx).value().to_owned();
                                        model.send_chat_message_to(send_id.clone(), content, cx);
                                    },
                                )
                            }),
                    ),
            );
        }
    } else {
        panel = panel.child(scroll);
        if let Some(id) = item.decision_id {
            panel = panel.child(div().p_2().child(button(
                format!("dock-help-{id}"),
                "Ask Companion",
                true,
                cx,
                move |model, cx| model.ask_companion_about_decision(id.clone(), cx),
            )));
        }
    }
    panel.into_any_element()
}

fn decision_card(
    model: &AppModel,
    decision: &PendingDecision,
    conversation: Option<&str>,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    if decision.forge_questions.is_some() {
        return crate::companion_forge::question_card(model, decision, conversation, cx);
    }
    if decision.kind == hvc_core::orchestration::decisions::PendingDecisionKind::CliFallback {
        let (fallback, cancel_armed, busy) = {
            let agents = model.agents.read(cx);
            (
                agents
                    .fallbacks
                    .iter()
                    .find(|fallback| {
                        Some(&fallback.checkpoint_id) == decision.checkpoint_id.as_ref()
                    })
                    .cloned(),
                agents.pending_fallback_cancel.clone(),
                agents.busy,
            )
        };
        if let Some(fallback) = fallback {
            return crate::agents_view::fallback_panel(
                &fallback,
                cancel_armed.as_deref() == Some(fallback.checkpoint_id.as_str()),
                busy,
                cx,
            );
        }
    }
    let mut card = div()
        .p_2()
        .rounded_md()
        .border_1()
        .border_color(theme::accent())
        .bg(theme::accent_soft())
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child("Host decision"),
        )
        .child(div().text_sm().child(decision.description.clone()));
    if let Some(scope) = model
        .pulsations
        .read(cx)
        .failure_checkpoints
        .iter()
        .find(|checkpoint| Some(&checkpoint.checkpoint_id) == decision.checkpoint_id.as_ref())
        .and_then(|checkpoint| checkpoint.scope_request.as_ref())
    {
        card = card.child(crate::scope_request_view::scope_request_card(scope));
    }
    if let Some(set) = &decision.question_set {
        for question in &set.questions {
            let key = format!("answer:{}:{}", decision.id, question.id);
            card = card.child(div().text_xs().child(question.question.clone()));
            if model
                .companion_dock
                .form_draft_is_stale(&key, decision.state_version)
            {
                card = card.child(stale_draft_warning(&key));
            }
            if let Some(input) = model.companion_dock.forms.get(&key) {
                card = card.child(input.clone());
            }
            if let Some(conversation) = conversation {
                let conversation = conversation.to_owned();
                card = card.child(button(
                    format!("dock-use-{key}"),
                    "Use latest reply as draft",
                    true,
                    cx,
                    move |model, cx| model.use_companion_answer(&conversation, &key, cx),
                ));
            }
        }
    }
    let feedback_key = format!("feedback:{}", decision.id);
    if let Some(input) = model.companion_dock.forms.get(&feedback_key)
        && decision.question_set.is_none()
    {
        if model
            .companion_dock
            .form_draft_is_stale(&feedback_key, decision.state_version)
        {
            card = card.child(stale_draft_warning(&feedback_key));
        }
        card = card.child(input.clone());
    }
    let busy = model.companion_dock.action_busy.contains(&decision.id);
    let mut actions = div().flex().flex_wrap().gap_1();
    for action in &decision.actions {
        let (label, verb) = match action {
            PendingDecisionAction::Approve => ("Approve", Some("approve")),
            PendingDecisionAction::RequestRevision => ("Request changes", Some("revision")),
            PendingDecisionAction::Reject => ("Reject", Some("reject")),
            PendingDecisionAction::AnswerQuestions => ("Submit answers", Some("answer")),
            PendingDecisionAction::Retry => ("Retry", Some("retry")),
            PendingDecisionAction::Abort => ("Abort", Some("abort")),
            PendingDecisionAction::GrantScope => ("Grant and retry", Some("grant")),
            PendingDecisionAction::OpenAgents => ("Open provider actions", None),
            PendingDecisionAction::OpenForge => ("Open Feature Forge", None),
            PendingDecisionAction::OpenDelivery => ("Open Delivery", None),
            PendingDecisionAction::OpenReconciliation => ("Open reconciliation", None),
            PendingDecisionAction::OpenFullReview => ("Open full review", None),
        };
        let decision = decision.clone();
        let confirming = verb.is_some_and(|verb| {
            model.companion_dock.confirming.as_deref() == Some(&format!("{}:{verb}", decision.id))
        });
        actions = actions.child(button(
            format!("dock-action-{}-{action:?}", decision.id),
            if confirming {
                format!("Confirm {label}")
            } else {
                label.into()
            },
            !busy,
            cx,
            move |model, cx| {
                if let Some(verb) = verb {
                    model.resolve_companion_decision(decision.clone(), verb, cx);
                } else {
                    model.open_companion_decision_detail(decision.clone(), cx);
                }
            },
        ));
    }
    card.child(actions).into_any_element()
}

fn stale_draft_warning(key: &str) -> AnyElement {
    let key = key.to_owned();
    div().debug_selector(move || format!("dock-stale-draft-{key}"))
        .text_xs().text_color(theme::warning())
        .child("This draft was written for an earlier decision. Review the current context before submitting.")
        .into_any_element()
}
