use gpui::{AnyElement, Context, IntoElement, SharedString, div, prelude::*, px};
use hvc_core::chat_service::{ChatMessage, ChatToolArgumentValue, ChatToolCall, ChatTransportKind};
use hvc_core::terminals::{CliAvailabilityState, CliModelSelectionSource};
use hvc_editor::{ChatContentPart, parse_chat_content, parse_markdown};

use crate::agents::CliModelPickerTarget;
use crate::agents_view::cli_model_picker_panel;
use crate::app_model::AppModel;
use crate::theme;

use crate::workspace_view::render_markdown_block;

pub(crate) fn chat_panel(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let (
        project_id,
        connections,
        conversations,
        active_conversation,
        messages,
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
            chat.active_messages().to_vec(),
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
                    .border_1()
                    .border_color(theme::border())
                    .bg(theme::surface())
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
    let show_tool_activity = should_show_tool_activity(active_conversation.as_ref())
        && (active_conversation.is_some() || selected_transport == ChatTransportKind::Api);
    let cli_model_label = match selected_cli_model_source {
        CliModelSelectionSource::Manual => format!(
            "{} · unverified",
            selected_cli_model.as_deref().unwrap_or("CLI default")
        ),
        _ => selected_cli_model
            .as_deref()
            .unwrap_or("CLI default")
            .to_string(),
    };
    let cli_profile_label = selected_cli_model_profile
        .clone()
        .unwrap_or_else(|| "CLI default".into());

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
                .rounded_lg()
                .border_1()
                .border_color(theme::border())
                .bg(theme::surface())
                .flex()
                .flex_col()
                .gap_3()
                .child(section_header("ACTIVE PROJECT", "Conversations", conversations.len()))
                .child({

                    let transport_label = match selected_transport {
                        ChatTransportKind::Api => "API",
                        ChatTransportKind::Cli => "CLI",
                    };
                    div()
                        .child(crate::select_menu::control("chat-transport","TRANSPORT",transport_label,!busy && !streaming,cx))

                })
                .child({
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
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(
                                    if selected_transport == ChatTransportKind::Api {
                                        selected_connection
                                            .map(|connection| {
                                                format!(
                                                    "{} · {}",
                                                    connection.display_name,
                                                    connection.default_model
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
                                                    match item.state {
                                                        CliAvailabilityState::Available => "ready",
                                                        CliAvailabilityState::AuthRequired => "login required",
                                                        CliAvailabilityState::UnsupportedVersion => "unsupported version",
                                                        CliAvailabilityState::CapabilityMissing => "adapter update required",
                                                        CliAvailabilityState::Missing => "not installed",
                                                        CliAvailabilityState::ProbeFailed => "probe failed",
                                                        CliAvailabilityState::Probing => "probing",
                                                        CliAvailabilityState::Unsupported => "unsupported",
                                                    }
                                                )
                                            })
                                            .unwrap_or_else(|| "CLI not detected".into())
                                    },
                                ),
                        );

                    if selected_transport == ChatTransportKind::Api && connections.len() > 1 {

                        let conn_wrapper = div().mt_2().child(crate::select_menu::control(
                            "chat-cycle-connection", "Connection", selected_connection.map(|c| c.display_name.clone()).unwrap_or_default(), !busy && !streaming, cx,
                        ));

                        auth_card = auth_card.child(conn_wrapper);
                    } else if selected_transport == ChatTransportKind::Cli {



                        let profiles = model.model_profiles(
                            selected_cli_backend,
                            selected_cli_model.as_deref(),
                            cx,
                        );


                        let backend_div = div().child(crate::select_menu::control("chat-cli-backend","CLI BACKEND",selected_cli_backend.label(),!busy && !streaming,cx));


                        let profile_div = div().child(crate::select_menu::control("chat-cli-profile","PROFILE",cli_profile_label,!busy && !streaming && selected_cli_model.is_some() && !profiles.is_empty(),cx));


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
                                        !busy && !streaming,
                                        cx,
                                        |model, cx| model.open_companion_model_picker(cx),
                                    ))
                                    .child(profile_div),
                            )
                            .child(
                                div()
                                    .mt_2()
                                    .text_xs()
                                    .text_color(theme::text_muted())
                                    .child("Uses the CLI's existing login. HVC does not request or store its API key."),
                            );
                    }

                    auth_card
                })
                .child(action(
                    "chat-new-conversation",
                    if busy { "Please wait…" } else { "New conversation" },
                    new_target_available && !busy && !streaming,
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
                            let awaiting_confirmation = pending_delete
                                .as_ref()
                                .is_some_and(|pending| pending.as_ref() == id);
                            div()
                                .id(SharedString::from(format!("chat-conversation:{id}")))
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
                                .child(
                                    div()
                                        .id(SharedString::from(format!("chat-select:{id}")))
                                        .cursor_pointer()
                                        .on_click(cx.listener(move |model, _, _, cx| {
                                            model.select_chat_conversation(id.clone(), cx)
                                        }))
                                        .child(
                                             div()
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .child(conversation.title),
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
                                )
                                .child(action(
                                    SharedString::from(format!("chat-delete:{delete_id}")),
                                    if awaiting_confirmation {
                                        "Confirm deletion"
                                    } else {
                                        "Delete"
                                    },
                                    !busy && !streaming,
                                    cx,
                                    move |model, cx| {
                                        model.delete_chat_conversation(delete_id.clone(), cx)
                                    },
                                ))
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
                .border_1()
                .border_color(theme::border())
                .bg(theme::surface())
                .flex()
                .flex_col()
                .gap_3()
                .when(show_tool_activity, |panel| panel.child(action(
                    "chat-toggle-activity", if tool_activity_open { "Hide activity" } else { "Show activity" }, true, cx,
                    |model, cx| { model.chat.update(cx, |chat, _| chat.tool_activity_open = !chat.tool_activity_open); cx.notify(); }
                )))

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
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .items_start()
                                .child(
                                    div()
                                        .w_full()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(theme::accent())
                                                .child("CHAT / COMPANION"),
                                        )
                                        .child(
                                            div()
                                                .text_xl()
                                                .whitespace_normal()
                                                .font_weight(gpui::FontWeight::BOLD)
                                                .child(conversation.title.clone()),
                                        ),
                                )
                                .child(
                                    div()
                                        .w_full()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .text_xs()
                                        .child(conversation.provider_id.clone())
                                        .child(
                                            div()
                                                .font_family(theme::FONT_MONO)
                                                .text_color(theme::text_muted())
                                                .child(conversation.model_id.clone()),
                                        ),
                                ),
                        )
                        .when(!conversation.connection_available, |panel| {
                            panel.child(warning(
                                if conversation.provider_id=="specification" {"Questions presented by Specification, without extra LLM. Answer in the Pulsations view; this conversation preserves history."}else{"The pinned connection was removed or blocked. History remains readable."},
                            ))
                        })
                        .child(
                            div()
                                .id("chat-messages-scroll")
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scroll()
                                .flex()
                                .flex_col()
                                .gap_3()
                                .when(messages.is_empty(), |list| {
                                    list.child(
                                        div()
                                            .p_4()
                                            .rounded_lg()
                                            .bg(theme::accent_soft())
                                            .child(
                                                div()
                                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                                    .child("Chat about the real architecture of this repository."),
                                            )
                                            .child(
                                                div()
                                                    .mt_2()
                                                    .text_sm()
                                                    .text_color(theme::text_muted())
                                                    .child("The Companion can list, search, and read safe snippets. It cannot apply diffs or modify files."),
                                            ),
                                    )
                                })
                                .children(messages.into_iter().map(|message| {
                                    render_message(message, cx)
                                })),
                        )
                        .when(disclosure_required && !disclosure_accepted, |panel| {
                            panel.child(
                                div()
                                    .p_3()
                                    .rounded_md()
                                    .border_1()
                                    .border_color(theme::warning())
                                    .bg(theme::warning_soft())
                                    .child(if conversation.transport == ChatTransportKind::Cli {
                                        "Content requested by the Companion may be transmitted to the provider configured in this CLI. HVC does not manage that credential."
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
                            )
                        })
                        .child(
                            div()
                                .debug_selector(|| "chat-composer".into())
                                .flex_shrink_0()
                                .p_3()
                                .rounded_lg()
                                .border_1()
                                .border_color(theme::border())
                                .bg(theme::surface_raised())
                                .child(draft)
                                .child(
                                    div()
                                        .mt_2()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(theme::text_muted())
                                                .child("Enter sends · Shift+Enter adds a new line"),
                                        )
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
                        )
                }),
        )
        .when(show_tool_activity && tool_activity_open, |layout| layout.child(
            div()
                .w(px(260.0)).flex_shrink_0()
                .min_h_0()
                .p_3()
                .rounded_lg()
                .border_1()
                .border_color(theme::border())
                .bg(theme::surface())
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

fn should_show_tool_activity(
    conversation: Option<&hvc_core::chat_service::ChatConversation>,
) -> bool {
    conversation.is_none_or(|conversation| conversation.transport != ChatTransportKind::Cli)
}

fn render_message(message: ChatMessage, cx: &mut Context<AppModel>) -> AnyElement {
    let assistant = message.role == "assistant";
    div()
        .p_3()
        .rounded_lg()
        .border_1()
        .border_color(if assistant {
            theme::border()
        } else {
            theme::accent()
        })
        .bg(if assistant {
            theme::surface_raised()
        } else {
            theme::accent_soft()
        })
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(if assistant { "Companion" } else { "You" })
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(format!(
                            "{} · {}",
                            message.status,
                            crate::time::format_timestamp(&message.created_at)
                        )),
                ),
        )
        .when(message.content.is_empty(), |card| {
            card.child(
                div()
                    .mt_2()
                    .text_sm()
                    .text_color(theme::text_muted())
                    .child(
                        if matches!(message.status.as_str(), "pending" | "streaming") {
                            "Thinking…"
                        } else {
                            "No content."
                        },
                    ),
            )
        })
        .when(!message.content.is_empty(), |card| {
            card.child(
                div()
                    .mt_2()
                    .children(render_chat_content(&message.content, cx)),
            )
        })
        .when_some(message.error_message, |card, error| {
            card.child(
                div()
                    .mt_2()
                    .text_sm()
                    .text_color(theme::danger())
                    .child(error),
            )
        })
        .into_any_element()
}

fn render_chat_content(content: &str, cx: &mut Context<AppModel>) -> Vec<AnyElement> {
    parse_chat_content(content)
        .into_iter()
        .flat_map(|part| match part {
            ChatContentPart::Markdown(markdown) => parse_markdown(&markdown)
                .into_iter()
                .map(render_markdown_block)
                .collect::<Vec<_>>(),
            ChatContentPart::UnifiedDiff { content, file_path } => {
                let copy = content.clone();
                vec![
                    div()
                        .my_2()
                        .p_3()
                        .rounded_md()
                        .border_1()
                        .border_color(theme::warning())
                        .bg(theme::canvas())
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child(file_path),
                                )
                                .child(action(
                                    SharedString::from(format!(
                                        "chat-copy-diff:{}",
                                        stable_text_id(&copy)
                                    )),
                                    "Copy diff",
                                    true,
                                    cx,
                                    move |model, cx| model.copy_chat_diff(copy.clone(), cx),
                                )),
                        )
                        .child(
                            div()
                                .mt_2()
                                .font_family(theme::FONT_MONO)
                                .text_xs()
                                .child(content),
                        )
                        .child(
                            div()
                                .mt_2()
                                .text_xs()
                                .text_color(theme::warning())
                                .child("Proposed diff not applied · apply action does not exist"),
                        )
                        .into_any_element(),
                ]
            }
        })
        .collect()
}

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
    div()
        .flex()
        .items_end()
        .justify_between()
        .child(
            div()
                .child(div().text_xs().text_color(theme::accent()).child(kicker))
                .child(
                    div()
                        .text_lg()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(title),
                ),
        )
        .child(
            div()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(theme::surface_raised())
                .text_xs()
                .child(count.to_string()),
        )
}

fn action(
    id: impl Into<gpui::ElementId>,
    label: &'static str,
    enabled: bool,
    cx: &mut Context<AppModel>,
    handler: impl Fn(&mut AppModel, &mut Context<AppModel>) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .mt_2()
        .px_3()
        .py_2()
        .rounded_md()
        .border_1()
        .border_color(if enabled {
            theme::accent()
        } else {
            theme::border()
        })
        .text_sm()
        .text_color(if enabled {
            theme::text()
        } else {
            theme::text_muted()
        })
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .hover(|style| style.bg(theme::accent_soft()))
                .on_click(cx.listener(move |model, _, _, cx| handler(model, cx)))
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

fn stable_text_id(text: &str) -> u64 {
    text.bytes().fold(1_469_598_103_934_665_603, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(1_099_511_628_211)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let conversation = hvc_core::chat_service::ChatConversation {
            id: "cli-chat".into(),
            project_id: "project".into(),
            connection_id: None,
            provider_id: "claude".into(),
            model_id: "sonnet".into(),
            transport: ChatTransportKind::Cli,
            cli_backend: Some(hvc_core::terminals::CliBackend::Claude),
            model_profile: Some("high".into()),
            model_source: CliModelSelectionSource::Catalog,
            title: "CLI chat".into(),
            connection_available: true,
            created_at: "now".into(),
            updated_at: "now".into(),
        };
        assert!(!should_show_tool_activity(Some(&conversation)));
        assert!(should_show_tool_activity(None));
    }
}
