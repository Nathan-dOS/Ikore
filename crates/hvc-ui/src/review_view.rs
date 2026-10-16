//! Complete checkpoint artifacts, scoped to the persisted producer and independent of Workspace.

use gpui::{AnyElement, Context, IntoElement, SharedString, div, prelude::*, px};
use hvc_core::persistence_service::{
    HitlDecision, StageFailureCheckpoint, StageFailureDecision, StageFailureDetail,
};

use crate::app_model::AppModel;
use crate::failure_checkpoint::{AbortFailedStage, RetryFailedStage, ReviseFailedStage};
use crate::hitl::{ApproveHitl, RejectHitl, RequestHitlRevision};
use crate::theme;
use crate::workspace_view::render_markdown_block;

/// The decision surface for a stage failure parked on a checkpoint.
///
/// Shares the Review tab with the artifact review: a pulsation holds at most one waiting
/// checkpoint, so exactly one of the two is ever showing. Which buttons exist comes from
/// the host's persisted `allow_retry`/`allow_revision`, never from a local guess.
fn failure_panel(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let pulsations = model.pulsations.read(cx);
    let failure = &pulsations.failure;
    let Some(checkpoint) = failure.checkpoint.clone() else {
        return div().into_any_element();
    };
    let title = failure.title().unwrap_or_else(|| "Stage failure".into());
    let rationale = failure.rationale().unwrap_or_default();
    let busy = failure.busy;
    let can_decide = !busy;
    let allow_retry = checkpoint.allow_retry;
    let allow_revision = checkpoint.allow_revision;
    let show_feedback = failure.show_feedback;
    let detail = failure.detail.clone();
    let detail_loading = failure.detail_loading;
    let detail_error = failure.detail_error.clone();
    let decision_error = failure.error.clone();
    let confirm_abort = model.confirm_failure_abort;
    let revision_targets = failure.revision_targets();
    let selected_target = failure.selected_revision_target();
    let revision_label = selected_target
        .map(|stage| format!("Request revision to {stage:?}"))
        .unwrap_or_else(|| "Request revision".to_string());

    div()
        .size_full()
        .min_w_0()
        .min_h_0()
        .overflow_hidden()
        .flex()
        .flex_col()
        .key_context("FailureCheckpoint")
        .on_action(cx.listener(|model, _: &RetryFailedStage, _, cx| {
            model.resolve_stage_failure(StageFailureDecision::RetryStage, cx);
        }))
        .on_action(cx.listener(|model, _: &ReviseFailedStage, _, cx| {
            model.resolve_stage_failure(StageFailureDecision::RequestRevision, cx);
        }))
        .on_action(cx.listener(|model, _: &AbortFailedStage, _, cx| {
            if model.confirm_failure_abort {
                model.set_confirm_failure_abort(false, cx);
                model.resolve_stage_failure(StageFailureDecision::Abort, cx);
            } else if model.pulsations.read(cx).failure.checkpoint.is_some() {
                model.set_confirm_failure_abort(true, cx);
            }
        }))
        .child(
            div()
                .flex_shrink_0()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .p_3()
                .border_b_1()
                .border_color(theme::border())
                .bg(theme::danger_soft())
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::danger())
                        .child("STAGE FAILURE"),
                )
                .child(
                    div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_ellipsis()
                        .child(title),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(rationale),
                ),
        )
        .child(
            div()
                .id("failure-detail")
                .flex_1()
                .min_w_0()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_3()
                .p_3()
                .child(failure_facts(&checkpoint))
                .when(revision_targets.len() > 1, |panel| {
                    panel.child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme::text_muted())
                                    .child("Revise from"),
                            )
                            .children(revision_targets.iter().copied().map(|target| {
                                let selected = selected_target == Some(target);
                                let selector =
                                    gpui::SharedString::from(format!("failure-target-{target:?}"));
                                div()
                                    .id(selector.clone())
                                    .debug_selector(move || selector.to_string())
                                    .px_2()
                                    .py_1()
                                    .rounded_md()
                                    .text_xs()
                                    .cursor_pointer()
                                    .bg(if selected {
                                        theme::accent_soft()
                                    } else {
                                        theme::surface_raised()
                                    })
                                    .on_click(cx.listener(move |model, _, _, cx| {
                                        model.pulsations.update(cx, |state, _| {
                                            state.failure.select_revision_target(target);
                                        });
                                        cx.notify();
                                    }))
                                    .child(format!("{target:?}"))
                            })),
                    )
                })
                .child(match (detail_loading, detail_error, detail) {
                    (true, _, _) => div()
                        .text_sm()
                        .text_color(theme::text_muted())
                        .child("Loading the rejected output…")
                        .into_any_element(),
                    (_, Some(error), _) => div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .text_color(theme::danger())
                                .child(format!("Could not read the last attempt: {error}")),
                        )
                        .child(crate::widgets::btn(
                            "failure-detail-retry",
                            "Try again",
                            crate::widgets::ButtonVariant::Secondary,
                            true,
                            cx,
                            |model, cx| {
                                model.pulsations.update(cx, |state, _| {
                                    state.failure.retry_detail();
                                });
                                model.refresh_failure_detail(cx);
                            },
                        ))
                        .into_any_element(),
                    // Never invent an output: an executor that died before the agent ran
                    // has nothing to show, and saying so is the honest thing.
                    (_, _, Some(detail))
                        if detail.raw_output.is_some() || detail.validation_error.is_some() =>
                    {
                        rejected_output(&detail)
                    }
                    _ => div()
                        .text_sm()
                        .text_color(theme::text_muted())
                        .child("No model output was persisted for this attempt.")
                        .into_any_element(),
                }),
        )
        .when_some(decision_error, |panel, error| {
            panel.child(
                div()
                    .flex_shrink_0()
                    .px_3()
                    .py_2()
                    .text_sm()
                    .text_color(theme::danger())
                    .child(error),
            )
        })
        .when(show_feedback && allow_revision, |panel| {
            panel.child(
                div()
                    .flex_shrink_0()
                    .min_w_0()
                    .p_3()
                    .border_t_1()
                    .border_color(theme::border())
                    .child(
                        div()
                            .mb_1()
                            .text_xs()
                            .text_color(theme::text_muted())
                            .child("Guidance for the retried stage"),
                    )
                    .child(crate::sizing::text_area(model.stage_feedback_input.clone())),
            )
        })
        .child(
            div()
                .debug_selector(|| "failure-decision-bar".into())
                .flex_shrink_0()
                .min_h(px(54.0))
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .p_3()
                .border_t_1()
                .border_color(theme::border())
                .when(allow_retry, |bar| {
                    bar.child(crate::widgets::btn(
                        "failure-retry",
                        "Retry stage",
                        crate::widgets::ButtonVariant::Primary,
                        can_decide,
                        cx,
                        |model, cx| {
                            model.resolve_stage_failure(StageFailureDecision::RetryStage, cx);
                        },
                    ))
                })
                .when(allow_revision, |bar| {
                    bar.child(crate::widgets::btn(
                        "failure-revision",
                        revision_label.clone(),
                        crate::widgets::ButtonVariant::Secondary,
                        can_decide,
                        cx,
                        |model, cx| {
                            model.resolve_stage_failure(StageFailureDecision::RequestRevision, cx);
                        },
                    ))
                })
                .when(allow_revision, |bar| {
                    bar.child(
                        div()
                            .id("failure-feedback-toggle")
                            .px_2()
                            .py_1()
                            .text_xs()
                            .cursor_pointer()
                            .text_color(theme::text_muted())
                            .on_click(cx.listener(|model, _, _, cx| {
                                model.pulsations.update(cx, |state, _| {
                                    state.failure.show_feedback = !state.failure.show_feedback
                                });
                                cx.notify();
                            }))
                            .child(if show_feedback {
                                "Hide guidance"
                            } else {
                                "Add guidance"
                            }),
                    )
                })
                .child(crate::widgets::btn_confirm(
                    "failure-abort",
                    "Abort pulsation",
                    "Confirm abort",
                    confirm_abort,
                    can_decide,
                    crate::widgets::ButtonVariant::Danger,
                    cx,
                    |model, cx| model.set_confirm_failure_abort(true, cx),
                    |model, cx| {
                        model.set_confirm_failure_abort(false, cx);
                        model.resolve_stage_failure(StageFailureDecision::Abort, cx);
                    },
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

fn failure_facts(checkpoint: &StageFailureCheckpoint) -> AnyElement {
    let mut rows = vec![
        ("Error", checkpoint.error_code.clone()),
        ("Stage", format!("{:?}", checkpoint.stage)),
        ("Attempts", checkpoint.attempts.max(0).to_string()),
        ("Diagnostic", checkpoint.diagnostic.clone()),
    ];
    if checkpoint.stage_execution_id.is_some() {
        rows.push((
            "Extra permits left",
            checkpoint
                .remaining_extra_invocations
                .map(|n| n.to_string())
                .unwrap_or_else(|| "unknown".into()),
        ));
        rows.push((
            "Permitted targets",
            if checkpoint.allowed_revision_targets.is_empty() {
                "none".into()
            } else {
                checkpoint
                    .allowed_revision_targets
                    .iter()
                    .map(|t| format!("{t:?}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            },
        ));
    }
    div()
        .flex()
        .flex_col()
        .gap_2()
        .children(rows.into_iter().map(|(label, value)| {
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .child(
                    div()
                        .flex_shrink_0()
                        .w(px(140.0))
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(label),
                )
                .child(div().flex_1().min_w_0().text_sm().child(value))
        }))
        .into_any_element()
}

fn rejected_output(detail: &StageFailureDetail) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .when_some(detail.validation_error.clone(), |block, error| {
            block.child(
                div()
                    .text_xs()
                    .text_color(theme::danger())
                    .child(format!("Rejected because: {error}")),
            )
        })
        .when_some(detail.raw_output.clone(), |block, raw| {
            block
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child("Last output from the agent"),
                )
                .child(
                    div()
                        .id("failure-raw-output")
                        .max_h(px(360.0))
                        .overflow_y_scroll()
                        .p_2()
                        .rounded_sm()
                        .bg(theme::surface_raised())
                        .text_xs()
                        .child(raw),
                )
        })
        .into_any_element()
}

pub(crate) fn review_panel(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    // A pulsation holds at most one waiting checkpoint, so the failure surface simply
    // takes over the Review tab when that checkpoint is a parked failure.
    if model.pulsations.read(cx).failure.checkpoint.is_some() {
        return failure_panel(model, cx);
    }
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
    // The Brief tab (ADR-024) shares the reader with the documents; the decision
    // bar and its gate do not change with it.
    let has_brief_tab = hitl.has_brief_tab() && !documents.is_empty();
    let showing_brief = has_brief_tab && hitl.brief_selected;
    let brief_content = showing_brief.then(|| match (&hitl.brief, &hitl.brief_error) {
        (Some(brief), _) => crate::review_brief_view::brief_panel(brief),
        (None, Some(error)) => crate::review_brief_view::brief_error_panel(error),
        (None, None) => crate::review_brief_view::brief_error_panel("No brief was produced."),
    });
    let active_document = documents.get(selected).cloned().filter(|_| !showing_brief);
    let reader_id = SharedString::from(format!(
        "review-reader-{}-{}-{}",
        hitl.expected_checkpoint
            .as_ref()
            .map(|c| c.checkpoint_id.as_str())
            .unwrap_or("none"),
        if showing_brief {
            "brief"
        } else {
            active_document
                .as_ref()
                .map(|d| d.artifact.artifact_id.as_str())
                .unwrap_or("none")
        },
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
                    .when(has_brief_tab, |tabs| {
                        tabs.child(
                            div()
                                .id("review-brief-tab")
                                .debug_selector(|| "review-brief-tab".into())
                                .flex_shrink_0()
                                .flex()
                                .items_center()
                                .gap_2()
                                .px_3()
                                .py_1()
                                .rounded_sm()
                                .text_sm()
                                .bg(if showing_brief {
                                    theme::accent_soft()
                                } else {
                                    theme::surface_raised()
                                })
                                .cursor_pointer()
                                .on_click(cx.listener(|model, _, _, cx| {
                                    model.set_confirm_hitl_reject(false, cx);
                                    model.pulsations.update(cx, |pulsations, cx| {
                                        pulsations.hitl.brief_selected = true;
                                        cx.notify();
                                    });
                                    cx.notify();
                                }))
                                .child(crate::widgets::icon(
                                    crate::icons::Icon::Eye,
                                    px(14.0),
                                    theme::accent(),
                                ))
                                .child("Brief")
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme::text_muted())
                                        .child("derived"),
                                ),
                        )
                    })
                    .children(documents.iter().enumerate().map(|(index, document)| {
                        div()
                            .id(SharedString::from(format!(
                                "review-document-{}",
                                document.artifact.artifact_id
                            )))
                            .debug_selector(move || format!("review-document-tab-{index}"))
                            // One line, ellipsised: a wrapping label collapsed to its
                            // first word ("1") inside the 40 px scrolling strip.
                            .flex_shrink_0()
                            .max_w(px(320.0))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .px_3()
                            .py_1()
                            .rounded_sm()
                            .text_sm()
                            .bg(if index == selected && !showing_brief {
                                theme::accent_soft()
                            } else {
                                theme::surface_raised()
                            })
                            .cursor_pointer()
                            .on_click(cx.listener(move |model, _, _, cx| {
                                model.set_confirm_hitl_reject(false, cx);
                                model.pulsations.update(cx, |pulsations, cx| {
                                    pulsations.hitl.selected_document = index;
                                    pulsations.hitl.brief_selected = false;
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
                .when_some(brief_content, |reader, content| reader.child(content))
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
                    .child(crate::sizing::text_area(model.stage_feedback_input.clone())),
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
    let variant = if danger {
        crate::widgets::ButtonVariant::Danger
    } else if decision == HitlDecision::Approve {
        crate::widgets::ButtonVariant::Primary
    } else {
        crate::widgets::ButtonVariant::Secondary
    };
    crate::widgets::button_frame(
        div().id(id).debug_selector(move || id.into()).tab_index(0),
        variant,
        enabled,
    )
    .when(enabled, |button| {
        button.on_click(cx.listener(move |model, _, _, cx| {
            model.set_confirm_hitl_reject(false, cx);
            model.apply_hitl_decision(decision, cx);
        }))
    })
    .child(label)
}

fn hitl_reject_button(
    id: &'static str,
    label: &'static str,
    enabled: bool,
    confirming: bool,
    cx: &mut Context<AppModel>,
) -> impl IntoElement {
    crate::widgets::button_frame(
        div().id(id).debug_selector(move || id.into()).tab_index(0),
        crate::widgets::ButtonVariant::Danger,
        enabled,
    )
    // The armed second click is a solid danger fill: it must not look like the first.
    .when(enabled && confirming, |button| {
        button
            .bg(theme::danger())
            .border_color(theme::danger())
            .text_color(theme::canvas())
    })
    .when(enabled, |button| {
        button.on_click(cx.listener(move |model, _, _, cx| {
            if confirming {
                model.apply_hitl_decision(HitlDecision::Reject, cx);
            } else {
                model.set_confirm_hitl_reject(true, cx);
            }
        }))
    })
    .child(label)
}
