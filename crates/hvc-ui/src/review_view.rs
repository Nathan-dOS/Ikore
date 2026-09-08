// The HITL checkpoint review surface. Formerly a Workspace view mode reached through a separate
// Editor/HITL toggle (three navigation levels away from the pipeline in Pulsations); now a section
// of Pulsations itself, selected automatically when a checkpoint appears. See the UI flow plan
// Part 1.2.

use gpui::{AnyElement, Context, IntoElement, div, prelude::*, px};
use hvc_core::persistence_service::HitlDecision;
use hvc_editor::{DiffMark, build_diff_rows};

use crate::app_model::AppModel;
use crate::hitl::{ApproveHitl, RejectHitl, RequestHitlRevision};
use crate::theme;
use crate::workspace::WorkspaceTabModel;

struct HitlSurfaceContent {
    checkpoint_id: Option<String>,
    title: Option<String>,
    input: String,
    generated: String,
    busy: bool,
    error: Option<String>,
}

pub(crate) fn review_panel(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    // The diff prefers the Workspace editor's open tab for the checkpoint's file when one happens
    // to be open (real before/after content); otherwise it falls back to the orchestration
    // runtime's synthetic checkpoint summary. This mirrors the pre-move behavior exactly.
    let active_tab = model.workspace.read(cx).active_tab().cloned();
    let (hitl, diff_expanded, confirm_reject) = {
        let pulsations = model.pulsations.read(cx);
        (
            HitlSurfaceContent {
                checkpoint_id: pulsations.hitl.pulsation_id.clone(),
                title: pulsations.hitl.title.clone(),
                input: pulsations.hitl.input.clone(),
                generated: pulsations.hitl.generated.clone(),
                busy: pulsations.hitl.busy,
                error: pulsations.hitl.error.clone(),
            },
            pulsations.diff_expanded,
            model.confirm_hitl_reject,
        )
    };
    div()
        .size_full()
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
            } else {
                model.set_confirm_hitl_reject(true, cx);
            }
        }))
        .child(hitl_diff_surface(
            active_tab,
            hitl,
            diff_expanded,
            confirm_reject,
            cx,
        ))
        .into_any_element()
}

fn hitl_diff_surface(
    active_tab: Option<WorkspaceTabModel>,
    hitl: HitlSurfaceContent,
    expanded: bool,
    confirm_reject: bool,
    cx: &mut Context<AppModel>,
) -> impl IntoElement {
    let input = active_tab
        .as_ref()
        .map(|tab| tab.base_content.as_str())
        .unwrap_or(hitl.input.as_str());
    let generated = active_tab
        .as_ref()
        .map(|tab| tab.content.as_str())
        .unwrap_or(hitl.generated.as_str());
    let review_title = active_tab
        .as_ref()
        .map(|tab| tab.relative_path.to_string())
        .or(hitl.title)
        .unwrap_or_else(|| "Markdown Diff".into());
    let rows = build_diff_rows(input, generated);
    let has_checkpoint = hitl.checkpoint_id.is_some();
    let busy = hitl.busy;
    div()
        .size_full()
        .flex()
        .flex_col()
        .child(
            div()
                .h(px(52.0))
                .flex()
                .items_center()
                .justify_between()
                .px_4()
                .border_b_1()
                .border_color(theme::border())
                .child(
                    div()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::text_muted())
                                .child("CHECKPOINT REVIEW"),
                        )
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(review_title),
                        ),
                )
                .child(
                    div()
                        .id("review-diff-expand")
                        .px_3()
                        .py_1()
                        .rounded_sm()
                        .bg(theme::surface_raised())
                        .cursor_pointer()
                        .on_click(cx.listener(|model, _, _, cx| {
                            model.toggle_pulsation_diff(cx);
                        }))
                        .child(if expanded {
                            "Collapse diff"
                        } else {
                            "Expand diff"
                        }),
                ),
        )
        .child(div().flex_1().min_h_0().flex().children(
            [("Input baseline", true), ("Generated draft", false)].map(|(title, left)| {
                div()
                    .id(if left {
                        "review-diff-left"
                    } else {
                        "review-diff-right"
                    })
                    .flex_1()
                    .min_w_0()
                    .overflow_y_scroll()
                    .when(left, |panel| {
                        panel.border_r_1().border_color(theme::border())
                    })
                    .child(
                        div()
                            .p_3()
                            .bg(theme::surface_raised())
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .children(rows.iter().map(move |row| {
                        let side = if left { &row.left } else { &row.right };
                        let color = match side.mark {
                            DiffMark::Add => theme::accent(),
                            DiffMark::Remove | DiffMark::Change => theme::warning(),
                            DiffMark::Neutral | DiffMark::Empty => theme::text_muted(),
                        };
                        div()
                            .min_h(px(24.0))
                            .flex()
                            .gap_2()
                            .px_3()
                            .font_family(theme::FONT_MONO)
                            .text_sm()
                            .text_color(color)
                            .child(
                                div().w(px(34.0)).text_color(theme::text_muted()).child(
                                    side.line.map(|line| line.to_string()).unwrap_or_default(),
                                ),
                            )
                            .child(side.marker.to_string())
                            .child(if side.text.is_empty() {
                                " ".to_owned()
                            } else {
                                side.text.clone()
                            })
                    }))
            }),
        ))
        .when_some(hitl.error, |surface, error| {
            surface.child(
                div()
                    .px_4()
                    .py_2()
                    .bg(theme::danger_soft())
                    .text_color(theme::danger())
                    .child(error),
            )
        })
        .child(
            div()
                .min_h(px(54.0))
                .flex()
                .items_center()
                .gap_2()
                .px_4()
                .border_t_1()
                .border_color(theme::border())
                .child(hitl_button(
                    "hitl-approve",
                    "Approve  Enter",
                    !busy && has_checkpoint,
                    HitlDecision::Approve,
                    false,
                    cx,
                ))
                .child(hitl_button(
                    "hitl-revision",
                    "Request revision  R",
                    !busy && has_checkpoint,
                    HitlDecision::RequestRevision,
                    false,
                    cx,
                ))
                .child(hitl_reject_button(
                    "hitl-reject",
                    if confirm_reject {
                        "Confirm rejection"
                    } else {
                        "Reject"
                    },
                    !busy && has_checkpoint,
                    confirm_reject,
                    cx,
                ))
                .when(!has_checkpoint, |bar| {
                    bar.child(
                        div()
                            .ml_auto()
                            .text_sm()
                            .text_color(theme::text_muted())
                            .child("No checkpoint awaiting decision."),
                    )
                })
                .when(busy, |bar| {
                    bar.child(
                        div()
                            .ml_auto()
                            .text_sm()
                            .text_color(theme::accent())
                            .child("Applying decision…"),
                    )
                }),
        )
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
