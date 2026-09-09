//! Complete checkpoint artifacts, scoped to the persisted producer and independent of Workspace.

use gpui::{AnyElement, Context, IntoElement, SharedString, div, prelude::*, px};
use hvc_core::persistence_service::HitlDecision;

use crate::app_model::AppModel;
use crate::hitl::{ApproveHitl, RejectHitl, RequestHitlRevision};
use crate::theme;
use crate::workspace_view::render_markdown_block;

pub(crate) fn review_panel(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let pulsations = model.pulsations.read(cx);
    let hitl = &pulsations.hitl;
    let title = hitl
        .title
        .clone()
        .unwrap_or_else(|| "Checkpoint review".into());
    let has_checkpoint = hitl.expected_checkpoint.is_some();
    let can_decide = has_checkpoint && !hitl.busy;
    let can_approve = hitl.can_approve();
    let busy = hitl.busy;
    let loading = hitl.documents_loading;
    let load_error = hitl.documents_error.clone();
    let decision_error = hitl.error.clone();
    let selected = hitl.selected_document;
    let source = hitl.show_source;
    let show_feedback = hitl.show_feedback;
    let expanded = pulsations.diff_expanded;
    let documents = hitl.documents.clone();
    let active_document = documents.get(selected).cloned();
    let reader_id = SharedString::from(format!(
        "review-reader-{}-{}-{}",
        hitl.expected_checkpoint
            .as_ref()
            .map(|c| c.checkpoint_id.as_str())
            .unwrap_or("none"),
        active_document
            .as_ref()
            .map(|d| d.artifact.artifact_id.as_str())
            .unwrap_or("none"),
        source,
    ));
    let confirm_reject = model.confirm_hitl_reject;

    div()
        .size_full()
        .min_w_0()
        .min_h_0()
        .overflow_hidden()
        .flex()
        .flex_col()
        .key_context("HitlReview")
        .on_action(cx.listener(|model, _: &ApproveHitl, _, cx| {
            model.apply_hitl_decision(HitlDecision::Approve, cx);
        }))
        .on_action(cx.listener(|model, _: &RequestHitlRevision, _, cx| {
            model.apply_hitl_decision(HitlDecision::RequestRevision, cx);
        }))
        .on_action(cx.listener(|model, _: &RejectHitl, _, cx| {
            if model.confirm_hitl_reject {
                model.set_confirm_hitl_reject(false, cx);
                model.apply_hitl_decision(HitlDecision::Reject, cx);
            } else if model.pulsations.read(cx).hitl.expected_checkpoint.is_some() {
                model.set_confirm_hitl_reject(true, cx);
            }
        }))
        .child(
            div()
                .flex_shrink_0()
                .min_w_0()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap_2()
                .p_3()
                .border_b_1()
                .border_color(theme::border())
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::text_muted())
                                .child("CHECKPOINT REVIEW"),
                        )
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_ellipsis()
                                .child(title),
                        ),
                )
                .child(
                    div()
                        .id("review-expand")
                        .debug_selector(|| "review-expand".into())
                        .px_3()
                        .py_1()
                        .rounded_sm()
                        .bg(theme::surface_raised())
                        .cursor_pointer()
                        .on_click(cx.listener(|model, _, _, cx| model.toggle_pulsation_diff(cx)))
                        .child(if expanded {
                            "Show pulsations"
                        } else {
                            "Expand review"
                        }),
                ),
        )
        .when(!documents.is_empty(), |panel| {
            panel.child(
                div()
                    .id("review-document-tabs")
                    .flex_shrink_0()
                    .min_w_0()
                    .h(px(40.0))
                    .overflow_x_scroll()
                    .flex()
                    .gap_1()
                    .p_2()
                    .border_b_1()
                    .border_color(theme::border())
                    .children(documents.iter().enumerate().map(|(index, document)| {
                        div()
                            .id(SharedString::from(format!(
                                "review-document-{}",
                                document.artifact.artifact_id
                            )))
                            .min_w_0()
                            .flex_shrink_0()
                            .max_w_full()
                            .px_3()
                            .py_1()
                            .rounded_sm()
                            .text_sm()
                            .bg(if index == selected {
                                theme::accent_soft()
                            } else {
                                theme::surface_raised()
                            })
                            .cursor_pointer()
                            .on_click(cx.listener(move |model, _, _, cx| {
                                model.set_confirm_hitl_reject(false, cx);
                                model.pulsations.update(cx, |pulsations, cx| {
                                    pulsations.hitl.selected_document = index;
                                    cx.notify();
                                });
                                cx.notify();
                            }))
                            .child(format!("{}. {}", index + 1, document.artifact.name))
                    })),
            )
        })
        .when_some(active_document.clone(), |panel, document| {
            let copy = document.artifact.content.clone();
            panel.child(
                div()
                    .flex_shrink_0()
                    .min_w_0()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(theme::text_muted())
                            .text_ellipsis()
                            .child(document.artifact.relative_path.clone().unwrap_or_else(|| {
                                format!("{} · Persisted document", document.artifact.kind)
                            })),
                    )
                    .child(
                        div()
                            .id("review-source-toggle")
                            .debug_selector(|| "review-source-toggle".into())
                            .px_2()
                            .py_1()
                            .rounded_sm()
                            .bg(theme::surface_raised())
                            .text_sm()
                            .cursor_pointer()
                            .on_click(cx.listener(|model, _, _, cx| {
                                model.pulsations.update(cx, |pulsations, cx| {
                                    pulsations.hitl.show_source = !pulsations.hitl.show_source;
                                    cx.notify();
                                });
                                cx.notify();
                            }))
                            .child(if source {
                                "Document"
                            } else {
                                "Markdown source"
                            }),
                    )
                    .child(
                        div()
                            .id("review-copy-document")
                            .debug_selector(|| "review-copy-document".into())
                            .px_2()
                            .py_1()
                            .rounded_sm()
                            .bg(theme::surface_raised())
                            .text_sm()
                            .cursor_pointer()
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy.clone()))
                            })
                            .child("Copy Markdown"),
                    ),
            )
        })
        .child(
            div()
                .id(reader_id)
                .debug_selector(|| "review-document-reader".into())
                .flex_1()
                .min_w_0()
                .min_h_0()
                .overflow_scroll()
                .p_4()
                .when(!has_checkpoint, |reader| {
                    reader.child("No checkpoint awaiting decision.")
                })
                .when(loading, |reader| {
                    reader.child("Loading complete checkpoint documents…")
                })
                .when_some(load_error, |reader, error| {
                    reader
                        .child(div().text_color(theme::danger()).child(error))
                        .child(
                            div()
                                .id("review-retry")
                                .mt_3()
                                .px_3()
                                .py_2()
                                .rounded_md()
                                .bg(theme::surface_raised())
                                .cursor_pointer()
                                .on_click(cx.listener(|model, _, _, cx| {
                                    model.pulsations.update(cx, |pulsations, _| {
                                        pulsations.hitl.retry_documents()
                                    });
                                    model.refresh_hitl_documents(cx);
                                }))
                                .child("Retry loading documents"),
                        )
                })
                .when_some(active_document, |reader, document| {
                    if source {
                        reader.child(
                            div()
                                .min_w_0()
                                .w_full()
                                .whitespace_normal()
                                .font_family(theme::FONT_MONO)
                                .text_sm()
                                .line_height(px(22.0))
                                .child(document.artifact.content),
                        )
                    } else {
                        reader.children(document.preview.into_iter().map(render_markdown_block))
                    }
                }),
        )
        .child(
            div()
                .id("review-validation")
                .flex_shrink_0()
                .h(px(28.0))
                .overflow_y_scroll()
                .px_3()
                .text_sm()
                .text_color(theme::danger())
                .child(decision_error.unwrap_or_default()),
        )
        .when(has_checkpoint && show_feedback, |panel| {
            panel.child(
                div()
                    .flex_shrink_0()
                    .px_3()
                    .py_2()
                    .border_t_1()
                    .border_color(theme::border())
                    .child(
                        div()
                            .mb_1()
                            .text_xs()
                            .text_color(theme::text_muted())
                            .child("Revision feedback"),
                    )
                    .child(model.stage_feedback_input.clone()),
            )
        })
        .child(
            div()
                .debug_selector(|| "review-decision-bar".into())
                .flex_shrink_0()
                .min_h(px(54.0))
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .p_3()
                .border_t_1()
                .border_color(theme::border())
                .child(hitl_button(
                    "hitl-approve",
                    "Approve",
                    can_approve,
                    HitlDecision::Approve,
                    false,
                    cx,
                ))
                .child(hitl_button(
                    "hitl-revision",
                    "Request revision",
                    can_decide,
                    HitlDecision::RequestRevision,
                    false,
                    cx,
                ))
                .when(has_checkpoint, |bar| {
                    bar.child(
                        div()
                            .id("review-feedback-toggle")
                            .px_2()
                            .py_1()
                            .text_xs()
                            .cursor_pointer()
                            .text_color(theme::text_muted())
                            .on_click(cx.listener(|model, _, _, cx| {
                                model.pulsations.update(cx, |state, _| {
                                    state.hitl.show_feedback = !state.hitl.show_feedback
                                });
                                cx.notify();
                            }))
                            .child(if show_feedback {
                                "Hide feedback"
                            } else {
                                "Add feedback"
                            }),
                    )
                })
                .child(hitl_reject_button(
                    "hitl-reject",
                    if confirm_reject {
                        "Confirm rejection"
                    } else {
                        "Reject"
                    },
                    can_decide,
                    confirm_reject,
                    cx,
                ))
                .when(busy, |bar| {
                    bar.child(
                        div()
                            .text_sm()
                            .text_color(theme::accent())
                            .child("Applying decision…"),
                    )
                }),
        )
        .into_any_element()
}

fn hitl_button(
    id: &'static str,
    label: &'static str,
    enabled: bool,
    decision: HitlDecision,
    danger: bool,
    cx: &mut Context<AppModel>,
) -> impl IntoElement {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .px_3()
        .py_2()
        .rounded_md()
        .tab_index(0)
        .bg(if danger {
            theme::danger_soft()
        } else {
            theme::surface_raised()
        })
        .text_color(if danger {
            theme::danger()
        } else {
            theme::text()
        })
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .hover(|style| style.opacity(theme::HOVER_OPACITY))
                .on_click(cx.listener(move |model, _, _, cx| {
                    model.set_confirm_hitl_reject(false, cx);
                    model.apply_hitl_decision(decision, cx);
                }))
        })
        .when(!enabled, |button| button.opacity(0.45))
        .child(label)
}

fn hitl_reject_button(
    id: &'static str,
    label: &'static str,
    enabled: bool,
    confirming: bool,
    cx: &mut Context<AppModel>,
) -> impl IntoElement {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .px_3()
        .py_2()
        .rounded_md()
        .tab_index(0)
        .bg(if confirming {
            theme::danger()
        } else {
            theme::danger_soft()
        })
        .text_color(if confirming {
            theme::canvas()
        } else {
            theme::danger()
        })
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .hover(|style| style.opacity(theme::HOVER_OPACITY))
                .on_click(cx.listener(move |model, _, _, cx| {
                    if confirming {
                        model.apply_hitl_decision(HitlDecision::Reject, cx);
                    } else {
                        model.set_confirm_hitl_reject(true, cx);
                    }
                }))
        })
        .when(!enabled, |button| button.opacity(0.45))
        .child(label)
}
