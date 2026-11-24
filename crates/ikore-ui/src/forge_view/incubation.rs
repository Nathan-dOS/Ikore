use super::{
    agent_message_card, button_variant, gated_button_variant, label, state, task_kind_label,
    turn_failure_notice, turn_notices, user_message_card,
};
use crate::{
    AppModel,
    forge::{AssumptionInput, ForgeAction},
    icons::Icon,
    sizing::text_area,
    theme,
    widgets::{self, ButtonVariant},
};
use gpui::{AnyElement, Context, Div, SharedString, div, prelude::*, px};
use ikore_core::feature_forge::{
    AssumptionCategory, AssumptionReviewAction, Feature, TurnKind,
};

fn assumption_category_label(category: AssumptionCategory) -> &'static str {
    match category {
        AssumptionCategory::Scope => "scope",
        AssumptionCategory::Security => "security",
        AssumptionCategory::Ux => "ux",
        AssumptionCategory::Technical => "technical",
    }
}

/// The conversation column: fixed header, scrollable stream, and pinned composer.
pub(super) fn incubation_main(
    model: &AppModel,
    f: &Feature,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let context_suffix = if !f.brief.coverage.trim().is_empty() {
        format!(" · {} context", f.brief.coverage)
    } else {
        String::new()
    };

    let mut header = div()
        .px_3()
        .py_2()
        .border_b_1()
        .border_color(theme::border())
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .min_w_0()
                        .text_base()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(f.title.clone()),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(format!(
                            "{} · rev {}{}",
                            state(f.state),
                            f.revision,
                            context_suffix
                        )),
                ),
        );

    if !f.brief.sources.is_empty() {
        let shown_len = f.brief.sources.len().min(4);
        let mut sources_row = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1()
            .font_family(theme::FONT_MONO)
            .text_xs()
            .text_color(theme::text_subtle())
            .child("Sources:");
        for (idx, source) in f.brief.sources.iter().take(4).enumerate() {
            let full_path = source.relative_path.as_str().to_owned();
            let file_name = full_path
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(&full_path)
                .to_owned();
            let display = if idx + 1 < shown_len {
                format!("{file_name},")
            } else {
                file_name
            };
            sources_row = sources_row.child(
                div()
                    .id(SharedString::from(format!("source-chip-{idx}")))
                    .tooltip(widgets::text_tooltip(full_path))
                    .hover(|s| s.text_color(theme::text_muted()))
                    .child(display),
            );
        }
        if f.brief.sources.len() > 4 {
            sources_row = sources_row.child(format!("+{} more", f.brief.sources.len() - 4));
        }
        header = header.child(sources_row);
    }

    let mut stream = div()
        .id("forge-incubation-stream")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .p_3()
        .flex()
        .flex_col()
        .gap_3()
        .child(user_message_card(
            format!("forge-idea:{}", f.id),
            &f.original_idea,
        ));

    // Analysis turns share the table but are not conversation -- their verdicts
    // are read in the Triage and Dependencies columns, not here.
    let actionable = ikore_core::feature_forge::failure_actions(&model.forge.history, TurnKind::Discovery);
    for turn in model
        .forge
        .history
        .iter()
        .filter(|turn| turn.kind == TurnKind::Discovery)
    {
        stream = stream.child(user_message_card(
            format!("forge-user:{}", turn.id),
            &turn.user_message,
        ));
        if let Some(reply) = &turn.assistant_message {
            stream = stream.child(agent_message_card(
                format!("forge-assistant:{}", turn.id),
                reply,
            ));
        }
        if let Some(notes) = turn_notices(turn) {
            stream = stream.child(notes);
        }
        let actions = actionable
            .as_ref()
            .filter(|(id, _)| id == &turn.id)
            .map(|(_, actions)| *actions);
        if let Some(notice) = turn_failure_notice(model, turn, actions, ForgeAction::RetryTurn, cx)
        {
            stream = stream.child(notice);
        }
        if turn.status == "running" {
            stream = stream.child(
                div()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(theme::surface())
                    .border_1()
                    .border_color(theme::border())
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_xs()
                            .text_color(theme::running())
                            .child(widgets::status_dot(theme::running()))
                            .child("Feature Agent is responding..."),
                    )
                    .child(button_variant(
                        format!("cancel-feature-turn-{}", turn.id),
                        "Cancel",
                        ButtonVariant::Secondary,
                        ForgeAction::CancelTurn(turn.id.clone()),
                        None,
                        cx,
                    )),
            );
        }
    }
    if let Some(questions) = feature_question_card(model, f, cx) {
        stream = stream.child(questions);
    }

    let has_draft = !model.forge.message.read(cx).value().is_empty();
    let composer = div()
        .px_3()
        .py_2()
        .border_t_1()
        .border_color(theme::border())
        .bg(theme::surface())
        .flex_shrink_0()
        .child(
            div()
                .debug_selector(|| "forge-message-composer".into())
                .flex()
                .flex_col()
                .flex_shrink_0()
                .gap_2()
                .child(
                    // Resolve the capped field width before measuring wrapped rows,
                    // so the field's border and the footer reserve the same height.
                    div().w_full().flex_shrink_0().flex().child(
                        text_area(model.forge.message.clone())
                            .flex_shrink_0()
                            .debug_selector(|| "forge-message-input".into()),
                    ),
                )
                .child(
                    label("Enter sends · Shift+Enter inserts a newline")
                        .debug_selector(|| "forge-message-hint".into()),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .child(button_variant(
                            "send-feature-message",
                            "Send",
                            ButtonVariant::Primary,
                            ForgeAction::Send,
                            None,
                            cx,
                        ))
                        .when(has_draft, |row| {
                            row.child(button_variant(
                                "clear-feature-message",
                                "Clear",
                                ButtonVariant::Ghost,
                                ForgeAction::ClearMessageDraft,
                                None,
                                cx,
                            ))
                        }),
                ),
        );

    div()
        .id("forge-detail")
        .debug_selector(|| "forge-detail".into())
        .size_full()
        .min_h_0()
        .min_w_0()
        .flex()
        .flex_col()
        .rounded_lg()
        .bg(theme::surface_raised())
        .border_1()
        .border_color(theme::border())
        .overflow_hidden()
        .child(header)
        .child(stream)
        .child(composer)
        .into_any_element()
}

fn collapsible_spec_section(
    model: &AppModel,
    key: &'static str,
    title: impl Into<SharedString>,
    right_el: Option<AnyElement>,
    body: impl FnOnce(Div, &mut Context<AppModel>) -> Div,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let collapsed = model.forge.collapsed_spec_sections.contains(key);
    let header_id = SharedString::from(format!("spec-section-toggle-{key}"));
    let header_debug = header_id.clone();
    let mut section = div()
        .border_b_1()
        .border_color(theme::border())
        .child(
            div()
                .id(header_id)
                .debug_selector(move || header_debug.to_string())
                .px_3()
                .py_2()
                .cursor_pointer()
                .hover(|s| s.bg(theme::surface_hover()))
                .on_click(cx.listener(move |m, _, _, cx| {
                    m.forge_action(ForgeAction::ToggleSpecSection(key), cx)
                }))
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .min_w_0()
                        .child(widgets::icon(
                            if collapsed {
                                Icon::ChevronRight
                            } else {
                                Icon::ChevronDown
                            },
                            px(12.0),
                            theme::text_subtle(),
                        ))
                        .child(
                            div()
                                .text_xs()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(theme::text())
                                .child(title.into()),
                        ),
                )
                .when_some(right_el, |row, el| row.child(el)),
        );
    if !collapsed {
        section = section.child(body(
            div()
                .px_3()
                .pb_3()
                .pt_1()
                .flex()
                .flex_col()
                .gap_2()
                .min_w_0(),
            cx,
        ));
    }
    section.into_any_element()
}

fn readiness_token(
    ok: bool,
    ok_label: impl Into<SharedString>,
    blocked_label: impl Into<SharedString>,
) -> AnyElement {
    div()
        .text_xs()
        .text_color(if ok {
            theme::text_subtle()
        } else {
            theme::warning()
        })
        .child(if ok {
            ok_label.into()
        } else {
            blocked_label.into()
        })
        .into_any_element()
}

/// The specification column: compact readiness strip, flat spec sections, and governance footer.
pub(super) fn incubation_aside(
    model: &AppModel,
    f: &Feature,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let outer = div()
        .id("forge-incubation-aside")
        .debug_selector(|| "forge-incubation-aside".into())
        .size_full()
        .min_h_0()
        .min_w_0()
        .flex()
        .flex_col()
        .rounded_lg()
        .bg(theme::surface_raised())
        .border_1()
        .border_color(theme::border())
        .overflow_hidden();

    let header = div()
        .px_3()
        .py_2()
        .border_b_1()
        .border_color(theme::border())
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .child(
            div()
                .text_sm()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child("Specification"),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .font_family(theme::FONT_MONO)
                .text_xs()
                .child(
                    div()
                        .text_color(theme::text_muted())
                        .child(format!("rev {}", f.revision)),
                )
                .when(f.approved_revision == Some(f.revision), |row| {
                    row.child(
                        div()
                            .text_color(theme::success())
                            .child("· Approved"),
                    )
                }),
        );

    if f.brief.objective.is_empty() {
        return outer
            .child(header)
            .child(
                div()
                    .p_4()
                    .text_sm()
                    .text_center()
                    .text_color(theme::text_muted())
                    .child("Specification fills in here as the conversation settles."),
            )
            .into_any_element();
    }

    let already_approved = f.approved_revision == Some(f.revision);
    let core_ok = !f.brief.objective.trim().is_empty()
        && !f.brief.audience.trim().is_empty()
        && !f.brief.behavior.trim().is_empty()
        && !f.brief.scope.trim().is_empty();
    let plan_ok = !f.brief.criteria.is_empty() && !f.brief.tasks.is_empty();
    let flagged_count = f
        .brief
        .assumptions
        .iter()
        .filter(|a| a.review == Some(AssumptionReviewAction::Flag))
        .count();
    let assumptions_ok = flagged_count == 0;
    let pending_count = f.brief.pending_decisions.len();
    let decisions_ok = pending_count == 0;
    let met_count = [core_ok, plan_ok, assumptions_ok, decisions_ok]
        .into_iter()
        .filter(|v| *v)
        .count();

    let readiness_strip = div()
        .debug_selector(|| "forge-scope-readiness".into())
        .px_3()
        .py_2()
        .border_b_1()
        .border_color(theme::border())
        .bg(theme::surface())
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::text_muted())
                        .child("Readiness"),
                )
                .child(
                    div()
                        .text_xs()
                        .font_family(theme::FONT_MONO)
                        .text_color(if met_count == 4 {
                            theme::success()
                        } else {
                            theme::text_muted()
                        })
                        .child(format!("{met_count}/4")),
                ),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(readiness_token(core_ok, "Brief", "Brief incomplete"))
                .child(div().text_xs().text_color(theme::text_subtle()).child("·"))
                .child(readiness_token(plan_ok, "Tasks", "Tasks missing"))
                .child(div().text_xs().text_color(theme::text_subtle()).child("·"))
                .child(readiness_token(
                    assumptions_ok,
                    "Assumptions",
                    "Flagged assumptions",
                ))
                .child(div().text_xs().text_color(theme::text_subtle()).child("·"))
                .child(readiness_token(
                    decisions_ok,
                    "Decisions",
                    format!("{pending_count} pending"),
                )),
        );

    let mut spec_scroll = div()
        .id("forge-incubation-spec-scroll")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .flex()
        .flex_col();

    spec_scroll = spec_scroll.child(collapsible_spec_section(
        model,
        "overview",
        "Overview",
        if core_ok {
            None
        } else {
            Some(
                div()
                    .text_xs()
                    .text_color(theme::warning())
                    .child("Incomplete")
                    .into_any_element(),
            )
        },
        |mut sec, _| {
            for (heading_txt, val) in [
                ("Objective", &f.brief.objective),
                ("Audience", &f.brief.audience),
                ("Behavior", &f.brief.behavior),
            ] {
                if !val.trim().is_empty() {
                    sec = sec
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::text_subtle())
                                .child(heading_txt),
                        )
                        .child(div().text_sm().child(val.clone()));
                }
            }
            sec
        },
        cx,
    ));

    spec_scroll = spec_scroll.child(collapsible_spec_section(
        model,
        "scope",
        "Scope",
        None,
        |mut sec, _| {
            if !f.brief.scope.trim().is_empty() {
                sec = sec
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::text_subtle())
                            .child("In scope"),
                    )
                    .child(div().text_sm().child(f.brief.scope.clone()));
            }
            if !f.brief.exclusions.trim().is_empty() {
                sec = sec
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::text_subtle())
                            .child("Out of scope"),
                    )
                    .child(div().text_sm().child(f.brief.exclusions.clone()));
            }
            sec
        },
        cx,
    ));

    if !f.brief.pending_decisions.is_empty()
        || !f.brief.decisions.is_empty()
        || !f.brief.assumptions.is_empty()
        || !f.brief.alternatives.is_empty()
    {
        let total_items = f.brief.pending_decisions.len()
            + f.brief.decisions.len()
            + f.brief.assumptions.len()
            + f.brief.alternatives.len();
        let attention_count = pending_count + flagged_count;
        spec_scroll = spec_scroll.child(collapsible_spec_section(
            model,
            "decisions",
            format!("Decisions & assumptions ({total_items})"),
            if attention_count > 0 {
                Some(
                    div()
                        .text_xs()
                        .text_color(theme::warning())
                        .child(format!("{attention_count} pending"))
                        .into_any_element(),
                )
            } else {
                None
            },
            |mut sec, cx| {
                for pending in &f.brief.pending_decisions {
                    sec = sec.child(
                        div()
                            .text_xs()
                            .flex()
                            .items_start()
                            .gap_1()
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_color(theme::warning())
                                    .child("Pending:"),
                            )
                            .child(div().flex_1().min_w_0().child(pending.clone())),
                    );
                }
                for decision in &f.brief.decisions {
                    sec = sec.child(
                        div()
                            .text_xs()
                            .text_color(theme::text())
                            .child(format!("· {decision}")),
                    );
                }
                for item in &f.brief.assumptions {
                    let flagged = item.review == Some(AssumptionReviewAction::Flag);
                    let mut assumption_row = div()
                        .pt_2()
                        .border_t_1()
                        .border_color(theme::border())
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .font_family(theme::FONT_MONO)
                                .text_xs()
                                .text_color(theme::text_subtle())
                                .child(format!(
                                    "{} · {}",
                                    item.id,
                                    assumption_category_label(item.category)
                                )),
                        )
                        .child(div().text_sm().child(item.text.clone()))
                        .child(label(item.reason.clone()));
                    if let Some(AssumptionReviewAction::Answer { text }) = &item.review {
                        assumption_row = assumption_row.child(
                            div()
                                .text_xs()
                                .text_color(theme::success())
                                .child(format!("Answer: {text}")),
                        );
                    }
                    if !already_approved {
                        assumption_row = assumption_row.child(gated_button_variant(
                            format!("forge-review-assumption-{}", item.id),
                            if flagged {
                                "Save draft as answer"
                            } else {
                                "Flag"
                            },
                            if flagged {
                                ButtonVariant::Secondary
                            } else {
                                ButtonVariant::Ghost
                            },
                            ForgeAction::ReviewAssumption(
                                f.id.clone(),
                                f.revision,
                                item.id.clone(),
                                if flagged {
                                    AssumptionInput::AnswerFromDraft
                                } else {
                                    AssumptionInput::Flag
                                },
                            ),
                            !model.forge.busy,
                            None,
                            cx,
                        ));
                    }
                    sec = sec.child(assumption_row);
                }
                for alternative in &f.brief.alternatives {
                    sec = sec.child(label(format!("Alternative: {alternative}")));
                }
                sec
            },
            cx,
        ));
    }

    spec_scroll = spec_scroll.child(collapsible_spec_section(
        model,
        "tasks",
        format!(
            "Criteria ({}) & tasks ({})",
            f.brief.criteria.len(),
            f.brief.tasks.len()
        ),
        Some(
            div()
                .text_xs()
                .font_family(theme::FONT_MONO)
                .text_color(theme::text_subtle())
                .child(format!("{} tasks", f.brief.tasks.len()))
                .into_any_element(),
        ),
        |mut sec, _| {
            for c in &f.brief.criteria {
                sec = sec.child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(format!("· {}", c.description)),
                );
            }
            for t in &f.brief.tasks {
                sec = sec.child(
                    div()
                        .pt_2()
                        .border_t_1()
                        .border_color(theme::border())
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .justify_between()
                                .gap_2()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_xs()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child(t.title.clone()),
                                )
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .text_xs()
                                        .text_color(theme::text_subtle())
                                        .child(task_kind_label(t.kind)),
                                ),
                        )
                        .child(label(t.description.clone())),
                );
            }
            sec
        },
        cx,
    ));

    if !f.brief.sources.is_empty() {
        spec_scroll = spec_scroll.child(collapsible_spec_section(
            model,
            "sources",
            format!("Sources ({})", f.brief.sources.len()),
            None,
            |mut sec, _| {
                for s in &f.brief.sources {
                    sec = sec.child(
                        div()
                            .font_family(theme::FONT_MONO)
                            .text_xs()
                            .text_color(theme::text_muted())
                            .child(s.relative_path.as_str().to_owned()),
                    );
                }
                sec
            },
            cx,
        ));
    }

    let preview = model
        .forge
        .start_preview
        .as_ref()
        .filter(|p| p.feature_id == f.id && p.expected_revision == f.revision);
    let mut footer = div()
        .px_3()
        .py_2()
        .border_t_1()
        .border_color(theme::border())
        .bg(theme::surface())
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(gated_button_variant(
                    "approve-feature",
                    "Approve scope",
                    ButtonVariant::Primary,
                    ForgeAction::Approve,
                    !already_approved && f.brief.pending_decisions.is_empty(),
                    None,
                    cx,
                ))
                .child(gated_button_variant(
                    format!("approve-and-start-{}", f.id),
                    if already_approved {
                        "Start"
                    } else {
                        "Approve & start"
                    },
                    ButtonVariant::Secondary,
                    ForgeAction::ApproveAndStart,
                    !model.forge.busy && preview.is_some_and(|p| p.blockers.is_empty()),
                    None,
                    cx,
                )),
        );

    if already_approved {
        footer = footer.child(
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child(format!(
                    "Approved for rev {}. Continue chatting for a new revision.",
                    f.revision
                )),
        );
    } else if let Some(preview) = preview {
        for blocker in &preview.blockers {
            footer = footer.child(
                div()
                    .text_xs()
                    .text_color(theme::warning())
                    .child(blocker.message.clone()),
            );
        }
    } else {
        footer = footer.child(
            div()
                .text_xs()
                .text_color(theme::text_subtle())
                .child("Checking start requirements..."),
        );
    }

    if !already_approved
        && !f.brief.pending_decisions.is_empty()
        && ikore_core::feature_forge::remaining_questions(&model.forge.history) == 0
    {
        footer = footer.child(gated_button_variant(
            "forge-continue-questions",
            "Continue clarification",
            ButtonVariant::Ghost,
            ForgeAction::ContinueClarification(f.id.clone(), f.revision),
            !model.forge.busy,
            None,
            cx,
        ));
    }
    if model.forge.busy {
        footer = footer.child(
            div()
                .text_xs()
                .text_color(theme::running())
                .child("Working..."),
        );
    }

    outer
        .child(header)
        .child(readiness_strip)
        .child(spec_scroll)
        .child(footer)
        .into_any_element()
}

/// Selecting an option fills an editable developer draft; it never sends a turn.
pub(super) fn feature_question_card(
    model: &AppModel,
    feature: &Feature,
    cx: &mut Context<AppModel>,
) -> Option<Div> {
    let turn = model.forge.question_turn(feature)?;
    let mut panel = div()
        .debug_selector(|| "forge-question-options".into())
        .p_3()
        .rounded_lg()
        .bg(theme::surface())
        .border_1()
        .border_color(theme::border_strong())
        .flex()
        .flex_col()
        .gap_3()
        .min_w_0()
        .child(
            div()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::text_muted())
                .child("Open question"),
        );

    for (question_index, question) in turn.questions.iter().enumerate() {
        let mut q_block = div()
            .flex()
            .flex_col()
            .gap_2()
            .min_w_0()
            .child(
                div()
                    .text_sm()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(question.clone()),
            );
        if let Some(detail) = turn
            .question_details
            .iter()
            .find(|d| d.question_index == question_index)
        {
            if !detail.explanation.trim().is_empty() {
                q_block = q_block.child(label(detail.explanation.clone()));
            }
            for (option_index, option) in detail.options.iter().enumerate() {
                let recommended = detail.recommended_option == Some(option_index);
                let opt_id = SharedString::from(format!(
                    "forge-question-{}-{question_index}-{option_index}",
                    turn.id
                ));
                let debug_id = opt_id.clone();
                let turn_id = turn.id.clone();
                q_block = q_block.child(
                    div()
                        .id(opt_id)
                        .debug_selector(move || debug_id.to_string())
                        .tab_index(0)
                        .cursor_pointer()
                        .p_2()
                        .rounded_md()
                        .border_1()
                        .border_color(theme::border())
                        .bg(theme::surface_raised())
                        .hover(|s| {
                            s.bg(theme::surface_hover())
                                .border_color(theme::accent())
                        })
                        .focus_visible(|s| s.border_color(theme::accent()))
                        .on_click(cx.listener(move |model, _, _, cx| {
                            model.forge_action(
                                ForgeAction::ChooseQuestionOption(
                                    turn_id.clone(),
                                    question_index,
                                    option_index,
                                ),
                                cx,
                            )
                        }))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_2()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .text_color(theme::text())
                                        .child(option.label.clone()),
                                )
                                .when(recommended, |row| {
                                    row.child(
                                        div()
                                            .flex_shrink_0()
                                            .text_xs()
                                            .text_color(theme::accent())
                                            .child("Recommended"),
                                    )
                                }),
                        )
                        .when(!option.explanation.trim().is_empty(), |card_el| {
                            card_el.child(
                                div()
                                    .text_xs()
                                    .text_color(theme::text_muted())
                                    .child(option.explanation.clone()),
                            )
                        }),
                );
            }
        }
        panel = panel.child(q_block);
    }

    Some(panel)
}

pub(super) fn feature_start_card(
    model: &AppModel,
    feature: &Feature,
    cx: &mut Context<AppModel>,
) -> Div {
    let preview = model
        .forge
        .start_preview
        .as_ref()
        .filter(|p| p.feature_id == feature.id && p.expected_revision == feature.revision);
    let approved = feature.approved_revision == Some(feature.revision);
    let mut panel = div()
        .flex()
        .flex_col()
        .gap_1()
        .child(gated_button_variant(
            format!("approve-and-start-{}", feature.id),
            if approved {
                "Start"
            } else {
                "Approve & start"
            },
            ButtonVariant::Secondary,
            ForgeAction::ApproveAndStart,
            !model.forge.busy && preview.is_some_and(|p| p.blockers.is_empty()),
            None,
            cx,
        ));
    if let Some(preview) = preview {
        for blocker in &preview.blockers {
            panel = panel.child(
                div()
                    .text_xs()
                    .text_color(theme::warning())
                    .child(blocker.message.clone()),
            );
        }
    } else {
        panel = panel.child(
            div()
                .text_xs()
                .text_color(theme::text_subtle())
                .child("Checking start requirements..."),
        );
    }
    panel
}
