use gpui::{AnyElement, Context, IntoElement, div, prelude::*, px, relative};
use hvc_core::orchestration::BaseState;
use hvc_core::orchestration::brownfield::BrownfieldCoverageMetrics;
use hvc_core::persistence_service::{Pulsation, PulsationStage, PulsationStatus};

use crate::app_model::AppModel;
use crate::pulsations::{
    PIPELINE_STAGES, PulsationDetailSection, STATUS_GROUPS, stage_label, status_group_id,
    status_label,
};
use crate::sizing::StyledExt;
use crate::theme;

pub(crate) fn pulsations_panel(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let has_project = model.workspace.read(cx).project_id.is_some();
    let queue = model
        .pulsations
        .read(cx)
        .coordination
        .as_ref()
        .map(|s| s.queue.clone())
        .unwrap_or_default();
    let (
        pulsations,
        active_id,
        active,
        artifacts,
        runtime_loading,
        artifacts_loading,
        awaiting_hitl,
        running_jobs,
        _notice,
        _error,
        brownfield_coverage,
        section,
        diff_expanded,
    ) = {
        let state = model.pulsations.read(cx);
        (
            state.pulsations.clone(),
            state.active_id(),
            state.active().cloned(),
            state.artifacts.clone(),
            state.runtime_loading,
            state.artifacts_loading,
            state.awaiting_hitl,
            state.running_jobs,
            state.notice.clone(),
            state.error.clone(),
            state.brownfield_coverage.clone(),
            state.detail_section,
            state.diff_expanded,
        )
    };
    let hide_pulsation_list = section == PulsationDetailSection::Review && diff_expanded;

    if !has_project {
        return empty_panel(
            "No active project",
            "Open a workspace before creating or tracking pulsations.",
        )
        .into_any_element();
    }

    let list = div()
        .size_full()
        .min_w_0()
        .overflow_hidden()
        .min_h_0()
        .p_3()
        .rounded_lg()
        .border_1()
        .border_color(theme::border())
        .bg(theme::surface())
        .flex()
        .flex_col()
        .gap_3()
        .child(section_heading(
            "PROJECT FLOW",
            "Pulsations",
            pulsations.len(),
        ))
        .child(crate::forge_view::button(
            "compose-task-batch",
            "Compose pulsation with tasks",
            crate::forge::ForgeAction::OpenBacklog,
            cx,
        ))
        .child(
            div()
                .id("pulsation-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_2()
                .when(pulsations.is_empty(), |list| {
                    list.child(
                        div()
                            .p_3()
                            .text_sm()
                            .text_color(theme::text_muted())
                            .child("Create this project's first pulsation."),
                    )
                })
                .children(STATUS_GROUPS.into_iter().filter_map(|status| {
                    let members: Vec<_> = pulsations
                        .iter()
                        .filter(|pulse| pulse.status == status)
                        .cloned()
                        .collect();
                    // Keep Running anchored even while no job is running.
                    if members.is_empty() && status != PulsationStatus::Running {
                        return None;
                    }
                    let expanded = model.pulsations.read(cx).group_expanded(status);
                    let selected = members
                        .iter()
                        .any(|pulse| Some(pulse.id.as_str()) == active_id.as_deref());
                    let id = gpui::SharedString::from(format!(
                        "pulsation-group-{}",
                        status_group_id(status)
                    ));
                    let debug_id = id.clone();
                    Some(
                        div()
                            .flex_shrink_0()
                            .min_w_0()
                            .rounded_md()
                            .border_1()
                            .border_color(theme::border())
                            .overflow_hidden()
                            .child(
                                div()
                                    .id(id)
                                    .debug_selector(move || debug_id.to_string())
                                    .px_3()
                                    .py_2()
                                    .cursor_pointer()
                                    .text_sm()
                                    .bg(theme::surface_raised())
                                    .text_color(if selected {
                                        theme::accent()
                                    } else {
                                        theme::text()
                                    })
                                    .hover(|style| style.bg(theme::accent_soft()))
                                    .on_click(cx.listener(move |model, _, _, cx| {
                                        model
                                            .pulsations
                                            .update(cx, |state, _| state.toggle_group(status));
                                        cx.notify();
                                    }))
                                    .child(format!(
                                        "{} {} · {}{}",
                                        if expanded { "▾" } else { "▸" },
                                        status_label(status, None),
                                        members.len(),
                                        if selected && !expanded {
                                            " · selected"
                                        } else {
                                            ""
                                        }
                                    )),
                            )
                            .when(expanded, |group| {
                                group.child(
                                    div()
                                        .p_2()
                                        .flex()
                                        .flex_col()
                                        .gap_2()
                                        .when(members.is_empty(), |list| {
                                            list.child(
                                                div()
                                                    .p_2()
                                                    .text_sm()
                                                    .text_color(theme::text_muted())
                                                    .child("No running pulsations."),
                                            )
                                        })
                                        .children(members.into_iter().map(|pulse| {
                                            let position =
                                                queue.iter().find(|p| p.pulsation_id == pulse.id);
                                            let label = position.map(|p| {
                                                format!(
                                                    "#{} · {}",
                                                    p.sequence,
                                                    blocking_label(p.blocking_reason.as_deref())
                                                )
                                            });
                                            pulsation_card(pulse, active_id.as_deref(), label, cx)
                                        })),
                                )
                            }),
                    )
                })),
        )
        .into_any_element();
    let detail = div()
        .id("pulsation-detail-scroll")
        .size_full()
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
        .when_some(active, |panel, active| {
            let section = model.pulsations.read(cx).detail_section;
            let review_pending = model.pulsations.read(cx).hitl.expected_checkpoint.is_some();
            let preparation_pending = review_pending
                || model.pulsations.read(cx).detail().is_some_and(|detail| {
                    detail.questions.is_some() || detail.description_editable
                });
            let backend_override = model.pulsations.read(cx).backend_override.clone();
            let backend_override_busy = model.pulsations.read(cx).backend_override_busy;
            panel
                .child(
                    div()
                        .id("pulsation-section-tabs")
                        .debug_selector(|| "pulsation-section-tabs".into())
                        .flex()
                        .flex_shrink_0()
                        .items_center()
                        .justify_center()
                        .flex_wrap()
                        .gap_2()
                        .children(
                            [
                                (
                                    PulsationDetailSection::Review,
                                    "pulsation-review",
                                    if review_pending {
                                        "Review · pending"
                                    } else {
                                        "Review"
                                    },
                                ),
                                (
                                    PulsationDetailSection::Overview,
                                    "pulsation-overview",
                                    "Overview",
                                ),
                                (
                                    PulsationDetailSection::Preparation,
                                    "pulsation-preparation",
                                    if preparation_pending {
                                        "Preparation · pending"
                                    } else {
                                        "Preparation"
                                    },
                                ),
                                (
                                    PulsationDetailSection::Artifacts,
                                    "pulsation-artifacts",
                                    "Artifacts",
                                ),
                            ]
                            .into_iter()
                            .map(|(target, id, label)| {
                                div()
                                    .id(id)
                                    .debug_selector(move || id.into())
                                    .px_3()
                                    .py_2()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .text_sm()
                                    .bg(if section == target {
                                        theme::accent_soft()
                                    } else {
                                        theme::surface_raised()
                                    })
                                    .text_color(if section == target {
                                        theme::accent()
                                    } else {
                                        theme::text_muted()
                                    })
                                    .on_click(cx.listener(move |model, _, _, cx| {
                                        model
                                            .pulsations
                                            .update(cx, |state, _| state.detail_section = target);
                                        cx.notify();
                                    }))
                                    .child(label)
                            }),
                        ),
                )
                .child(
                    div()
                        .id("pulsation-section-scroll")
                        .debug_selector(|| "pulsation-section-scroll".into())
                        .flex_1()
                        .min_h_0()
                        .min_w_0()
                        .when(section == PulsationDetailSection::Review, |panel| {
                            panel.overflow_hidden()
                        })
                        .when(section != PulsationDetailSection::Review, |panel| {
                            panel.overflow_y_scroll()
                        })
                        .when(section != PulsationDetailSection::Review, |scroll| {
                            scroll.child(
                                div()
                                    .flex_shrink_0()
                                    .mb_4()
                                    .child(pulsation_detail_header(
                                        active.clone(),
                                        runtime_loading,
                                        backend_override,
                                        backend_override_busy,
                                        cx,
                                    ))
                                    .child(pipeline(active.stage)),
                            )
                        })
                        .child(match section {
                            PulsationDetailSection::Review => {
                                crate::review_view::review_panel(model, cx)
                            }
                            PulsationDetailSection::Overview => detail_overview(
                                model,
                                &active,
                                running_jobs,
                                awaiting_hitl,
                                brownfield_coverage,
                                cx,
                            ),
                            PulsationDetailSection::Preparation => preparation_panel(model, cx),
                            PulsationDetailSection::Artifacts => {
                                detail_artifacts(model, artifacts, artifacts_loading, cx)
                            }
                        }),
                )
        })
        .when(active_id.is_none(), |panel| {
            panel.child(empty_panel(
                "No pulsations",
                "Create the first pulsation to view the pipeline and artifacts.",
            ))
        })
        .into_any_element();
    div()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .p_4()
        .overflow_hidden()
        .child(if hide_pulsation_list {
            detail
        } else {
            crate::split_pane::split_pane(
                "pulsations-columns",
                &model.pulsations_split,
                list,
                detail,
                cx,
            )
        })
        .into_any_element()
}

fn detail_overview(
    model: &AppModel,
    active: &Pulsation,
    running_jobs: usize,
    awaiting_hitl: usize,
    coverage: Option<BrownfieldCoverageMetrics>,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    div()
        .when_some(coverage, |panel, coverage| {
            panel.child(brownfield_progress_panel(coverage))
        })
        // Undoing a failed pulsation's files comes before adopting the base.
        .when(active.status == PulsationStatus::Failed, |panel| {
            panel.child(cleanup_panel(model, cx))
        })
        .child(coordination_panel(model, cx))
        .child(composition_panel(model, cx))
        .child(crate::usage::pulsation_usage_panel(model, cx))
        .child(
            div()
                .grid()
                .grid_cols(3)
                .gap_2()
                .my_4()
                .child(fact("CURRENT STAGE", stage_label(active.stage)))
                .child(fact("ACTIVE AGENTS", running_jobs.to_string()))
                .child(fact("PENDING HITL", awaiting_hitl.to_string())),
        )
        .child(fact(
            "CREATED AT",
            crate::time::format_timestamp(&active.created_at),
        ))
        .child(fact(
            "LAST TRANSITION",
            crate::time::format_timestamp(&active.updated_at),
        ))
        .into_any_element()
}

fn detail_artifacts(
    model: &AppModel,
    artifacts: Vec<hvc_core::persistence_service::Artifact>,
    artifacts_loading: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    div()
        .child(artifact_preview_panel(model, cx))
        .child(
            div()
                .mt_5()
                .child(section_heading(
                    "GENERATED OUTPUT",
                    "Artifacts",
                    artifacts.len(),
                ))
                .when(artifacts_loading, |section| {
                    section.child(
                        div()
                            .py_3()
                            .text_color(theme::text_muted())
                            .child("Loading artifacts…"),
                    )
                })
                .when(!artifacts_loading && artifacts.is_empty(), |section| {
                    section.child(
                        div()
                            .py_3()
                            .text_color(theme::text_muted())
                            .child("No artifacts generated yet."),
                    )
                })
                .children(artifacts.into_iter().map(|artifact| {
                    let pulse = artifact.pulsation_id.clone();
                    let artifact_id = artifact.id.clone();
                    div()
                        .id(gpui::SharedString::from(format!(
                            "artifact:{}",
                            artifact.id
                        )))
                        .mt_2()
                        .p_3()
                        .rounded_md()
                        .border_1()
                        .border_color(theme::border())
                        .bg(theme::surface_raised())
                        .cursor_pointer()
                        .on_click(cx.listener(move |model, _, _, cx| {
                            model.preview_artifact(pulse.clone(), artifact_id.clone(), cx)
                        }))
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(artifact.name),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::text_muted())
                                .child(format!(
                                    "{} · {}",
                                    artifact.kind,
                                    artifact
                                        .relative_path
                                        .unwrap_or_else(|| { "no workspace path".into() })
                                )),
                        )
                })),
        )
        .into_any_element()
}

fn pulsation_card(
    pulsation: Pulsation,
    active_id: Option<&str>,
    queue_label: Option<String>,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let selected = active_id == Some(pulsation.id.as_str());
    let id = pulsation.id.clone();
    let title_selector = format!("pulsation-title:{id}");
    div()
        .id(gpui::SharedString::from(format!("pulsation:{id}")))
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
        .cursor_pointer()
        .on_click(cx.listener(move |model, _, _, cx| {
            model.select_pulsation(id.clone(), cx);
        }))
        .child(
            div()
                .flex()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .debug_selector(move || title_selector.clone())
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(crate::fading_title::FadingTitle::new(
                            pulsation.feature_name,
                            if selected {
                                theme::accent_soft()
                            } else {
                                theme::surface_raised()
                            },
                        )),
                )
                .child(status_dot(pulsation.status)),
        )
        .child(
            div()
                .mt_1()
                .text_xs()
                .text_color(theme::text_muted())
                .child(format!(
                    "{} · {}",
                    stage_label(pulsation.stage),
                    status_label(pulsation.status, pulsation.terminal_reason.as_deref())
                )),
        )
        .when_some(queue_label, |card, label| {
            card.child(
                div()
                    .mt_1()
                    .text_xs()
                    .text_color(theme::warning())
                    .child(label),
            )
        })
        .into_any_element()
}

fn preparation_panel(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    use hvc_core::orchestration::{DescriptionReviewState, PreSpecSkipReason, PreSpecState};
    use hvc_core::persistence_service::HitlDecision;
    let state = model.pulsations.read(cx);
    let Some(detail) = state.detail().cloned() else {
        return div().into_any_element();
    };
    let busy = state.action_busy;
    let artifact_review = state.hitl.expected_checkpoint.is_some();
    let review_busy = state.hitl.busy;
    let prep = &detail.preparation;
    let main_actionable = !matches!(
        detail.status,
        PulsationStatus::Done | PulsationStatus::Failed
    );
    let description_pending =
        main_actionable && prep.description_review == DescriptionReviewState::Pending;
    let label = match prep.prespec_state {
        PreSpecState::NotScheduled => "Not scheduled",
        PreSpecState::Pending => "Waiting for local slot",
        PreSpecState::Running => "Collecting local context",
        PreSpecState::Ready => "Bundle ready for revalidation",
        PreSpecState::Skipped => "Preparation skipped",
        PreSpecState::Stale => "Stale bundle; current context used",
        PreSpecState::Failed => "Collection failed; on-demand context",
        PreSpecState::Cancelled => "Preparation cancelled; on-demand context",
    };
    let reason = prep.skip_reason.map(|r| match r {
        PreSpecSkipReason::Disabled => "Disabled in settings",
        PreSpecSkipReason::SkippedNoPredecessor => "No predecessor",
        PreSpecSkipReason::AmbiguousRequest => "Request still ambiguous after user skip",
        PreSpecSkipReason::NoMatches => "No matches in limited scope",
        PreSpecSkipReason::BaseReleased => "Base released; Specification does not wait",
    });
    let mut panel = div().mt_4().flex().flex_col().gap_3();

    let mut prespec_card = prep_card("PREPARATION & LOCAL CONTEXT")
        .child(format!("Revision {} · {}", detail.input_revision, label))
        .when_some(reason, |p, r| {
            p.child(div().text_sm().text_color(theme::text_muted()).child(r))
        })
        .when_some(detail.context_disposition.clone(), |p, d| {
            p.child(format!("Selected context: {d}"))
        })
        .when_some(detail.diagnostic.clone(), |p, d| {
            p.child(div().text_sm().child(d))
        })
        .child(
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child("Contextual code does not require vault or AST."),
        );

    if let Some(assessment) = &prep.assessment {
        prespec_card = prespec_card.child(format!(
            "TaskRefine: {} / 100 · threshold {} · {}",
            assessment.score.get(),
            assessment.threshold.get(),
            assessment.ruleset_version
        ));
        for contribution in &assessment.contributions {
            prespec_card = prespec_card.child(div().text_sm().child(format!(
                "+{} — {}",
                contribution.weight, contribution.reason
            )));
        }
    }
    panel = panel.child(prespec_card);

    if description_pending || (main_actionable && detail.description_editable) {
        let mut desc_card = prep_card("REQUEST REVIEW");
        if description_pending {
            desc_card = desc_card.child(
                div().text_sm().text_color(theme::warning()).child(
                    "Awaiting your review: edit the request or confirm to proceed. This does not approve the specification.",
                ),
            );
        }
        if main_actionable && detail.description_editable {
            desc_card = desc_card
                .child(model.description_review_input.clone())
                .child(preparation_validation(model, "description", cx))
                .child(preparation_button(
                    "description-edit",
                    "Edit request",
                    !busy,
                    cx,
                    |m, cx| m.revise_description(cx),
                ));
        }
        if description_pending {
            desc_card = desc_card.child(preparation_button(
                "description-confirm",
                "Continue to specification",
                !busy,
                cx,
                |m, cx| m.confirm_description(cx),
            ));
        }
        panel = panel.child(desc_card);
    }

    if let Some(questions) = detail.questions.filter(|_| main_actionable) {
        let question_set_id = questions.id.clone();
        let mut questions_card = prep_card("SPECIFICATION · PENDING QUESTIONS")
            .child(div().text_sm().child(
                "Questions produced by Specification and presented in Companion without additional LLM calls. No complete specification to approve yet.",
            ));
        let mut complete = true;
        for question in questions.questions {
            questions_card = questions_card
                .child(format!("[{}] {}", question.id, question.question))
                .child(
                    div()
                        .text_sm()
                        .text_color(theme::text_muted())
                        .child(question.rationale),
                );
            if let Some((_, input)) = model
                .specification_answer_inputs
                .iter()
                .find(|(id, _)| id == &question.id)
            {
                complete &= !input.read(cx).value().trim().is_empty();
                questions_card = questions_card.child(input.clone());
            } else {
                complete = false;
            }
        }
        questions_card = questions_card
            .child(preparation_validation(model, "answers", cx))
            .child(preparation_button(
                "specification-answer",
                "Submit answers and resume Specification",
                !busy && complete,
                cx,
                |m, cx| m.answer_specification(cx),
            ))
            .child(preparation_button(
                "specification-conversation",
                "Companion questions",
                true,
                cx,
                |m, cx| m.open_specification_conversation(cx),
            ))
            .child(preparation_button(
                "resolution-conversation",
                "Resolution conversation",
                true,
                cx,
                move |m, cx| m.toggle_resolution_chip(question_set_id.clone(), cx),
            ));
        panel = panel.child(questions_card);
    } else if artifact_review && main_actionable {
        let confirm_stage_reject = model.confirm_stage_reject;
        let artifact_card = prep_card("ARTIFACT REVIEW & HITL")
            .child(
                div()
                    .text_sm()
                    .child("Inspect the persisted content below before making a decision."),
            )
            .child(model.stage_feedback_input.clone())
            .child(preparation_button(
                "artifact-approve",
                "Approve stage",
                !review_busy,
                cx,
                |m, cx| {
                    m.set_confirm_stage_reject(false, cx);
                    m.apply_hitl_decision(HitlDecision::Approve, cx);
                },
            ))
            .child(preparation_button(
                "artifact-revise",
                "Request revision with feedback",
                !review_busy,
                cx,
                |m, cx| {
                    m.set_confirm_stage_reject(false, cx);
                    m.apply_hitl_decision(HitlDecision::RequestRevision, cx);
                },
            ))
            .child(
                div()
                    .id("artifact-reject")
                    .debug_selector(|| "artifact-reject".into())
                    .p_2()
                    .rounded_md()
                    .tab_index(0)
                    .bg(if confirm_stage_reject {
                        theme::danger()
                    } else {
                        theme::danger_soft()
                    })
                    .text_color(if confirm_stage_reject {
                        theme::canvas()
                    } else {
                        theme::danger()
                    })
                    .text_sm()
                    .opacity(if !review_busy { 1.0 } else { 0.5 })
                    .when(!review_busy, |button| {
                        button
                            .cursor_pointer()
                            .hover(|style| style.opacity(theme::HOVER_OPACITY))
                            .on_click(cx.listener(move |m, _, _, cx| {
                                if confirm_stage_reject {
                                    m.apply_hitl_decision(HitlDecision::Reject, cx);
                                } else {
                                    m.set_confirm_stage_reject(true, cx);
                                }
                            }))
                    })
                    .child(if confirm_stage_reject {
                        "Confirm stage rejection"
                    } else {
                        "Reject stage"
                    }),
            );
        panel = panel.child(artifact_card);
    }

    use hvc_core::orchestration::LearningState;
    let learning_label = match detail.learning {
        LearningState::NotScheduled => "Not scheduled",
        LearningState::Unavailable => "History without runner (will not be reprocessed)",
        LearningState::Pending => "Waiting for priority and 5s connection quiet period",
        LearningState::Running => "Generating advisory proposal",
        LearningState::Preempted => "Preempted by primary demand",
        LearningState::DeferredBudget => "Budget exhausted; awaiting authorization",
        LearningState::Disabled => "Disabled; budget preserved",
        LearningState::Publishing => "Publishing proposal",
        LearningState::Published => "Proposal published; see artifact below",
        LearningState::Conflict => "Publication conflict; files preserved",
        LearningState::Failed => "Optional failure; completed pulsation preserved",
        LearningState::Cancelled => "Report dismissed",
    };

    let mut learning_card = prep_card("ADVISORY LEARNING")
        .child(
            div()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(format!("State: {learning_label}")),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child("Proposals do not alter rules or reopen Done."),
        );

    if let Some(run) = &detail.learning_run {
        learning_card = learning_card
            .child(format!(
                "Calls: {}/3 in this version · {} total in history",
                run.budget_calls, run.total_calls
            ))
            .when_some(run.diagnostic.clone(), |panel, diagnostic| {
                panel.child(div().text_sm().child(diagnostic))
            });
        for audit in &run.context_audit {
            let citation_rate = if audit.delivered == 0 {
                "n/a".to_string()
            } else {
                format!(
                    "{:.0}%",
                    f64::from(audit.cited) * 100.0 / f64::from(audit.delivered)
                )
            };
            let determinate = audit.audited.saturating_sub(audit.indeterminate);
            let observed_precision = if determinate == 0 {
                "n/a".to_string()
            } else {
                format!(
                    "{:.0}%",
                    f64::from(audit.observed_use) * 100.0 / f64::from(determinate)
                )
            };
            learning_card = learning_card.child(
                div().text_xs().text_color(theme::text_muted()).child(format!(
                    "{} · pool {} · delivered {} · cited {} · audited {} · indeterminate {} · citation rate {} · heuristic observed precision {}",
                    audit.stage_name,audit.considered,audit.delivered,audit.cited,audit.audited,audit.indeterminate,citation_rate,observed_precision
                )),
            );
        }
        let mut has_pending_proposals = false;
        for proposal in &run.graph_proposals {
            learning_card = learning_card.child(
                div()
                    .mt_2()
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(theme::border())
                    .child(format!(
                        "{} · {} → {}",
                        proposal.relation, proposal.source_path, proposal.target_path
                    ))
                    .child(div().text_sm().child(proposal.rationale.clone()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::text_muted())
                            .child(format!("Evidence: {}", proposal.evidence_ids.join(", "))),
                    )
                    .child(
                        div()
                            .mt_1()
                            .p_2()
                            .bg(theme::surface_raised())
                            .text_xs()
                            .child(proposal.preview_diff.clone()),
                    )
                    .child(format!(
                        "Decision: {} · publication: {}",
                        proposal.decision, proposal.publication_state
                    ))
                    .when_some(proposal.diagnostic.clone(), |card, diagnostic| {
                        card.child(
                            div()
                                .text_xs()
                                .text_color(theme::warning())
                                .child(diagnostic),
                        )
                    }),
            );
            if proposal.decision == "pending" {
                has_pending_proposals = true;
                let approve_id = proposal.id.clone();
                let reject_id = proposal.id.clone();
                let selected = model.learning_graph_decisions.get(&proposal.id).copied();
                learning_card = learning_card.child(
                    div()
                        .flex()
                        .gap_2()
                        .child(learning_decision_button(
                            format!("learning-approve:{}", proposal.id),
                            "Approve".into(),
                            selected
                                == Some(hvc_core::orchestration::LearningGraphDecision::Approve),
                            !busy,
                            cx,
                            move |model, cx| {
                                model.choose_learning_graph_decision(
                                    approve_id.clone(),
                                    hvc_core::orchestration::LearningGraphDecision::Approve,
                                    cx,
                                )
                            },
                        ))
                        .child(learning_decision_button(
                            format!("learning-reject:{}", proposal.id),
                            "Reject".into(),
                            selected
                                == Some(hvc_core::orchestration::LearningGraphDecision::Reject),
                            !busy,
                            cx,
                            move |model, cx| {
                                model.choose_learning_graph_decision(
                                    reject_id.clone(),
                                    hvc_core::orchestration::LearningGraphDecision::Reject,
                                    cx,
                                )
                            },
                        )),
                );
            }
        }
        if has_pending_proposals {
            learning_card = learning_card.child(preparation_button(
                "learning-graph-confirm",
                "Confirm graph decisions",
                !busy,
                cx,
                |model, cx| model.submit_learning_graph_review(cx),
            ));
        }
        if matches!(
            detail.learning,
            LearningState::Failed | LearningState::DeferredBudget
        ) && run.artifact_id.is_none()
        {
            learning_card = learning_card.child(preparation_button(
                "learning-retry",
                "Authorize up to 3 new calls",
                !busy,
                cx,
                |m, cx| m.learning_action(true, cx),
            ));
        }
        if !matches!(
            detail.learning,
            LearningState::Published | LearningState::Cancelled
        ) {
            learning_card = learning_card.child(preparation_button(
                "learning-dismiss",
                "Dismiss report",
                !busy,
                cx,
                |m, cx| m.learning_action(false, cx),
            ));
        }
    }
    panel = panel.child(learning_card);

    panel.into_any_element()
}

fn preparation_validation(model: &AppModel, field: &'static str, cx: &gpui::App) -> AnyElement {
    let message = model
        .pulsations
        .read(cx)
        .validation_error
        .as_ref()
        .filter(|(target, _)| *target == field)
        .map(|(_, message)| message.clone())
        .unwrap_or_default();
    div()
        .id(gpui::SharedString::from(format!(
            "preparation-validation-{field}"
        )))
        .h(px(40.0))
        .flex_shrink_0()
        .overflow_y_scroll()
        .text_sm()
        .text_color(theme::danger())
        .child(message)
        .into_any_element()
}

fn prep_card(title: &'static str) -> gpui::Div {
    div()
        .p_3()
        .rounded_lg()
        .border_1()
        .border_color(theme::border())
        .bg(theme::surface_raised())
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::accent())
                .child(title),
        )
}

fn preparation_button(
    id: &'static str,
    label: &'static str,
    enabled: bool,
    cx: &mut Context<AppModel>,
    action: impl Fn(&mut AppModel, &mut Context<AppModel>) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .self_start()
        .flex_shrink_0()
        .p_2()
        .rounded_md()
        .tab_index(0)
        .bg(theme::surface_raised())
        .text_sm()
        .opacity(if enabled { 1.0 } else { 0.5 })
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .hover(|style| style.opacity(theme::HOVER_OPACITY))
                .on_click(cx.listener(move |m, _, _, cx| action(m, cx)))
        })
        .child(label)
}

fn learning_decision_button(
    id: String,
    label: String,
    selected: bool,
    enabled: bool,
    cx: &mut Context<AppModel>,
    action: impl Fn(&mut AppModel, &mut Context<AppModel>) + 'static,
) -> impl IntoElement {
    div()
        .id(gpui::SharedString::from(id))
        .self_start()
        .flex_shrink_0()
        .px_3()
        .py_2()
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
        .text_sm()
        .opacity(if enabled { 1.0 } else { 0.5 })
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .on_click(cx.listener(move |model, _, _, cx| action(model, cx)))
        })
        .child(label)
}

fn artifact_preview_panel(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let Some((id, content)) = model.pulsations.read(cx).artifact_preview.clone() else {
        return div().into_any_element();
    };
    let pages = content.chars().count().max(1).div_ceil(8000);
    let page = model.artifact_preview_page.min(pages - 1);
    let text: String = content.chars().skip(page * 8000).take(8000).collect();
    div()
        .mt_3()
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(theme::accent())
        .flex()
        .flex_col()
        .gap_2()
        .child(format!(
            "Persisted content · {id} · page {}/{}",
            page + 1,
            pages
        ))
        .child(div().id("artifact-preview-scroll").text_sm().child(text))
        .child(preparation_button(
            "artifact-prev",
            "Previous page",
            page > 0,
            cx,
            |m, cx| {
                m.artifact_preview_page = m.artifact_preview_page.saturating_sub(1);
                cx.notify();
            },
        ))
        .child(preparation_button(
            "artifact-next",
            "Next page",
            page + 1 < pages,
            cx,
            |m, cx| {
                m.artifact_preview_page += 1;
                cx.notify();
            },
        ))
        .child(preparation_button(
            "artifact-copy",
            "Copy full content",
            true,
            cx,
            move |_, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string(content.clone())),
        ))
        .child(preparation_button(
            "artifact-close",
            "Close content",
            true,
            cx,
            |m, cx| {
                m.pulsations.update(cx, |p, _| p.artifact_preview = None);
                cx.notify();
            },
        ))
        .into_any_element()
}

fn blocking_label(reason: Option<&str>) -> &'static str {
    match reason {
        Some("waiting_for_base") => "Waiting for previous base",
        Some("waiting_for_user") => "Awaiting your action",
        Some("needs_review" | "legacy_owner_selection") => "Reconciliation required",
        Some("publishing") => "Checking publications",
        _ => "Persisted queue",
    }
}

fn brownfield_progress_panel(coverage: BrownfieldCoverageMetrics) -> impl IntoElement {
    let basis_points = coverage
        .extraction_progress_basis_points
        .min(BrownfieldCoverageMetrics::SCALE_BASIS_POINTS);
    let progress = basis_points as f32 / BrownfieldCoverageMetrics::SCALE_BASIS_POINTS as f32;
    let percentage = format!("{:.2}%", basis_points as f32 / 100.0);
    let bootstrap = if coverage.frontier_initialized {
        "Initial structural inventory completed"
    } else {
        "Initial structural inventory pending"
    };

    div()
        .id("brownfield-progress")
        .mb_3()
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(theme::border())
        .bg(theme::surface_raised())
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child("BROWNFIELD MAPPING"),
                )
                .child(
                    div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(percentage),
                ),
        )
        .child(
            div()
                .mt_2()
                .h(px(8.0))
                .w_full()
                .rounded_full()
                .overflow_hidden()
                .bg(theme::canvas())
                .child(
                    div()
                        .h_full()
                        .w(relative(progress))
                        .rounded_full()
                        .bg(theme::accent()),
                ),
        )
        .child(div().mt_2().text_sm().child(format!(
            "{} of {} eligible sources with valid extraction",
            coverage.extracted_valid_sources, coverage.eligible_source_denominator
        )))
        .child(
            div()
                .mt_1()
                .text_xs()
                .text_color(theme::text_muted())
                .child("Denominator: known sources in inventory minus ignored sources."),
        )
        .child(
            div()
                .mt_1()
                .text_xs()
                .text_color(theme::text_muted())
                .child(bootstrap),
        )
        .child(
            div()
                .mt_2()
                .flex()
                .flex_wrap()
                .gap_x_3()
                .gap_y_1()
                .text_xs()
                .text_color(theme::text_muted())
                .child(format!(
                    "Extraction: {} partial · {} stale · {} failed",
                    coverage.extracted_partial_sources,
                    coverage.extracted_stale_sources,
                    coverage.extraction_failed_sources
                ))
                .child(format!(
                    "Documentation: {} proposed · {} validated · {} stale",
                    coverage.documentation_proposed_sources,
                    coverage.documentation_validated_sources,
                    coverage.documentation_stale_sources
                ))
                .child(format!(
                    "Relations: {} resolved · {} ambiguous · {} external · {} pending{}",
                    coverage.resolved_references,
                    coverage.ambiguous_references,
                    coverage.external_references,
                    coverage.unresolved_references,
                    coverage
                        .resolver_version
                        .as_deref()
                        .map_or(String::new(), |version| format!(" · {version}"))
                ))
                .child(format!("Ignored: {}", coverage.ignored_sources)),
        )
}

fn coordination_panel(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let state = model.pulsations.read(cx);
    let Some(snapshot) = state.coordination.clone() else {
        return div().into_any_element();
    };
    let active = state.active_id();
    let blocked = snapshot.pipeline.base_state == BaseState::NeedsReview;
    let input = model.reconciliation_input.clone();
    let can_confirm = blocked
        && !state.reconciliation_busy
        && !input.read(cx).value().trim().is_empty()
        && (snapshot.owner_candidates.is_empty()
            || active
                .as_ref()
                .is_some_and(|id| snapshot.owner_candidates.contains(id)));
    let candidates: Vec<_> = snapshot
        .owner_candidates
        .iter()
        .filter_map(|id| state.pulsations.iter().find(|p| &p.id == id).cloned())
        .collect();
    div().mb_3().p_3().rounded_md().border_1().border_color(if blocked {theme::warning()} else {theme::border()})
        .child(div().text_sm().child(format!("Base #{} · {} · owner: {}",snapshot.pipeline.base_epoch,
            match snapshot.pipeline.base_state {BaseState::Ready=>"consistent",BaseState::Publishing=>"publishing in progress",BaseState::NeedsReview=>"review required"},
            snapshot.pipeline.active_pulsation_id.as_deref().unwrap_or("idle"))))
        .when(blocked, |panel| panel
            .child(div().mt_2().text_color(theme::warning()).child(snapshot.pipeline.blocking_reason.unwrap_or_default()))
            .child(div().mt_2().text_sm().child("Inspect files in your external editor; the app editor opens .md/.txt. Adopting the current base does not undo changes or declare success. Choose which work to resume; others will remain preserved and blocked."))
            .children(candidates.into_iter().map(|p| {
                let id=p.id.clone();
                div().id(gpui::SharedString::from(format!("resume:{}",p.id))).mt_2().p_2().rounded_md()
                    .bg(if active.as_deref()==Some(p.id.as_str()) {theme::accent_soft()} else {theme::surface_raised()})
                    .cursor_pointer().on_click(cx.listener(move |m,_,_,cx| m.select_pulsation(id.clone(),cx)))
                    .child(format!("Resume: {} · {}",p.feature_name,stage_label(p.stage)))
            }))
            .children(snapshot.publications.into_iter().map(|p| {
                let path=p.relative_path.clone();
                let openable=path.to_ascii_lowercase().ends_with(".md") || path.to_ascii_lowercase().ends_with(".txt");
                div().id(gpui::SharedString::from(format!("review:{}",p.artifact_id))).mt_2().p_2().bg(theme::surface_raised())
                    .when(openable,|row|row.cursor_pointer().on_click(cx.listener(move |m,_,_,cx| m.open_workspace_file(path.clone(),cx))))
                    .child(div().text_sm().child(format!("{} · {}",p.relative_path,p.state)))
                    .child(div().text_xs().child(format!("Desired hash: {}",p.desired_sha256.unwrap_or_else(||"unavailable".into()))))
                    .child(div().text_xs().child(format!("Observed hash: {}",p.observed_sha256.unwrap_or_else(||"missing or unreadable".into()))))
                    .when_some(p.diagnostic,|row,error|row.child(div().text_xs().text_color(theme::danger()).child(error)))
                    .when_some(p.observation_error,|row,error|row.child(div().text_xs().text_color(theme::danger()).child(error)))
            }))
            .child(crate::sizing::text_area(input).mt_2())
            .child(div().mt_2().flex().gap_2()
                .child(div().id("refresh-base-review").p_2().cursor_pointer().bg(theme::surface_raised())
                    .on_click(cx.listener(|m,_,_,cx|m.refresh_pulsations(cx))).child("Refresh review"))
                .child(div().id("confirm-base-review").p_2().bg(if can_confirm {theme::accent_soft()} else {theme::surface_raised()})
                    .when(can_confirm,|b|b.cursor_pointer().on_click(cx.listener(|m,_,_,cx|m.reconcile_project_base(cx))))
                    .child("Reviewed: adopt current base"))))
        .into_any_element()
}

/// "Leftover files" of a failed pulsation: the host's dry run of what the
/// cleanup would remove (files the pulsation itself created and never edited
/// since) and what it deliberately keeps. Removal takes two clicks and never
/// touches kept files; the base review stays the explicit step afterwards.
fn cleanup_panel(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    use hvc_core::orchestration::CleanupDisposition;
    let state = model.pulsations.read(cx);
    let cleanup = &state.cleanup;
    let plan = cleanup.plan.clone();
    let loading = cleanup.loading;
    let busy = cleanup.busy;
    let armed = cleanup.armed;
    let can_remove = cleanup.can_remove();
    let error = cleanup.error.clone();
    let removable = plan.as_ref().map_or(0, |plan| plan.removable_files());
    let kept = plan.as_ref().map_or(0, |plan| plan.kept_files());
    let summary = match &plan {
        _ if loading && plan.is_none() => "Reading the workspace…".to_string(),
        Some(plan) if plan.is_noop() && plan.entries.is_empty() => {
            "Nothing left on disk from this pulsation.".to_string()
        }
        Some(plan) => format!(
            "This pulsation published {} file(s): {} can be removed, {} kept.",
            plan.entries.len(),
            removable,
            kept
        ),
        None => "Preview unavailable.".to_string(),
    };
    let remove_label = if busy {
        "Removing…".to_string()
    } else if armed {
        "Confirm removal".to_string()
    } else {
        format!("Remove {removable} file(s)")
    };
    div()
        .id("pulsation-cleanup")
        .debug_selector(|| "pulsation-cleanup".into())
        .mb_3()
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(theme::border())
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child("Leftover files"),
        )
        .child(div().text_sm().child(summary))
        .when_some(plan.clone(), |card, plan| {
            card.children(plan.entries.into_iter().map(|entry| {
                let (label, color) = match entry.disposition {
                    CleanupDisposition::Remove => ("will be removed", theme::danger()),
                    CleanupDisposition::AlreadyAbsent => ("already gone", theme::text_muted()),
                    CleanupDisposition::KeepPreexisting => {
                        ("kept: existed before this pulsation", theme::warning())
                    }
                    CleanupDisposition::KeepEdited => {
                        ("kept: edited after publication", theme::warning())
                    }
                    CleanupDisposition::KeepUnreadable => ("kept: unreadable", theme::warning()),
                };
                div()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(theme::surface_raised())
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_x_2()
                    .child(div().text_sm().child(entry.relative_path))
                    .child(div().text_xs().text_color(color).child(label))
                    .when_some(entry.detail, |row, detail| {
                        row.child(div().text_xs().text_color(theme::text_muted()).child(detail))
                    })
            }))
            .when(!plan.candidate_dirs.is_empty(), |card| {
                card.child(div().text_xs().text_color(theme::text_muted()).child(format!(
                    "+ {} disposable staging dir(s) in app data",
                    plan.candidate_dirs.len()
                )))
            })
        })
        .when_some(error, |card, error| {
            card.child(div().text_xs().text_color(theme::danger()).child(error))
        })
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(crate::widgets::btn(
                    "pulsation-cleanup-remove",
                    remove_label,
                    crate::widgets::ButtonVariant::Danger,
                    can_remove,
                    cx,
                    |model, cx| model.request_cleanup_failed_files(cx),
                ))
                .child(crate::widgets::btn(
                    "pulsation-cleanup-refresh",
                    if loading { "Refreshing…" } else { "Refresh" },
                    crate::widgets::ButtonVariant::Secondary,
                    !loading && !busy,
                    cx,
                    |model, cx| model.reload_cleanup_preview(cx),
                )),
        )
        .child(div().text_xs().text_color(theme::text_muted()).child(
            "Only files this pulsation created and never edited since are removed. Kept files stay in the base review; adopting the base remains an explicit step.",
        ))
        .into_any_element()
}

/// The composed execution of the active pulsation's current stage (pipeline
/// v4, ADR-022): units with their real states, attempts, effective model and
/// real versus reserved usage, host checks apart from authored coverage, and
/// the remaining shared budget. Child results have no approval control here
/// (SC-021): the human decides on the stage's Markdown review.
fn composition_panel(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    use hvc_core::orchestration::composition::{
        AttemptState, CheckStatus, ExecutionStatus, UnitState, UsageCompleteness,
    };
    let state = model.pulsations.read(cx);
    let Some(snapshot) = state
        .detail()
        .and_then(|detail| detail.stage_execution.clone())
    else {
        return div().into_any_element();
    };
    let status = match snapshot.status {
        ExecutionStatus::Planned => "planned",
        ExecutionStatus::Running => "running",
        ExecutionStatus::Validating => "validating",
        ExecutionStatus::Ready => "ready",
        ExecutionStatus::Accepted => "accepted",
        ExecutionStatus::Parked => "parked · awaiting a decision",
        ExecutionStatus::Cancelled => "cancelled",
        ExecutionStatus::Superseded => "superseded",
        ExecutionStatus::Failed => "failed",
    };
    let tokens = |value: Option<u32>| {
        value
            .map(|v| v.to_string())
            .unwrap_or_else(|| "unavailable".into())
    };
    let completeness = match snapshot.usage.completeness {
        UsageCompleteness::Complete => "complete",
        UsageCompleteness::Partial => "partial",
        UsageCompleteness::Unavailable => "unavailable",
    };
    let budget = snapshot.remaining_budget.clone();
    div().mb_3().p_3().rounded_md().border_1().border_color(theme::border())
        .debug_selector(|| "composition-panel".into())
        .child(div().text_sm().child(format!(
            "Composed stage (v4) · {} · {status} · generation {} · execution v{}",
            snapshot.stage.as_str(), snapshot.parent_generation, snapshot.execution_version
        )))
        .when_some(snapshot.blocked_reason.clone(), |panel, reason| panel
            .child(div().mt_1().text_xs().text_color(theme::warning()).child(reason)))
        .child(div().mt_2().text_xs().text_color(theme::text_muted()).child(format!(
            "Usage ({completeness}): input {} · output {} · reasoning {} · cached {} — from the ledger, never an estimate.",
            tokens(snapshot.usage.input_tokens), tokens(snapshot.usage.output_tokens),
            tokens(snapshot.usage.reasoning_tokens), tokens(snapshot.usage.cached_input_tokens)
        )))
        .child(div().mt_1().text_xs().text_color(theme::text_muted()).child(format!(
            "Remaining budget: {} initial / {} extra invocation(s) · {} / {} output tokens · {} / {} prompt characters.",
            budget.initial_invocations, budget.extra_invocations,
            budget.initial_output_tokens, budget.extra_output_tokens,
            budget.initial_prompt_characters, budget.extra_prompt_characters
        )))
        .child(div().mt_2().text_xs().text_color(theme::text_muted()).child("UNITS"))
        .children(snapshot.units.iter().map(|unit| {
            let unit_state = match unit.state {
                UnitState::Pending => "pending",
                UnitState::Running => "running",
                UnitState::Validated => "validated by the host",
                UnitState::Failed => "failed",
                UnitState::Cancelled => "cancelled",
                UnitState::Invalidated => "invalidated",
            };
            let origin = if unit.reused_from_attempt_id.is_some() {
                " · reused (no call)"
            } else if unit.attempts.is_empty() && unit.state == UnitState::Validated {
                " · synthesized by the host (no call)"
            } else {
                ""
            };
            let selector = format!("composition-unit-{}", unit.logical_unit_key.as_str());
            div().mt_1().p_2().rounded_md().bg(theme::surface_raised())
                .debug_selector(move || selector.clone())
                .child(div().text_sm().child(format!(
                    "`{}` · {} · gen {} · {unit_state}{origin}",
                    unit.logical_unit_key.as_str(), unit.executor_kind.as_str(), unit.generation
                )))
                .children(unit.attempts.iter().enumerate().map(|(index, attempt)| {
                    let attempt_state = match attempt.state {
                        AttemptState::Reserved => "reserved",
                        AttemptState::Running => "running",
                        AttemptState::Succeeded => "succeeded",
                        AttemptState::Rejected => "rejected",
                        AttemptState::Interrupted => "interrupted",
                        AttemptState::Cancelled => "cancelled",
                    };
                    div().text_xs().text_color(theme::text_muted()).child(format!(
                        "attempt {} · {} · model {} · {attempt_state}{} · in {} / out {}",
                        index + 1,
                        attempt.executor_kind.as_str(),
                        attempt.effective_model,
                        attempt.error_code.as_ref().map(|c| format!(" ({})", c.as_str())).unwrap_or_default(),
                        tokens(attempt.usage.input_tokens),
                        tokens(attempt.usage.output_tokens)
                    ))
                }))
        }))
        .when(!snapshot.checks.is_empty(), |panel| panel
            .child(div().mt_2().text_xs().text_color(theme::text_muted()).child("HOST CHECKS (separate from authored coverage)"))
            .children(snapshot.checks.iter().map(|check| {
                let (label, color) = match check.status {
                    CheckStatus::Passed => ("passed", theme::success()),
                    CheckStatus::Failed => ("failed", theme::danger()),
                    CheckStatus::TimedOut => ("timed out", theme::danger()),
                    CheckStatus::Cancelled => ("cancelled", theme::warning()),
                    CheckStatus::Unavailable => ("unavailable", theme::warning()),
                    CheckStatus::Error => ("runner error", theme::danger()),
                    CheckStatus::NotRun => ("not run", theme::text_muted()),
                    CheckStatus::Pending => ("pending", theme::text_muted()),
                    CheckStatus::Running => ("running", theme::text_muted()),
                };
                div().mt_1().text_xs().text_color(color).child(format!(
                    "`{}` · {} · {label}{}",
                    check.check_ref.as_str(),
                    if check.required { "required" } else { "optional" },
                    check.receipt_id.as_ref().map(|id| format!(" · receipt {}", id.as_str().chars().take(8).collect::<String>())).unwrap_or_default()
                ))
            })))
        .into_any_element()
}

fn pulsation_detail_header(
    pulsation: Pulsation,
    busy: bool,
    backend_override: Option<hvc_core::terminals::BackendOverrideStatus>,
    backend_override_busy: bool,
    cx: &mut Context<AppModel>,
) -> impl IntoElement {
    let busy = busy
        || matches!(
            pulsation.status,
            PulsationStatus::Done | PulsationStatus::Failed
        );
    div()
        .flex()
        .flex_col()
        .w_full()
        .items_start()
        .gap_3()
        .child(
            div()
                .w_full()
                .min_w_0()
                .overflow_hidden()
                .debug_selector(|| "pulsation-heading".into())
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child("SELECTED PULSATION"),
                )
                .child(
                    div()
                        .text_2xl()
                        .whitespace_normal()
                        .font_weight(gpui::FontWeight::BOLD)
                        .child(pulsation.feature_name),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(pulsation.id),
                ),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .max_w_full()
                .items_center()
                .gap_2()
                .child(status_pill(
                    pulsation.status,
                    pulsation.terminal_reason.as_deref(),
                ))
                .child(
                    div()
                        .id("cancel-pulsation-jobs")
                        .debug_selector(|| "pulsation-cancel".into())
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .bg(if busy {
                            theme::surface_raised()
                        } else {
                            theme::danger_soft()
                        })
                        .text_color(if busy {
                            theme::text_muted()
                        } else {
                            theme::danger()
                        })
                        .when(!busy, |button| {
                            button.cursor_pointer().on_click(
                                cx.listener(|model, _, _, cx| {
                                    model.cancel_active_pulsation_jobs(cx)
                                }),
                            )
                        })
                        .child("Cancel pulsation"),
                ),
        )
        .when_some(backend_override, |header, backend_override| {
            let can_retry =
                !backend_override_busy && backend_override.job_status.as_deref() == Some("pending");
            header.child(
                div()
                    .w_full()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .p_2()
                    .rounded_md()
                    .bg(theme::warning_soft())
                    .debug_selector(|| "pulsation-backend-override".into())
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .min_w_0()
                            .child(div().text_sm().text_color(theme::warning()).child(format!(
                                "Running on {} (automatic failover)",
                                backend_override.backend.label()
                            )))
                            .when_some(backend_override.reason.clone(), |col, reason| {
                                col.child(
                                    div()
                                        .text_xs()
                                        .text_color(theme::text_muted())
                                        .child(reason),
                                )
                            }),
                    )
                    .child(
                        div()
                            .id("clear-backend-override")
                            .debug_selector(|| "pulsation-clear-override".into())
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .bg(if can_retry {
                                theme::accent_soft()
                            } else {
                                theme::surface_raised()
                            })
                            .text_color(if can_retry {
                                theme::accent()
                            } else {
                                theme::text_muted()
                            })
                            .when(can_retry, |button| {
                                button
                                    .cursor_pointer()
                                    .on_click(cx.listener(|model, _, _, cx| {
                                        model.clear_backend_override_and_retry(cx)
                                    }))
                            })
                            .child(if backend_override_busy {
                                "Clearing…"
                            } else {
                                "Clear override and retry"
                            }),
                    ),
            )
        })
}

fn pipeline(current_stage: PulsationStage) -> impl IntoElement {
    let current = PIPELINE_STAGES
        .iter()
        .position(|stage| *stage == current_stage)
        .unwrap_or_default();
    div()
        .mt_5()
        .flex()
        .items_start()
        .gap_1()
        .children(
            PIPELINE_STAGES
                .into_iter()
                .enumerate()
                .map(|(index, stage)| {
                    let complete = index < current;
                    let active = index == current;
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_1()
                        .child(
                            div()
                                .size_7()
                                .rounded_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .bg(if complete || active {
                                    theme::accent_soft()
                                } else {
                                    theme::surface_raised()
                                })
                                .text_color(if complete || active {
                                    theme::accent()
                                } else {
                                    theme::text_muted()
                                })
                                .child(if complete {
                                    "✓".into()
                                } else {
                                    (index + 1).to_string()
                                }),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(if active {
                                    theme::text()
                                } else {
                                    theme::text_muted()
                                })
                                .child(stage_label(stage)),
                        )
                }),
        )
}

fn status_dot(status: PulsationStatus) -> impl IntoElement {
    div().size_2().rounded_full().bg(status_color(status))
}

fn status_pill(status: PulsationStatus, terminal_reason: Option<&str>) -> impl IntoElement {
    div()
        .px_3()
        .py_1()
        .rounded_full()
        .bg(theme::surface_raised())
        .text_color(status_color(status))
        .child(status_label(status, terminal_reason))
}

fn status_color(status: PulsationStatus) -> gpui::Rgba {
    match status {
        PulsationStatus::Done => theme::accent(),
        PulsationStatus::Failed => theme::danger(),
        PulsationStatus::Checkpoint => theme::warning(),
        PulsationStatus::Idle | PulsationStatus::Planning | PulsationStatus::Running => {
            gpui::rgb(0x73b7ff)
        }
    }
}

fn section_heading(eyebrow: &'static str, title: &'static str, count: usize) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(eyebrow),
                )
                .child(
                    div()
                        .text_lg()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(title),
                ),
        )
        .child(
            div()
                .min_w(px(24.0))
                .h(px(24.0))
                .px_2()
                .rounded_full()
                .bg(theme::surface_raised())
                .flex()
                .items_center()
                .justify_center()
                .text_xs()
                .child(count.to_string()),
        )
}

fn fact(label: &'static str, value: impl Into<String>) -> impl IntoElement {
    div()
        .mt_2()
        .p_3()
        .rounded_md()
        .bg(theme::surface_raised())
        .child(div().text_xs().text_color(theme::text_muted()).child(label))
        .child(div().mt_1().child(value.into()))
}

fn empty_panel(title: &'static str, description: &'static str) -> impl IntoElement {
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_2()
        .child(
            div()
                .text_lg()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(title),
        )
        .child(
            div()
                .text_color(theme::text_muted())
                .text_sm()
                .child(description),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Entity, Pixels, Render, TestAppContext, Window};

    struct HeaderHarness {
        model: Entity<AppModel>,
        width: Pixels,
    }

    impl Render for HeaderHarness {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let pulse = Pulsation {
                id: "pulse-layout".into(),
                project_id: "project-layout".into(),
                feature_name: "M5 Questions: validate token and reject invalid tokens. ".repeat(6),
                stage: PulsationStage::Specification,
                status: PulsationStatus::Checkpoint,
                terminal_reason: None,
                created_at: String::new(),
                updated_at: String::new(),
            };
            div().w(self.width).child(self.model.update(cx, |_, cx| {
                pulsation_detail_header(pulse, false, None, false, cx).into_any_element()
            }))
        }
    }

    #[gpui::test]
    fn long_description_keeps_cancel_visible_inside_the_detail_width(cx: &mut TestAppContext) {
        for width in [400.0, 680.0] {
            let (_, visual) = cx.add_window_view(|_, cx| HeaderHarness {
                model: cx.new(AppModel::new_for_test),
                width: px(width),
            });
            let title = visual.debug_bounds("pulsation-heading").unwrap();
            let cancel = visual.debug_bounds("pulsation-cancel").unwrap();
            assert!(title.right() <= px(width));
            assert!(cancel.left() >= px(0.0));
            assert!(cancel.right() <= px(width));
            assert!(cancel.top() >= title.bottom());
            assert!(cancel.size.width > px(100.0));
            assert!(cancel.size.height > px(20.0));
        }
    }

    struct PreparationHarness(Entity<AppModel>);

    impl Render for PreparationHarness {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .w(px(680.0))
                .child(self.0.update(cx, |model, cx| preparation_panel(model, cx)))
        }
    }

    #[gpui::test]
    fn terminal_pulses_hide_description_actions_but_keep_optional_learning(
        cx: &mut TestAppContext,
    ) {
        use hvc_core::orchestration::*;
        for status in [
            PulsationStatus::Planning,
            PulsationStatus::Failed,
            PulsationStatus::Done,
        ] {
            let (_, visual) = cx.add_window_view(|_, cx| {
                let model = cx.new(AppModel::new_for_test);
                model.update(cx, |model, cx| {
                    model.pulsations.update(cx, |state, _| {
                        state.pulsations = vec![Pulsation {
                            id: "pulse-terminal".into(),
                            project_id: "project-terminal".into(),
                            feature_name: "improve system".into(),
                            stage: PulsationStage::Intake,
                            status,
                            terminal_reason: (status == PulsationStatus::Failed)
                                .then(|| "user_cancel".into()),
                            created_at: String::new(),
                            updated_at: String::new(),
                        }];
                        state.details = vec![PulsationCoordinationSnapshot {
                            pulsation_id: "pulse-terminal".into(),
                            project_id: "project-terminal".into(),
                            queue_sequence: 1,
                            pipeline_version: 2,
                            input_revision: 1,
                            state_version: 1,
                            stage: PulsationStage::Intake,
                            status,
                            description_editable: true,
                            context_disposition: None,
                            diagnostic: None,
                            blocking_reason: None,
                            preparation: PreparationSnapshot {
                                assessment: None,
                                description_review: DescriptionReviewState::Pending,
                                prespec_state: PreSpecState::NotScheduled,
                                context_artifact_id: None,
                                skip_reason: None,
                            },
                            questions: None,
                            learning: LearningState::DeferredBudget,
                            learning_run: Some(LearningRunSnapshot {
                                job_id: "learning-test".into(),
                                generation: 1,
                                budget_calls: 3,
                                total_calls: 3,
                                budget_grants: 1,
                                artifact_id: None,
                                diagnostic: None,
                                context_audit: vec![],
                                graph_proposals: vec![],
                            }),
                            artifact_ids: vec![],
                            stage_execution: None,
                        }];
                    });
                });
                PreparationHarness(model)
            });
            for selector in ["description-confirm", "description-edit"] {
                assert_eq!(
                    visual.debug_bounds(selector).is_some(),
                    status == PulsationStatus::Planning
                );
            }
            assert!(visual.debug_bounds("learning-retry").is_some());
        }
    }
}
