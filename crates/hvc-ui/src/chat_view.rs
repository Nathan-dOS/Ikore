use gpui::{AnyElement, Context, IntoElement, SharedString, div, prelude::*, px};
use hvc_core::chat_service::{ChatMessage, ChatToolArgumentValue, ChatToolCall};
use hvc_editor::{ChatContentPart, parse_chat_content, parse_markdown};

use crate::app_model::AppModel;
use crate::shell::render_markdown_block;
use crate::theme;

pub(crate) fn chat_panel(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let (
        project_id,
        connections,
        conversations,
        active_conversation,
        messages,
        tool_calls,
        selected_connection_id,
        streaming,
        loading,
        busy,
        error,
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
                            .child("Abra um projeto para conversar"),
                    )
                    .child(
                        div()
                            .mt_2()
                            .text_sm()
                            .text_color(theme::text_muted())
                            .child(
                                "O Companion pertence ao workspace ativo e nunca acessa outro repositório.",
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

    div()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .p_3()
        .flex()
        .gap_3()
        .child(
            div()
                .w(px(280.0))
                .min_h_0()
                .p_3()
                .rounded_lg()
                .border_1()
                .border_color(theme::border())
                .bg(theme::surface())
                .flex()
                .flex_col()
                .gap_3()
                .child(section_header("PROJETO ATIVO", "Conversas", conversations.len()))
                .child(
                    div()
                        .p_3()
                        .rounded_md()
                        .bg(theme::surface_raised())
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::text_muted())
                                .child("CONEXÃO COMPATÍVEL"),
                        )
                        .child(
                            div()
                                .mt_1()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(
                                    selected_connection
                                        .map(|connection| {
                                            format!(
                                                "{} · {}",
                                                connection.display_name, connection.default_model
                                            )
                                        })
                                        .unwrap_or_else(|| "Nenhuma disponível".into()),
                                ),
                        )
                        .when(connections.len() > 1, |card| {
                            card.child(action(
                                "chat-cycle-connection",
                                "Alternar conexão",
                                !busy,
                                cx,
                                |model, cx| model.cycle_chat_connection(cx),
                            ))
                        }),
                )
                .child(action(
                    "chat-new-conversation",
                    if busy { "Aguarde…" } else { "Nova conversa" },
                    selected_connection.is_some() && !busy,
                    cx,
                    |model, cx| model.create_chat_conversation(cx),
                ))
                .when(connections.is_empty(), |panel| {
                    panel.child(warning(
                        "Configure em Settings uma conexão habilitada, credenciada e com tool calling.",
                    ))
                })
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
                            list.child(empty("Nenhuma conversa neste projeto."))
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
                                                    "{} · {}",
                                                    conversation.provider_id, conversation.model_id
                                                )),
                                        ),
                                )
                                .child(action(
                                    SharedString::from(format!("chat-delete:{delete_id}")),
                                    if awaiting_confirmation {
                                        "Confirmar exclusão"
                                    } else {
                                        "Excluir"
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
                .when_some(error, |panel, error| {
                    panel.child(
                        div()
                            .p_3()
                            .rounded_md()
                            .bg(theme::danger_soft())
                            .text_color(theme::danger())
                            .child(error.presentable()),
                    )
                })
                .when(active_conversation.is_none(), |panel| {
                    panel.child(
                        div()
                            .flex_1()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(if loading {
                                "Carregando conversas…"
                            } else {
                                "Escolha uma conexão compatível e inicie uma conversa."
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
                                                .font_family("Cascadia Mono")
                                                .text_color(theme::text_muted())
                                                .child(conversation.model_id.clone()),
                                        ),
                                ),
                        )
                        .when(!conversation.connection_available, |panel| {
                            panel.child(warning(
                                if conversation.provider_id=="specification" {"Perguntas apresentadas pela Specification, sem LLM extra. Responda na view Pulsations; esta conversa preserva o histórico."}else{"A conexão fixada foi removida ou bloqueada. O histórico permanece legível."},
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
                                                    .child("Converse sobre a arquitetura real deste repositório."),
                                            )
                                            .child(
                                                div()
                                                    .mt_2()
                                                    .text_sm()
                                                    .text_color(theme::text_muted())
                                                    .child("O Companion pode listar, buscar e ler trechos seguros. Não pode aplicar diffs nem alterar arquivos."),
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
                                    .child("Trechos consultados do repositório serão enviados à API desta conexão.")
                                    .child(action(
                                        "chat-acknowledge-disclosure",
                                        "Entendi e autorizo este chat",
                                        !busy,
                                        cx,
                                        |model, cx| model.acknowledge_chat_disclosure(cx),
                                    )),
                            )
                        })
                        .child(
                            div()
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
                                                .child("Somente leitura · sem shell, web, Git ou escrita"),
                                        )
                                        .child(if streaming {
                                            action(
                                                "chat-cancel-turn",
                                                "Cancelar resposta",
                                                true,
                                                cx,
                                                |model, cx| model.cancel_chat_turn(cx),
                                            )
                                            .into_any_element()
                                        } else {
                                            action(
                                                "chat-send-message",
                                                if busy { "Enviando…" } else { "Enviar" },
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
        .child(
            div()
                .w(px(280.0))
                .min_h_0()
                .p_3()
                .rounded_lg()
                .border_1()
                .border_color(theme::border())
                .bg(theme::surface())
                .flex()
                .flex_col()
                .gap_3()
                .child(section_header("READ-ONLY", "Atividade", tool_calls.len()))
                .when(tool_calls.is_empty(), |panel| {
                    panel.child(empty(
                        "As buscas e leituras aparecerão aqui durante a resposta.",
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
        )
        .into_any_element()
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
                .child(if assistant { "Companion" } else { "Você" })
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(format!("{} · {}", message.status, message.created_at)),
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
                            "Pensando…"
                        } else {
                            "Sem conteúdo."
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
                                    "Copiar diff",
                                    true,
                                    cx,
                                    move |model, cx| model.copy_chat_diff(copy.clone(), cx),
                                )),
                        )
                        .child(
                            div()
                                .mt_2()
                                .font_family("Cascadia Mono")
                                .text_xs()
                                .child(content),
                        )
                        .child(
                            div()
                                .mt_2()
                                .text_xs()
                                .text_color(theme::warning())
                                .child("Proposta não aplicada · não existe ação de aplicar"),
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
                .font_family("Cascadia Mono")
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
                    "{} bytes · rodada {}",
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
        "list_repository_tree" => "Listando árvore".into(),
        "search_repository" => "Buscando no repositório".into(),
        "read_repository_file" => "Lendo arquivo".into(),
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
) -> impl IntoElement {
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
        let content = "Seguro<script>alert(1)</script>\n\n```diff\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-a\n+b\n```";
        let parts = parse_chat_content(content);
        let rendered = format!("{parts:?}");
        assert!(!format!("{:?}", parse_markdown(content)).contains("alert"));
        assert!(rendered.contains("UnifiedDiff"));
        assert!(rendered.contains("src/a.rs"));
        assert!(!rendered.to_ascii_lowercase().contains("apply"));
    }
}
