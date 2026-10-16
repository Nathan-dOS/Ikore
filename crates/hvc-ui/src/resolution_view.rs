//! Resolution modal (ADR-022 clarification loop, P4/A4.1): a stack of chips,
//! one per open Specification clarification checkpoint across the whole
//! project, and an accordion conversation with the Companion used to turn a
//! question into a confirmable `FILES_NEW:`/`FILES_MOD:`/`FILES:` answer.
use crate::sizing::{StyledExt, text_area};
use crate::widgets::{ButtonVariant, btn, empty_state};
use crate::{AppModel, theme};
use gpui::{AnyElement, Context, deferred, div, prelude::*, px, relative};

/// The floating chip stack, always visible whenever a clarification
/// checkpoint is open anywhere in the project -- independent of which view
/// the developer is currently looking at.
pub(crate) fn chip_stack(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let resolution = model.resolution.read(cx);
    let chips = resolution.chips.clone();
    let expanded = resolution.expanded.clone();
    if chips.is_empty() {
        return div().into_any_element();
    }
    let mut stack = div()
        .debug_selector(|| "resolution-chip-stack".into())
        .absolute()
        .bottom(px(16.0))
        .right(px(16.0))
        .flex()
        .flex_col()
        .items_end()
        .gap_2();
    for chip in &chips {
        let id = chip.id.clone();
        let is_expanded = expanded.as_deref() == Some(id.as_str());
        let question_count = chip.questions.len();
        let title = chip
            .questions
            .first()
            .map(|question| question.question.clone())
            .unwrap_or_else(|| "Specification needs a file path".into());
        stack = stack.child(
            div()
                .id(gpui::SharedString::from(format!(
                    "resolution-chip-{}",
                    chip.id
                )))
                .self_start()
                .max_w(px(320.0))
                .p_2()
                .rounded_lg()
                .border_1()
                .border_color(if is_expanded {
                    theme::accent()
                } else {
                    theme::border()
                })
                .bg(theme::surface_raised())
                .cursor_pointer()
                .flex()
                .items_center()
                .gap_2()
                .hover(|style| style.border_color(theme::accent()))
                .on_click(cx.listener(move |model, _, _, cx| {
                    model.toggle_resolution_chip(id.clone(), cx);
                }))
                .child(
                    div()
                        .min_w(px(20.0))
                        .h(px(20.0))
                        .px_1()
                        .rounded_full()
                        .bg(theme::accent_soft())
                        .text_color(theme::accent())
                        .text_xs()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(question_count.to_string()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(title),
                )
                .into_any_element(),
        );
    }
    deferred(stack).with_priority(10).into_any_element()
}

/// The expanded accordion card, rendered as a modal above every other overlay
/// (`with_priority(25)`) once a chip has been toggled open.
pub(crate) fn expanded_conversation(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let resolution = model.resolution.read(cx);
    let Some(question_set_id) = resolution.expanded.clone() else {
        return div().into_any_element();
    };
    let Some(chip) = resolution.chip(&question_set_id).cloned() else {
        return div().into_any_element();
    };
    let conversation = resolution.conversation.clone();
    let error = resolution.error.clone();

    let mut content = div()
        .debug_selector(|| "resolution-expanded-content".into())
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .w(px(720.0))
        .max_w(relative(0.9))
        .max_h(relative(0.85))
        .min_h_0()
        .flex()
        .flex_col()
        .gap_3()
        .p_3()
        .rounded_lg()
        .border_1()
        .border_color(theme::border())
        .bg(theme::surface());

    let close_id = question_set_id.clone();
    content = content.child(
        div()
            .flex()
            .items_center()
            .justify_between()
            .child(div().flex_1().min_w_0().flex().flex_col().gap_1().children(
                chip.questions.iter().map(|question| {
                    div()
                        .text_sm()
                        .child(question.question.clone())
                        .into_any_element()
                }),
            ))
            .child(btn(
                "resolution-close",
                "Close",
                ButtonVariant::Ghost,
                true,
                cx,
                move |model, cx| model.toggle_resolution_chip(close_id.clone(), cx),
            )),
    );

    if let Some(message) = error {
        content = content.child(
            div()
                .p_2()
                .rounded_md()
                .bg(theme::danger_soft())
                .text_color(theme::danger())
                .text_sm()
                .child(message),
        );
    }

    let Some(conversation) = conversation else {
        content = content.child(empty_state("Opening the resolution conversation…"));
        return backdrop(content, cx);
    };

    let mut history = div()
        .id("resolution-history")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_2();
    if conversation.messages.is_empty() {
        history = history.child(empty_state(
            "Discuss the question with the Companion, then ask it to synthesize an answer.",
        ));
    } else {
        history = history.children(
            conversation
                .messages
                .iter()
                .cloned()
                .map(|message| crate::chat_view::render_message(message, cx)),
        );
    }
    content = content.child(history);

    let busy = conversation.busy;
    let streaming = conversation.is_streaming();
    let draft = model.resolution_draft_input.clone();
    content = content.child(
        div()
            .debug_selector(|| "resolution-composer".into())
            .flex_shrink_0()
            .p_2()
            .rounded_md()
            .border_1()
            .border_color(theme::border())
            .bg(theme::surface_raised())
            .flex()
            .flex_col()
            .gap_2()
            .child(text_area(draft))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::text_muted())
                            .child("Enter sends · Shift+Enter adds a new line"),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(btn(
                                "resolution-send",
                                if busy { "Sending…" } else { "Send" },
                                ButtonVariant::Secondary,
                                !busy && !streaming,
                                cx,
                                |model, cx| model.send_resolution_message(cx),
                            ))
                            .child(btn(
                                "resolution-synthesize",
                                "Ask for a FILES_* answer",
                                ButtonVariant::Primary,
                                !busy && !streaming && !conversation.messages.is_empty(),
                                cx,
                                |model, cx| model.synthesize_resolution_reply(cx),
                            )),
                    ),
            ),
    );

    if conversation.synthesized_draft.is_some() {
        let synthesis_input = model.resolution_synthesis_input.clone();
        content = content.child(
            div()
                .debug_selector(|| "resolution-synthesis-bubble".into())
                .flex_shrink_0()
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
                        .text_color(theme::accent())
                        .child("Reply to the agent ↗ (edit before confirming)"),
                )
                .child(text_area(synthesis_input))
                .child(btn(
                    "resolution-confirm",
                    if busy {
                        "Sending…"
                    } else {
                        "Confirm and send"
                    },
                    ButtonVariant::Primary,
                    !busy,
                    cx,
                    |model, cx| model.confirm_resolution_answer(cx),
                )),
        );
    }

    backdrop(content, cx)
}

fn backdrop(content: gpui::Div, cx: &mut Context<AppModel>) -> AnyElement {
    deferred(
        div()
            .debug_selector(|| "resolution-overlay".into())
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .occlude()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|model, _, _, cx| {
                    if let Some(id) = model.resolution.read(cx).expanded.clone() {
                        model.toggle_resolution_chip(id, cx);
                    }
                    cx.stop_propagation();
                }),
            )
            .on_key_down(cx.listener(|model, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape"
                    && let Some(id) = model.resolution.read(cx).expanded.clone()
                {
                    model.toggle_resolution_chip(id, cx);
                    cx.stop_propagation();
                }
            }))
            .bg(gpui::rgba(0x000000aa))
            .child(content),
    )
    .with_priority(25)
    .into_any_element()
}
