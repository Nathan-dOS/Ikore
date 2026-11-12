use gpui::{AnyElement, Context, IntoElement, SharedString, div, prelude::*, px};
use ikore_core::chat_service::{
    ChatConversation, ChatMessage, ChatMessageModelEvidence, ChatModelVerification,
    ChatToolArgumentValue, ChatToolCall, ChatTransportKind,
};
use ikore_core::terminals::{CliAvailabilityState, CliBackend};
#[cfg(test)]
use ikore_editor::{parse_chat_content, parse_markdown};

use crate::agents::CliModelPickerTarget;
use crate::agents_view::cli_model_picker_panel;
use crate::app_model::AppModel;
use crate::theme;

use crate::selectable_message::SelectableMessage;

pub(crate) fn chat_panel(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let (
        project_id,
        connections,
        conversations,
        active_conversation,
        (messages, hidden_messages),
        tool_calls,
        selected_connection_id,
        selected_transport,
        selected_cli_backend,
        selected_cli_model,
        selected_cli_model_profile,
        selected_cli_model_source,
        selected_cli_availability,
        new_target_available,
        active_model_picker,
        streaming,
        loading,
        busy,
        target_change_pending,
        _error,
        disclosure_required,
        disclosure_accepted,
        pending_delete,
    ) = {
        let chat = model.chat.read(cx);
        (
            chat.project_id.clone(),
            chat.eligible_connections().cloned().collect::<Vec<_>>(),
            chat.conversations.clone(),
            chat.active_conversation_record().cloned(),
            {
                let (messages, hidden) = chat.rendered_messages();
                (messages.to_vec(), hidden)
            },
            chat.active_tool_calls().to_vec(),
            chat.selected_connection_id.clone(),
            chat.selected_transport,
            chat.selected_cli_backend,
            chat.selected_cli_model.clone(),
            chat.selected_cli_model_profile.clone(),
            chat.selected_cli_model_source,
            chat.selected_cli_availability().cloned(),
            chat.new_conversation_target_available(),
            model
                .agents
                .read(cx)
                .active_model_picker
                .clone()
                .filter(|picker| picker.target == CliModelPickerTarget::Companion),
            chat.streaming,
            chat.loading,
            chat.busy,
            chat.target_change_pending,
            chat.error.clone(),
            chat.disclosure_required(),
            chat.disclosure_accepted(),
            chat.pending_delete_conversation_id.clone(),
        )
    };

    if project_id.is_none() {
        return div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .p_6()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .max_w(px(560.0))
                    .p_6()
                    .rounded_lg()
                    .bg(theme::surface_raised())
                    .child(
                        div()
                            .text_xl()
                            .font_weight(gpui::FontWeight::BOLD)
                            .child("Open a project to chat"),
                    )
                    .child(
                        div()
                            .mt_2()
                            .text_sm()
                            .text_color(theme::text_muted())
                            .child(
                                "The Companion belongs to the active workspace and never accesses another repository.",
                            ),
                    ),
            )
            .into_any_element();
    }

    let selected_connection = selected_connection_id.as_ref().and_then(|selected| {
        connections
            .iter()
            .find(|connection| connection.id == selected.as_ref())
    });
    let draft = model.chat_draft_input.clone();
    let draft_empty = draft.read(cx).value().trim().is_empty();
    let tool_activity_open = model.chat.read(cx).tool_activity_open;
    let config_collapsed = model.chat.read(cx).config_collapsed;
    let target_change_deferred = model.chat.read(cx).target_change_deferred;
    let show_tool_activity = should_show_tool_activity(active_conversation.as_ref())
        && (active_conversation.is_some() || selected_transport == ChatTransportKind::Api);
    let cli_model_label = crate::agents_view::cli_model_label(
        selected_cli_model_source,
        selected_cli_model.as_deref(),
    );
    let cli_profile_label = selected_cli_model_profile
        .clone()
        .unwrap_or_else(|| "CLI default".into());
    let cli_target = selected_transport == ChatTransportKind::Cli;
    let composer_target = composer_target_controls(
        ComposerTarget {
            model: if cli_target {
                cli_model_label.clone()
            } else {
                selected_connection
                    .map(|connection| {
                        visible_api_model(active_conversation.as_ref(), &connection.default_model)
                            .to_owned()
                    })
                    .unwrap_or_else(|| "none".into())
            },
            model_selectable: cli_target,
            effort: cli_target.then(|| {
                (
                    cli_profile_label.clone(),
                    selected_cli_model.is_some()
                        && !model
                            .model_profiles(selected_cli_backend, selected_cli_model.as_deref(), cx)
                            .is_empty(),
                )
            }),
            route: if cli_target {
                format!("{} CLI", selected_cli_backend.label())
            } else {
                selected_connection
                    .map(|connection| format!("API · {}", connection.display_name))
                    .unwrap_or_else(|| "API · no connection".into())
            },
            enabled: !busy && !target_change_pending,
            deferred: target_change_deferred,
        },
        cx,
    );

    div()
        .relative().overflow_hidden()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .p_3()
        .flex()
        .gap_3()
        .child(
            div()
                .w(px(260.0)).flex_shrink_0()
                .min_h_0()
                .p_3()
                .border_r_1()
                .border_color(theme::border())
                .flex()
                .flex_col()
                .gap_3()
                .child(section_header("ACTIVE PROJECT", "Conversations", conversations.len()))
                .child(action(
                    "chat-toggle-config",
                    if config_collapsed { "Show connection config" } else { "Hide connection config" },
                    true,
                    cx,
                    |model, cx| {
                        model.chat.update(cx, |chat, _| chat.config_collapsed = !chat.config_collapsed);
                        cx.notify();
                    },
                ))
                .when(config_collapsed, |panel| {
                    let summary = match selected_transport {
                        ChatTransportKind::Api => selected_connection
                            .map(|connection| {
                                format!("API · {} · {}", connection.display_name, visible_api_model(active_conversation.as_ref(), &connection.default_model))
                            })
                            .unwrap_or_else(|| "API · no connection selected".into()),
                        ChatTransportKind::Cli => {
                            format!("CLI · {} · {}", selected_cli_backend.label(), cli_model_label)
                        }
                    };
                    panel.child(
                        div()
                            .p_2()
                            .rounded_md()
                            .bg(theme::surface_raised())
                            .text_xs()
                            .text_color(theme::text_muted())
                            .child(summary),
                    )
                })
                .when(!config_collapsed, |panel| panel.child({

                    let transport_label = match selected_transport {
                        ChatTransportKind::Api => "API",
                        ChatTransportKind::Cli => "CLI",
                    };
                    div()
                        .child(crate::select_menu::control("chat-transport","TRANSPORT",transport_label,!busy && !streaming && !target_change_pending,cx))

                }))
                .when(!config_collapsed, |panel| panel.child({
                    let mut auth_card = div()
                        .p_3()
                        .rounded_md()
                        .bg(theme::surface_raised())
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::text_muted())
                                .child(if selected_transport == ChatTransportKind::Api {
                                    "API CONNECTION"
                                } else {
                                    "CLI AUTHENTICATION"
                                }),
                        )
                        .child(
                            div()
                                .mt_1()
                                .flex()
                                .items_center()
                                .gap_2()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(
                                    if selected_transport == ChatTransportKind::Api {
                                        selected_connection
                                            .map(|connection| {
                                                format!(
                                                    "{} · {}",
                                                    connection.display_name,
                                                    visible_api_model(active_conversation.as_ref(), &connection.default_model)
                                                )
                                            })
                                            .unwrap_or_else(|| "None available".into())
                                    } else {
                                        selected_cli_availability
                                            .as_ref()
                                            .map(|item| {
                                                format!(
                                                    "{} · {}",
                                                    selected_cli_backend.label(),
                                                    cli_availability_label(item.state)
                                                )
                                            })
                                            .unwrap_or_else(|| "CLI not detected".into())
                                    },
                                )
                                .when(selected_transport == ChatTransportKind::Cli, |row| {
                                    row.child(crate::widgets::info_tip(
                                        "chat-cli-login",
                                        "Uses the CLI's existing login. Ikore does not request or store its API key. Changes apply to the open conversation from its next message and are remembered for this project.",
                                    ))
                                }),
                        );

                    if selected_transport == ChatTransportKind::Api && connections.len() > 1 {

                        let conn_wrapper = div().mt_2().child(crate::select_menu::control(
                            "chat-cycle-connection", "Connection", selected_connection.map(|c| c.display_name.clone()).unwrap_or_default(), !busy && !streaming && !target_change_pending, cx,
                        ));

                        auth_card = auth_card.child(conn_wrapper);
                    } else if selected_transport == ChatTransportKind::Cli {



                        let profiles = model.model_profiles(
                            selected_cli_backend,
                            selected_cli_model.as_deref(),
                            cx,
                        );


                        let backend_div = div().child(crate::select_menu::control("chat-cli-backend","CLI BACKEND",selected_cli_backend.label(),!busy && !streaming && !target_change_pending,cx));


                        let profile_div = div().child(crate::select_menu::control("chat-cli-profile","PROFILE",cli_profile_label,!busy && !streaming && !target_change_pending && selected_cli_model.is_some() && !profiles.is_empty(),cx));


                        auth_card = auth_card
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .child(backend_div)
                                    .child(selection(
                                        "chat-cli-model",
                                        "MODEL",
                                        cli_model_label,
                                        !busy && !streaming && !target_change_pending,
                                        cx,
                                        |model, cx| model.open_companion_model_picker(cx),
                                    ))
                                    .child(profile_div),
                            );
                    }

                    auth_card
                }))
                .child(action(
                    "chat-new-conversation",
                    if busy { "Please wait…" } else { "New conversation" },
                    new_target_available && !busy && !streaming && !target_change_pending,
                    cx,
                    |model, cx| model.create_chat_conversation(cx),
                ))
                .when(selected_transport == ChatTransportKind::Api && connections.is_empty(), |panel| {
                    panel.child(warning(
                        "Configure in Settings an enabled, credentialed connection with tool calling support.",
                    ))
                })
                .when(
                    selected_transport == ChatTransportKind::Cli && !new_target_available,
                    |panel| {
                        panel.child(warning(
                            "Install or authenticate the selected CLI, then refresh Agents diagnostics.",
                        ))
                    },
                )
                .child(
                    div()
                        .id("chat-conversations-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .when(conversations.is_empty() && !loading, |list| {
                            list.child(empty("No conversations in this project."))
                        })
                        .children(conversations.into_iter().map(|conversation| {
                            let id = conversation.id.clone();
                            let selected = active_conversation
                                .as_ref()
                                .is_some_and(|active| active.id == id);
                            let delete_id = id.clone();
                            let conversation_selector = format!("chat-conversation:{id}");
                            let title_selector = format!("chat-title:{id}");
                            let awaiting_confirmation = pending_delete
                                .as_ref()
                                .is_some_and(|pending| pending.as_ref() == id);
                            div()
                                .id(SharedString::from(conversation_selector.clone()))
                                .debug_selector(move || conversation_selector.clone())
                                .p_3()
                                .rounded_md()
                                .border_1()
                                .border_color(if selected {
                                    theme::accent()
                                } else {
                                    theme::border()
                                })
                                .bg(if selected {
                                    theme::accent_soft()
                                } else {
                                    theme::surface_raised()
                                })
                                .child(div().flex().items_start().gap_1().child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .id(SharedString::from(format!("chat-select:{id}")))
                                        .cursor_pointer()
                                        .on_click(cx.listener(move |model, _, _, cx| {
                                            model.select_chat_conversation(id.clone(), cx)
                                        }))
                                        .child(
                                             div()
                                                .debug_selector(move || title_selector.clone())
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .min_w_0()
                                                .child(crate::fading_title::FadingTitle::new(
                                                    conversation.title,
                                                    if selected { theme::accent_soft() } else { theme::surface_raised() },
                                                )),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(theme::text_muted())
                                                .child(format!(
                                                    "{} · {} · {}",
                                                    match conversation.transport {
                                                        ChatTransportKind::Api => "API",
                                                        ChatTransportKind::Cli => "CLI",
                                                    },
                                                    conversation.provider_id,
                                                    conversation.model_id
                                                )),
                                        ),
                                ).child(compact_icon_action(
                                    SharedString::from(format!("chat-delete:{delete_id}")),
                                    if awaiting_confirmation { crate::icons::Icon::Check } else { crate::icons::Icon::Trash },
                                    if awaiting_confirmation { "Confirm deletion" } else { "Delete conversation" },
                                    if awaiting_confirmation { theme::danger() } else { theme::text_muted() },
                                    !busy && !streaming,
                                    cx,
                                    move |model, cx| {
                                        model.delete_chat_conversation(delete_id.clone(), cx)
                                    },
                                )))
                        })),
                ),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .p_4()
                .rounded_lg()
                .flex()
                .flex_col()
                .items_center()
                .gap_3()
                .when(show_tool_activity, |panel| panel.child(div().w_full().flex().justify_end().child(action(
                    "chat-toggle-activity", if tool_activity_open { "Hide activity" } else { "Show activity" }, true, cx,
                    |model, cx| { model.chat.update(cx, |chat, _| chat.tool_activity_open = !chat.tool_activity_open); cx.notify(); }
                ))))

                .when(active_conversation.is_none(), |panel| {
                    panel.child(
                        div()
                            .flex_1()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(if loading {
                                "Loading conversations…"
                            } else {
                                "Choose a compatible connection and start a conversation."
                            }),
                    )
                })
                .when_some(active_conversation.clone(), |panel, conversation| {
                    panel
                        .child(centred(
                            chat_column()
                                .debug_selector(|| "chat-header".into())
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_3()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .text_lg()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child(conversation.title.clone()),
                                )
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .px_2()
                                        .py_1()
                                        .rounded_md()
                                        .bg(theme::surface_raised())
                                        .text_xs()
                                        .text_color(theme::text_muted())
                                        .child(conversation.provider_id.clone())
                                        .child(
                                            div()
                                                .font_family(theme::FONT_MONO)
                                                .text_color(theme::text())
                                                .child(conversation.model_id.clone()),
                                        ),
                                ),
                        ))
                        .when(!conversation.connection_available, |panel| {
                            panel.child(centred(chat_column().child(warning(
                                if conversation.provider_id=="specification" {"Questions presented by Specification, without extra LLM. Answer in the Pulsations view; this conversation preserves history."}else{"The pinned connection was removed or blocked. History remains readable."},
                            ))))
                        })
                        .when_some(model.canvas_authoring_load_error(cx), |panel, error| {
                            panel.child(centred(chat_column().child(
                                div().p_3().text_sm().text_color(theme::danger()).child(error),
                            )))
                        })
                        .child(
                            div()
                                .id("chat-messages-scroll")
                                .debug_selector(|| "chat-messages-scroll".into())
                                .w_full()
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scroll()
                                .flex()
                                .flex_col()
                                .items_center()
                                .child(centred(chat_column().debug_selector(|| "chat-messages-column".into()).flex().flex_col().gap_5().py_2()
                                .when(messages.is_empty(), |list| {
                                    list.child(
                                        div()
                                            .mt(px(72.0))
                                            .flex()
                                            .flex_col()
                                            .items_center()
                                            .gap_3()
                                            .child(
                                                div()
                                                    .size(px(44.0))
                                                    .rounded_full()
                                                    .bg(theme::accent_soft())
                                                    .flex()
                                                    .items_center()
                                                    .justify_center()
                                                    .child(crate::widgets::icon(
                                                        crate::icons::Icon::Sparkles,
                                                        px(20.0),
                                                        theme::accent(),
                                                    )),
                                            )
                                            .child(
                                                div()
                                                    .flex()
                                                    .items_center()
                                                    .gap_2()
                                                    .child(
                                                        div()
                                                            .text_xl()
                                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                                            .child("Chat about the real architecture of this repository."),
                                                    )
                                                    .child(crate::widgets::info_tip(
                                                        "chat-companion-scope",
                                                        "The Companion can list, search, and read safe snippets. Canvas explanations require your approval; repository source stays read-only.",
                                                    )),
                                            ),
                                    )
                                })
                                .when(hidden_messages > 0, |list| {
                                    list.child(
                                        div().flex().justify_center().child(action(
                                            "chat-show-earlier",
                                            "Show earlier messages",
                                            true,
                                            cx,
                                            |model, cx| {
                                                model.chat.update(cx, |chat, _| {
                                                    chat.history_expanded =
                                                        chat.active_conversation.clone();
                                                });
                                                cx.notify();
                                            },
                                        )),
                                    )
                                })
                                .children(messages.into_iter().map(|message| {
                                    let canvas_card = if message.role == "assistant" && message.status == "completed" {
                                        crate::canvas_authoring_ui::render_canvas_authoring_card(model, &message.id, cx)
                                    } else {
                                        None
                                    };
                                    render_message_with_canvas(message, canvas_card, cx)
                                })))),
                        )
                        .when(disclosure_required && !disclosure_accepted, |panel| {
                            panel.child(centred(
                                chat_column()
                                    .p_3()
                                    .rounded_md()
                                    .border_1()
                                    .border_color(theme::warning())
                                    .bg(theme::warning_soft())
                                    .child(if conversation.transport == ChatTransportKind::Cli {
                                        "Content requested by the Companion may be transmitted to the provider configured in this CLI. Ikore does not manage that credential."
                                    } else {
                                        "Consulted snippets from the repository will be sent to this connection's API."
                                    })
                                    .child(action(
                                        "chat-acknowledge-disclosure",
                                        "I understand and authorize this chat",
                                        !busy,
                                        cx,
                                        |model, cx| model.acknowledge_chat_disclosure(cx),
                                    )),
                            ))
                        })
                        .child(centred(
                            chat_column()
                                .debug_selector(|| "chat-composer".into())
                                .p_3()
                                .rounded_xl()
                                .border_1()
                                .border_color(theme::border_strong())
                                .bg(theme::surface_raised())
                                .shadow_md()
                                .child(draft)
                                .child(
                                    div()
                                        .debug_selector(|| "chat-composer-actions".into())
                                        .mt_2()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .gap_2()
                                        .child(composer_target)
                                        .child(crate::widgets::info_tip(
                                            "chat-composer-keys",
                                            "Enter sends. Shift+Enter adds a new line.",
                                        ))
                                        .child(if streaming {
                                            action(
                                                "chat-cancel-turn",
                                                "Cancel response",
                                                true,
                                                cx,
                                                |model, cx| model.cancel_chat_turn(cx),
                                            )
                                            .into_any_element()
                                        } else {
                                            action(
                                                "chat-send-message",
                                                if busy { "Sending…" } else { "Send" },
                                                !draft_empty
                                                    && disclosure_accepted
                                                    && !busy
                                                    && conversation.connection_available,
                                                cx,
                                                |model, cx| model.send_chat_message(cx),
                                            )
                                            .into_any_element()
                                        }),
                                ),
                        ))
                }),
        )
        .when(show_tool_activity && tool_activity_open, |layout| layout.child(
            div()
                .w(px(260.0)).flex_shrink_0()
                .min_h_0()
                .p_3()
                .border_l_1()
                .border_color(theme::border())
                .flex()
                .flex_col()
                .gap_3()
                .child(section_header("READ-ONLY", "Activity", tool_calls.len()))
                .when(tool_calls.is_empty(), |panel| {
                    panel.child(empty(
                        "Searches and file reads will appear here during generation.",
                    ))
                })
                .child(
                    div()
                        .id("chat-tools-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .children(tool_calls.into_iter().map(render_tool_call)),
                ),
        ))
                .when_some(active_model_picker, |panel, picker| {
                    panel.child(cli_model_picker_panel(model, picker, cx))
                })

        .into_any_element()
}

fn visible_api_model<'a>(
    conversation: Option<&'a ChatConversation>,
    connection_default: &'a str,
) -> &'a str {
    conversation
        .filter(|conversation| conversation.transport == ChatTransportKind::Api)
        .map(|conversation| conversation.model_id.as_str())
        .unwrap_or(connection_default)
}

fn should_show_tool_activity(
    conversation: Option<&ikore_core::chat_service::ChatConversation>,
) -> bool {
    conversation.is_none_or(|conversation| conversation.transport != ChatTransportKind::Cli)
}

pub(crate) fn render_message(message: ChatMessage, cx: &mut Context<AppModel>) -> AnyElement {
    render_message_with_canvas(message, None, cx)
}

fn render_message_with_canvas(
    message: ChatMessage,
    canvas_card: Option<AnyElement>,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let assistant = message.role == "assistant";
    let copy_text = copyable_chat_message_text(&message);
    let meta = format!(
        "{} · {}",
        message.status,
        crate::time::format_timestamp(&message.created_at)
    );
    let model_line = message_model_line(&message)
        .map(|line| (format!("chat-message-model:{}", message.id), line));
    let body = div()
        .min_w_0()
        .flex()
        .flex_col()
        .when(message.content.is_empty(), |card| {
            card.child(div().text_sm().text_color(theme::text_muted()).child(
                if matches!(message.status.as_str(), "pending" | "streaming") {
                    "Thinking…"
                } else {
                    "No content."
                },
            ))
        })
        .when(!message.content.is_empty(), |card| {
            let model = cx.entity().downgrade();
            card.child(
                SelectableMessage::new(
                    format!("chat-text:{}", message.id),
                    message.content.clone(),
                )
                .on_copy_diff(move |diff, cx| {
                    let _ = model.update(cx, |model, cx| model.copy_chat_diff(diff, cx));
                }),
            )
        })
        .when_some(message.error_message, |card, error| {
            card.child(div().mt_2().text_sm().text_color(theme::danger()).child(
                SelectableMessage::new(format!("chat-error:{}", message.id), error),
            ))
        });
    let id = message.id;
    if assistant {
        let copy_id = format!("chat-copy-message:{id}");
        div()
            .debug_selector(move || format!("chat-message:{id}"))
            .w_full()
            .flex()
            .items_start()
            .gap_3()
            .child(
                div()
                    .flex_shrink_0()
                    .size(px(28.0))
                    .rounded_full()
                    .bg(theme::accent_soft())
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(crate::widgets::icon(
                        crate::icons::Icon::Sparkles,
                        px(14.0),
                        theme::accent(),
                    )),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_xs()
                            .text_color(theme::text_muted())
                            .child(
                                div()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(theme::text())
                                    .child("Companion"),
                            )
                            .child(meta)
                            .when_some(copy_text, |header, text| {
                                header.child(compact_icon_action(
                                    SharedString::from(copy_id),
                                    crate::icons::Icon::Copy,
                                    "Copy message",
                                    theme::text_muted(),
                                    true,
                                    cx,
                                    move |_, cx| {
                                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                            text.clone(),
                                        ))
                                    },
                                ))
                            }),
                    )
                    .child(body)
                    .when_some(model_line, |message, (selector, line)| {
                        message.child(
                            div()
                                .debug_selector(move || selector)
                                .text_xs()
                                .text_color(theme::text_muted())
                                .child(line),
                        )
                    })
                    .when_some(canvas_card, |message, card| message.child(card)),
            )
            .into_any_element()
    } else {
        let copy_id = format!("chat-copy-message:{id}");
        div()
            .debug_selector(move || format!("chat-message:{id}"))
            .w_full()
            .flex()
            .flex_col()
            .items_end()
            .gap_1()
            .child(
                div()
                    .max_w(gpui::relative(0.85))
                    .min_w_0()
                    .px_4()
                    .py_3()
                    .rounded_xl()
                    .bg(theme::accent_soft())
                    .child(body),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(div().text_xs().text_color(theme::text_subtle()).child(meta))
                    .when_some(copy_text, |row, text| {
                        row.child(compact_icon_action(
                            SharedString::from(copy_id),
                            crate::icons::Icon::Copy,
                            "Copy message",
                            theme::text_muted(),
                            true,
                            cx,
                            move |_, cx| {
                                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.clone()))
                            },
                        ))
                    }),
            )
            .into_any_element()
    }
}

pub(crate) fn cli_availability_label(state: CliAvailabilityState) -> &'static str {
    match state {
        CliAvailabilityState::Available => "ready",
        CliAvailabilityState::AuthRequired => "login required",
        CliAvailabilityState::UnsupportedVersion => "unsupported version",
        CliAvailabilityState::CapabilityMissing => "adapter update required",
        CliAvailabilityState::Missing => "not installed",
        CliAvailabilityState::ProbeFailed => "probe failed",
        CliAvailabilityState::Probing => "probing",
        CliAvailabilityState::Unsupported => "unsupported",
    }
}

/// What the composer shows and lets the developer change for the next message.
struct ComposerTarget {
    model: String,
    /// API connections carry their own model; only a CLI model is picked here.
    model_selectable: bool,
    /// Effort profile and whether the selected model offers a choice. CLI only.
    effort: Option<(String, bool)>,
    route: String,
    enabled: bool,
    /// The choice waits for the running reply to end.
    deferred: bool,
}

fn composer_target_controls(target: ComposerTarget, cx: &mut Context<AppModel>) -> AnyElement {
    let enabled = target.enabled;
    div()
        .debug_selector(|| "chat-composer-target".into())
        .flex_1()
        .min_w_0()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_2()
        .child(composer_model_control(
            target.model,
            enabled && target.model_selectable,
            cx,
        ))
        .when_some(target.effort, |row, (effort, selectable)| {
            row.child(crate::select_menu::compact_control(
                "chat-composer-profile",
                "Effort",
                effort,
                enabled && selectable,
                cx,
            ))
        })
        .child(crate::select_menu::compact_control(
            "chat-composer-route",
            "Route",
            target.route,
            enabled,
            cx,
        ))
        .when(target.deferred, |row| {
            row.child(
                div()
                    .debug_selector(|| "chat-composer-target-deferred".into())
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child("Applies to the next message"),
            )
        })
        .into_any_element()
}

/// Opens the Companion model picker. It matches `select_menu::compact_control`,
/// whose menus cannot hold the picker's catalog, search and manual entry.
fn composer_model_control(model: String, enabled: bool, cx: &mut Context<AppModel>) -> AnyElement {
    crate::widgets::button_frame(
        div()
            .id("chat-composer-model")
            .debug_selector(|| "chat-composer-model".into())
            .tab_index(0),
        crate::widgets::ButtonVariant::Secondary,
        enabled,
    )
    .gap_2()
    .child(div().text_color(theme::text_muted()).child("Model:"))
    .child(model)
    .when(enabled, |control| {
        control
            .child(crate::widgets::icon(
                crate::icons::Icon::ChevronDown,
                px(12.0),
                theme::text_muted(),
            ))
            .on_click(cx.listener(|model, _, _, cx| model.open_companion_model_picker(cx)))
            .on_key_down(cx.listener(|model, event: &gpui::KeyDownEvent, _, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space" | "down") {
                    model.open_companion_model_picker(cx);
                    cx.stop_propagation();
                }
            }))
    })
    .into_any_element()
}

/// `model · effort · route` of a stored conversation, for a composer that
/// shows the target without changing it.
pub(crate) fn conversation_target_line(conversation: &ChatConversation) -> String {
    let mut parts = vec![if conversation.model_id == "default" {
        "CLI default".to_owned()
    } else {
        conversation.model_id.clone()
    }];
    if let Some(profile) = &conversation.model_profile {
        parts.push(profile.clone());
    }
    parts.push(match (conversation.transport, conversation.cli_backend) {
        (ChatTransportKind::Cli, Some(backend)) => format!("{} CLI", backend.label()),
        (ChatTransportKind::Cli, None) => "CLI".into(),
        (ChatTransportKind::Api, _) => format!("{} API", conversation.provider_id),
    });
    parts.join(" · ")
}

/// One line under an assistant answer: the model that served it, the effort
/// profile, the route, and how far the CLI output confirmed the model.
///
/// A turn that never reached a model call keeps the label frozen at send time.
pub(crate) fn message_model_line(message: &ChatMessage) -> Option<String> {
    let Some(evidence) = &message.model_evidence else {
        return message
            .target_label
            .as_ref()
            .map(|target| format!("requested: {target}"));
    };
    let mut parts = vec![match &evidence.observed_model {
        Some(observed) => observed.clone(),
        None => format!("requested {}", evidence.requested_model),
    }];
    if let Some(profile) = &evidence.model_profile {
        parts.push(profile.clone());
    }
    parts.push(match evidence.transport {
        ChatTransportKind::Cli => format!(
            "{} CLI",
            CliBackend::parse(&evidence.provider_id)
                .map_or(evidence.provider_id.as_str(), |backend| backend.label())
        ),
        ChatTransportKind::Api => format!("{} API", evidence.provider_id),
    });
    let settled = !matches!(message.status.as_str(), "pending" | "streaming");
    if settled && evidence.transport == ChatTransportKind::Cli {
        parts.push(cli_model_confirmation(evidence));
    }
    Some(parts.join(" · "))
}

fn cli_model_confirmation(evidence: &ChatMessageModelEvidence) -> String {
    match (evidence.verification, &evidence.observed_model) {
        (ChatModelVerification::Verified, _) => "verified".into(),
        (ChatModelVerification::FamilyVerified, _) => {
            format!("family match for {}", evidence.requested_model)
        }
        // No specific model was requested, so the CLI picked its own default.
        (ChatModelVerification::Unverified, Some(_)) if evidence.requested_model == "default" => {
            "chosen by the CLI".into()
        }
        // A model was reported, but not by every completed call of the turn.
        (ChatModelVerification::Unverified, Some(_)) => "partly confirmed".into(),
        (ChatModelVerification::Unverified, None) => "model not confirmed".into(),
    }
}

fn copyable_chat_message_text(message: &ChatMessage) -> Option<String> {
    let content = (!message.content.trim().is_empty()).then_some(message.content.as_str());
    let error = message
        .error_message
        .as_deref()
        .filter(|error| !error.trim().is_empty());
    match (content, error) {
        (Some(content), Some(error)) => Some(format!("{content}\n\n{error}")),
        (Some(content), None) => Some(content.to_owned()),
        (None, Some(error)) => Some(error.to_owned()),
        (None, None) => None,
    }
}

/// The conversation's reading width: header, messages and composer line up on it, centred in the
/// pane like a chat app, however wide the window is. Always place it through [`centred`].
fn chat_column() -> gpui::Div {
    div().w_full().max_w(px(CHAT_COLUMN_WIDTH)).min_w_0()
}

/// Centres a [`chat_column`] in its pane. The flex row resolves the column's clamped width before
/// its height is measured: placed straight in the pane, wrapped text was measured at the pane's
/// full width but painted at the column's, so a long draft pushed Send out through the composer
/// and the end of a long answer could never be scrolled into view.
fn centred(column: gpui::Div) -> gpui::Div {
    div()
        .w_full()
        .flex_shrink_0()
        .flex()
        .justify_center()
        .child(column)
}

const CHAT_COLUMN_WIDTH: f32 = 760.0;

fn render_tool_call(tool: ChatToolCall) -> impl IntoElement {
    let label = tool_label(&tool.tool_name);
    let arguments = summarize_arguments(&tool);
    let status = tool.status.clone();
    div()
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(if tool.status == "failed" {
            theme::danger()
        } else {
            theme::border()
        })
        .bg(theme::surface_raised())
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(label)
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(status),
                ),
        )
        .child(
            div()
                .mt_1()
                .font_family(theme::FONT_MONO)
                .text_xs()
                .child(arguments),
        )
        .when_some(tool.result_summary, |card, summary| {
            card.child(div().mt_1().text_sm().child(summary))
        })
        .when_some(tool.error_message, |card, error| {
            card.child(
                div()
                    .mt_1()
                    .text_sm()
                    .text_color(theme::danger())
                    .child(error),
            )
        })
        .child(
            div()
                .mt_1()
                .text_xs()
                .text_color(theme::text_muted())
                .child(format!(
                    "{} bytes · round {}",
                    tool.result_bytes,
                    tool.round_index + 1
                )),
        )
}

fn summarize_arguments(tool: &ChatToolCall) -> String {
    let display = |key: &str| match tool.sanitized_arguments.get(key) {
        Some(ChatToolArgumentValue::Text(value)) => Some(value.clone()),
        Some(ChatToolArgumentValue::Integer(value)) => Some(value.to_string()),
        Some(ChatToolArgumentValue::Boolean(value)) => Some(value.to_string()),
        None => None,
    };
    let path = display("path").unwrap_or_else(|| ".".into());
    display("query")
        .map(|query| format!("{path} · “{query}”"))
        .unwrap_or(path)
}

fn tool_label(name: &str) -> String {
    match name {
        "list_repository_tree" => "Listing tree".into(),
        "search_repository" => "Searching repository".into(),
        "read_repository_file" => "Reading file".into(),
        _ => name.to_string(),
    }
}

fn section_header(kicker: &'static str, title: &'static str, count: usize) -> impl IntoElement {
    crate::widgets::section_header(kicker, title, count)
}

fn compact_icon_action(
    id: SharedString,
    glyph: crate::icons::Icon,
    tooltip: &'static str,
    ink: gpui::Rgba,
    enabled: bool,
    cx: &mut Context<AppModel>,
    handler: impl Fn(&mut AppModel, &mut Context<AppModel>) + 'static,
) -> AnyElement {
    let debug_id = id.clone();
    div()
        .id(id)
        .debug_selector(move || debug_id.to_string())
        .flex_shrink_0()
        .size(px(30.0))
        .rounded_md()
        .flex()
        .items_center()
        .justify_center()
        .opacity(if enabled { 1.0 } else { 0.45 })
        .tab_index(0)
        .focus_visible(|style| style.border_1().border_color(ink))
        .tooltip(crate::widgets::text_tooltip(tooltip))
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .hover(|style| style.bg(theme::surface_hover()))
                .on_click(cx.listener(move |model, _, _, cx| handler(model, cx)))
        })
        .child(crate::widgets::icon(glyph, px(15.0), ink))
        .into_any_element()
}

fn action(
    id: impl Into<SharedString>,
    label: &'static str,
    enabled: bool,
    cx: &mut Context<AppModel>,
    handler: impl Fn(&mut AppModel, &mut Context<AppModel>) + 'static,
) -> AnyElement {
    use crate::widgets::ButtonVariant;
    let id: SharedString = id.into();
    // Sending leads; cancelling is danger; starting a conversation is tinted;
    // panel toggles and copy stay secondary.
    let variant = if id.as_ref() == "chat-send-message" {
        ButtonVariant::Primary
    } else if id.as_ref() == "chat-cancel-turn" {
        ButtonVariant::Danger
    } else if id.as_ref() == "chat-new-conversation" {
        ButtonVariant::Tinted
    } else {
        ButtonVariant::Secondary
    };
    let glyph = crate::widgets::action_icon(&id);
    crate::widgets::button_frame(div().id(id).mt_2(), variant, enabled)
        .when(enabled, |button| {
            button.on_click(cx.listener(move |model, _, _, cx| handler(model, cx)))
        })
        .gap_2()
        .when_some(glyph, |button, glyph| {
            button.child(crate::widgets::icon(
                glyph,
                px(14.0),
                crate::widgets::button_ink(variant, enabled),
            ))
        })
        .child(label)
        .into_any_element()
}

fn selection(
    id: impl Into<gpui::ElementId>,
    label: &'static str,
    value: impl Into<SharedString>,
    enabled: bool,
    cx: &mut Context<AppModel>,
    handler: impl Fn(&mut AppModel, &mut Context<AppModel>) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .mt_2()
        .p_2()
        .rounded_md()
        .border_1()
        .border_color(theme::border())
        .bg(theme::surface_raised())
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .hover(|style| style.border_color(theme::accent()))
                .on_click(cx.listener(move |model, _, _, cx| handler(model, cx)))
        })
        .child(div().text_xs().text_color(theme::text_muted()).child(label))
        .child(div().text_sm().child(value.into()))
}

fn warning(message: &'static str) -> impl IntoElement {
    div()
        .p_3()
        .rounded_md()
        .bg(theme::warning_soft())
        .text_sm()
        .text_color(theme::warning())
        .child(message)
}

fn empty(message: &'static str) -> impl IntoElement {
    div()
        .p_3()
        .text_sm()
        .text_color(theme::text_muted())
        .child(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ikore_core::terminals::CliModelSelectionSource;

    fn message(role: &str, content: &str, error: Option<&str>) -> ChatMessage {
        ChatMessage {
            id: "message".into(),
            conversation_id: "conversation".into(),
            ordinal: 0,
            role: role.into(),
            content: content.into(),
            status: "completed".into(),
            execution_id: None,
            generation_id: None,
            last_sequence: 0,
            input_tokens: None,
            output_tokens: None,
            estimated_cost_usd: None,
            error_code: None,
            error_message: error.map(str::to_owned),
            target_label: None,
            model_evidence: None,
            created_at: "now".into(),
            updated_at: "now".into(),
        }
    }

    fn cli_evidence(
        backend: &str,
        requested: &str,
        profile: Option<&str>,
        observed: Option<&str>,
        verification: ChatModelVerification,
    ) -> ChatMessageModelEvidence {
        ChatMessageModelEvidence {
            transport: ChatTransportKind::Cli,
            provider_id: backend.into(),
            requested_model: requested.into(),
            model_profile: profile.map(str::to_owned),
            observed_model: observed.map(str::to_owned),
            verification,
        }
    }

    fn model_line(status: &str, evidence: ChatMessageModelEvidence) -> String {
        let mut message = message("assistant", "Answer", None);
        message.status = status.into();
        message.model_evidence = Some(evidence);
        message_model_line(&message).unwrap()
    }

    #[test]
    fn model_line_names_the_observed_model_the_route_and_the_evidence() {
        assert_eq!(
            model_line(
                "completed",
                cli_evidence(
                    "codex",
                    "gpt-5.6-luna",
                    Some("low"),
                    Some("gpt-5.6-luna"),
                    ChatModelVerification::Verified,
                ),
            ),
            "gpt-5.6-luna · low · Codex CLI · verified"
        );
        assert_eq!(
            model_line(
                "completed",
                cli_evidence(
                    "claude",
                    "sonnet",
                    Some("high"),
                    Some("claude-sonnet-5-5"),
                    ChatModelVerification::FamilyVerified,
                ),
            ),
            "claude-sonnet-5-5 · high · Claude CLI · family match for sonnet"
        );
        assert_eq!(
            model_line(
                "completed",
                cli_evidence(
                    "gemini",
                    "default",
                    None,
                    Some("gemini-3-pro"),
                    ChatModelVerification::Unverified,
                ),
            ),
            "gemini-3-pro · Gemini CLI · chosen by the CLI"
        );
        assert_eq!(
            model_line(
                "failed",
                cli_evidence(
                    "opencode",
                    "glm-5",
                    None,
                    None,
                    ChatModelVerification::Unverified
                ),
            ),
            "requested glm-5 · OpenCode CLI · model not confirmed"
        );
    }

    #[test]
    fn model_line_claims_no_evidence_for_running_or_api_turns() {
        assert_eq!(
            model_line(
                "streaming",
                cli_evidence(
                    "claude",
                    "sonnet",
                    Some("high"),
                    None,
                    ChatModelVerification::Unverified,
                ),
            ),
            "requested sonnet · high · Claude CLI"
        );
        assert_eq!(
            model_line(
                "completed",
                ChatMessageModelEvidence {
                    transport: ChatTransportKind::Api,
                    provider_id: "openai".into(),
                    requested_model: "gpt-4.1-mini".into(),
                    model_profile: None,
                    observed_model: None,
                    verification: ChatModelVerification::Unverified,
                },
            ),
            "requested gpt-4.1-mini · openai API"
        );
    }

    #[test]
    fn model_line_does_not_overstate_a_partly_confirmed_turn() {
        assert_eq!(
            model_line(
                "completed",
                cli_evidence(
                    "claude",
                    "claude-sonnet-5-5",
                    None,
                    Some("claude-sonnet-5-5"),
                    ChatModelVerification::Unverified,
                ),
            ),
            "claude-sonnet-5-5 · Claude CLI · partly confirmed"
        );
    }

    #[test]
    fn model_line_falls_back_to_the_frozen_target_without_model_calls() {
        let mut pending = message("assistant", "", None);
        assert_eq!(message_model_line(&pending), None);
        pending.target_label = Some("claude / sonnet / high".into());
        assert_eq!(
            message_model_line(&pending).as_deref(),
            Some("requested: claude / sonnet / high")
        );
    }

    #[test]
    fn message_copy_includes_user_assistant_and_error_text() {
        assert_eq!(
            copyable_chat_message_text(&message("user", "Question", None)).as_deref(),
            Some("Question")
        );
        assert_eq!(
            copyable_chat_message_text(&message("assistant", "Answer", None)).as_deref(),
            Some("Answer")
        );
        assert_eq!(
            copyable_chat_message_text(&message("assistant", "", Some("CLI failed"))).as_deref(),
            Some("CLI failed")
        );
        assert_eq!(
            copyable_chat_message_text(&message("assistant", "Partial", Some("CLI failed")))
                .as_deref(),
            Some("Partial\n\nCLI failed")
        );
        assert!(copyable_chat_message_text(&message("assistant", "  ", Some("  "))).is_none());
    }

    #[test]
    fn renders_safe_markdown_with_copy_only_diff() {
        let content = "Safe<script>alert(1)</script>\n\n```diff\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-a\n+b\n```";
        let parts = parse_chat_content(content);
        let rendered = format!("{parts:?}");
        assert!(!format!("{:?}", parse_markdown(content)).contains("alert"));
        assert!(rendered.contains("UnifiedDiff"));
        assert!(rendered.contains("src/a.rs"));
        assert!(!rendered.to_ascii_lowercase().contains("apply"));
    }

    #[test]
    fn cli_conversations_never_render_internal_tool_activity() {
        let conversation = ikore_core::chat_service::ChatConversation {
            id: "cli-chat".into(),
            project_id: "project".into(),
            connection_id: None,
            provider_id: "claude".into(),
            model_id: "sonnet".into(),
            transport: ChatTransportKind::Cli,
            cli_backend: Some(ikore_core::terminals::CliBackend::Claude),
            model_profile: Some("high".into()),
            model_source: CliModelSelectionSource::Catalog,
            title: "CLI chat".into(),
            connection_available: true,
            created_at: "now".into(),
            updated_at: "now".into(),
        };
        assert!(!should_show_tool_activity(Some(&conversation)));
        assert!(should_show_tool_activity(None));
        assert_eq!(
            conversation_target_line(&conversation),
            "sonnet · high · Claude CLI"
        );
        assert_eq!(
            visible_api_model(Some(&conversation), "configured"),
            "configured"
        );
        let mut api_conversation = conversation;
        api_conversation.transport = ChatTransportKind::Api;
        api_conversation.model_id = "frozen-model".into();
        assert_eq!(
            visible_api_model(Some(&api_conversation), "changed-default"),
            "frozen-model"
        );
    }
}
