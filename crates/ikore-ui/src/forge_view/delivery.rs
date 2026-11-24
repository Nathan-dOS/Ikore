use super::{
    button_variant, card, feature_state_badge, gated_button_variant, heading, heading_info, label,
    mono_pill, pill, pill_icon, recessed_box, task_state_badge,
};
use crate::{
    AppModel,
    forge::ForgeAction,
    icons::Icon,
    pulsations::{PIPELINE_STAGES, stage_label, status_label},
    theme,
    widgets::{self, ButtonVariant},
};
use gpui::{AnyElement, Context, div, prelude::*, px, relative};
use ikore_core::feature_forge::{Feature, FeatureState};

pub(super) fn delivery(model: &AppModel, f: &Feature, cx: &mut Context<AppModel>) -> AnyElement {
    let tasks = model
        .forge
        .snapshot
        .tasks
        .iter()
        .filter(|t| {
            t.draft.feature_id.as_ref() == Some(&f.id) && t.feature_revision == f.approved_revision
        })
        .collect::<Vec<_>>();
    let total = tasks.len();
    let done = tasks.iter().filter(|t| t.state == "done").count();
    let in_flight = tasks
        .iter()
        .filter(|t| t.state == "queued" || t.state == "running")
        .count();
    let ready = tasks.iter().filter(|t| t.state == "ready").count();

    let (state_label, state_bg, state_fg) = feature_state_badge(f.state);
    let pulsations_by_id: std::collections::HashMap<
        String,
        (
            ikore_core::persistence_service::PulsationStatus,
            ikore_core::persistence_service::PulsationStage,
            Option<String>,
        ),
    > = model
        .pulsations
        .read(cx)
        .pulsations
        .iter()
        .map(|p| (p.id.clone(), (p.status, p.stage, p.terminal_reason.clone())))
        .collect();

    let pending_prereqs: Vec<String> = model
        .forge
        .snapshot
        .dependencies
        .iter()
        .filter(|d| d.feature_id == f.id)
        .filter_map(|d| {
            model
                .forge
                .snapshot
                .features
                .iter()
                .find(|x| x.id == d.prerequisite_id)
                .filter(|pre| pre.state != FeatureState::Done)
                .map(|pre| pre.title.clone())
        })
        .collect();
    let unblocks: Vec<String> = model
        .forge
        .snapshot
        .dependencies
        .iter()
        .filter(|d| d.prerequisite_id == f.id)
        .filter_map(|d| {
            model
                .forge
                .snapshot
                .features
                .iter()
                .find(|x| x.id == d.feature_id)
                .filter(|dep| dep.state != FeatureState::Archived)
                .map(|dep| dep.title.clone())
        })
        .collect();

    let mut summary_card = card()
        .border_1()
        .border_color(theme::border())
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_start()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .min_w_0()
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap_2()
                                .child(pill(state_label, state_bg, state_fg))
                                .child(mono_pill(
                                    format!("rev {}", f.revision),
                                    theme::surface(),
                                    theme::text_muted(),
                                ))
                                .when_some(f.approved_revision, |row, rev| {
                                    row.child(mono_pill(
                                        format!("Approved rev {rev}"),
                                        theme::accent_soft(),
                                        theme::accent(),
                                    ))
                                }),
                        )
                        .child(heading(f.title.clone())),
                )
                .child(button_variant(
                    "compose-feature-tasks",
                    "Select tasks from this feature",
                    if ready > 0 {
                        ButtonVariant::Primary
                    } else {
                        ButtonVariant::Tinted
                    },
                    ForgeAction::SelectFeatureTasks(f.id.clone()),
                    Some(Icon::ListTodo),
                    cx,
                )),
        );

    if total > 0 {
        let ratio = done as f32 / total as f32;
        summary_card = summary_card.child(
            recessed_box()
                .p_3()
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child(format!("{done}/{total} tasks integrated")),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap_2()
                                .when(in_flight > 0, |row| {
                                    row.child(mono_pill(
                                        format!("{in_flight} in flight"),
                                        theme::running_soft(),
                                        theme::running(),
                                    ))
                                })
                                .when(ready > 0, |row| {
                                    row.child(mono_pill(
                                        format!("{ready} ready"),
                                        theme::accent_soft(),
                                        theme::accent(),
                                    ))
                                }),
                        ),
                )
                .child(
                    div()
                        .w_full()
                        .h(px(6.0))
                        .rounded_full()
                        .bg(theme::surface_raised())
                        .overflow_hidden()
                        .when(done > 0, |track| {
                            track.child(
                                div()
                                    .h_full()
                                    .w(relative(ratio))
                                    .rounded_full()
                                    .bg(if done == total {
                                        theme::success()
                                    } else {
                                        theme::accent()
                                    }),
                            )
                        }),
                ),
        );
    } else {
        summary_card = summary_card.child(
            recessed_box()
                .p_3()
                .items_center()
                .child(widgets::icon(Icon::ListTodo, px(18.0), theme::text_subtle()))
                .child(label(if f.approved_revision.is_none() {
                    "Approve this feature in Incubation or Triage to generate delivery tasks."
                } else {
                    "No tasks are linked to the approved revision yet."
                })),
        );
    }

    if !pending_prereqs.is_empty() {
        summary_card = summary_card.child(
            div()
                .p_2()
                .rounded_md()
                .bg(theme::warning_soft())
                .border_1()
                .border_color(theme::warning())
                .flex()
                .items_start()
                .gap_2()
                .text_xs()
                .text_color(theme::warning())
                .child(div().mt(px(2.0)).child(widgets::icon(
                    Icon::TriangleAlert,
                    px(13.0),
                    theme::warning(),
                )))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(format!(
                            "Blocked by pending prerequisite(s): {}",
                            pending_prereqs.join(", ")
                        )),
                ),
        );
    }

    if !unblocks.is_empty() {
        summary_card = summary_card.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_xs()
                .text_color(theme::text_muted())
                .child(widgets::icon(Icon::GitBranch, px(13.0), theme::accent()))
                .child(format!(
                    "Completing this feature unblocks: {}",
                    unblocks.join(", ")
                )),
        );
    }

    let mut tasks_section = div().flex().flex_col().gap_2();
    let mut rendered_pulse_button_ids = std::collections::HashSet::new();
    for t in &tasks {
        let (t_bg, t_fg) = task_state_badge(&t.state);
        let evidence = model
            .forge
            .snapshot
            .integration_evidence
            .iter()
            .find(|e| e.task_id == t.id);
        let linked_pulse_id = t
            .active_pulsation_id
            .as_ref()
            .or(t.completed_pulsation_id.as_ref())
            .cloned();
        let pulse_info = linked_pulse_id
            .as_ref()
            .and_then(|pid| pulsations_by_id.get(pid))
            .cloned();

        let mut task_card = card()
            .border_1()
            .border_color(if t.state == "done" && evidence.is_some() {
                theme::success_soft()
            } else {
                theme::border()
            })
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
                            .min_w(px(160.0))
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_2()
                            .child(widgets::status_dot(if t.state == "done" {
                                theme::success()
                            } else if t.state == "queued" || t.state == "running" {
                                theme::running()
                            } else {
                                theme::text_subtle()
                            }))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(t.draft.title.clone()),
                            )
                            .child(mono_pill(t.state.clone(), t_bg, t_fg))
                            .when_some(pulse_info, |row, (p_status, p_stage, p_reason)| {
                                let stage_idx = PIPELINE_STAGES
                                    .iter()
                                    .position(|s| *s == p_stage)
                                    .map(|i| i + 1)
                                    .unwrap_or(1);
                                row.child(mono_pill(
                                    format!(
                                        "{} · {} ({stage_idx}/6)",
                                        status_label(p_status, p_reason.as_deref()),
                                        stage_label(p_stage)
                                    ),
                                    theme::surface(),
                                    theme::text_muted(),
                                ))
                            }),
                    )
                    .when_some(linked_pulse_id, |row, pulse_id| {
                        rendered_pulse_button_ids.insert(t.id.clone());
                        row.child(button_variant(
                            format!("delivery-evidence-{}", t.id),
                            "Open pulsation",
                            ButtonVariant::Secondary,
                            ForgeAction::OpenPulsation(pulse_id),
                            Some(Icon::Activity),
                            cx,
                        ))
                    }),
            );

        if !t.draft.criteria.is_empty() {
            let mut criteria_box = recessed_box();
            for c in &t.draft.criteria {
                let covered = evidence.is_some_and(|e| e.criterion_ids.contains(&c.id));
                criteria_box = criteria_box.child(
                    div()
                        .flex()
                        .items_start()
                        .gap_2()
                        .text_xs()
                        .text_color(if covered {
                            theme::text()
                        } else {
                            theme::text_muted()
                        })
                        .child(div().mt(px(2.0)).child(widgets::icon(
                            if covered { Icon::Check } else { Icon::Target },
                            px(12.0),
                            if covered {
                                theme::success()
                            } else {
                                theme::text_subtle()
                            },
                        )))
                        .child(div().flex_1().min_w_0().child(c.description.clone()))
                        .when(covered, |row| {
                            row.child(mono_pill(
                                "Evidence recorded",
                                theme::success_soft(),
                                theme::success(),
                            ))
                        }),
                );
            }
            task_card = task_card.child(criteria_box);
        }

        if let Some(evidence) = evidence {
            task_card = task_card.child(
                div()
                    .p_2()
                    .rounded_md()
                    .bg(theme::success_soft())
                    .border_1()
                    .border_color(theme::success())
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
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .text_xs()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(theme::success())
                                    .child(widgets::icon(
                                        Icon::ShieldCheck,
                                        px(13.0),
                                        theme::success(),
                                    ))
                                    .child("Integration evidence"),
                            )
                            .when(!evidence.artifact_positions.is_empty(), |row| {
                                row.child(mono_pill(
                                    format!("{} artifact(s)", evidence.artifact_positions.len()),
                                    theme::surface(),
                                    theme::success(),
                                ))
                            }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::text())
                            .child(evidence.summary.clone()),
                    )
                    .when(!evidence.criterion_ids.is_empty(), |box_el| {
                        box_el.child(
                            div()
                                .font_family(theme::FONT_MONO)
                                .text_xs()
                                .text_color(theme::text_muted())
                                .child(format!(
                                    "Covered criteria: {}",
                                    evidence.criterion_ids.join(", ")
                                )),
                        )
                    }),
            );
        } else {
            task_card = task_card.child(
                div()
                    .text_xs()
                    .text_color(theme::text_subtle())
                    .child("No integration evidence recorded yet."),
            );
        }

        tasks_section = tasks_section.child(task_card);
    }

    // Preserve shortcuts for any tasks of this feature outside the approved revision
    // that carry linked pulsations.
    let mut extra_links = div().flex().flex_wrap().gap_2();
    let mut has_extra_links = false;
    for t in &model.forge.snapshot.tasks {
        if t.draft.feature_id.as_ref() == Some(&f.id)
            && !rendered_pulse_button_ids.contains(&t.id)
            && let Some(pulse) = t
                .active_pulsation_id
                .as_ref()
                .or(t.completed_pulsation_id.as_ref())
        {
            has_extra_links = true;
            extra_links = extra_links.child(button_variant(
                format!("delivery-evidence-{}", t.id),
                format!("Evidence: {}", t.draft.title),
                ButtonVariant::Secondary,
                ForgeAction::OpenPulsation(pulse.clone()),
                Some(Icon::Activity),
                cx,
            ));
        }
    }

    // Check which feature criterion IDs are referenced by integration evidence on this feature's tasks.
    let covered_feature_criteria: std::collections::HashSet<&str> = model
        .forge
        .snapshot
        .integration_evidence
        .iter()
        .filter(|e| tasks.iter().any(|t| t.id == e.task_id))
        .flat_map(|e| e.criterion_ids.iter().map(String::as_str))
        .collect();

    let can_complete = f.state != FeatureState::Done
        && f.approved_revision == Some(f.revision)
        && total > 0
        && done == total;
    let remaining = total.saturating_sub(done);
    let completion_note = if f.state == FeatureState::Done {
        Some("Feature marked as done.".to_owned())
    } else if f.approved_revision != Some(f.revision) {
        Some("Approve the current revision before completing delivery.".to_owned())
    } else if total == 0 {
        Some("No tasks exist for this approved revision yet.".to_owned())
    } else if remaining > 0 {
        Some(format!(
            "All {total} tasks must be integrated before completing this feature ({remaining} remaining)."
        ))
    } else {
        None
    };

    let mut review_card = card()
        .border_1()
        .border_color(if can_complete || f.state == FeatureState::Done {
            theme::success()
        } else {
            theme::border()
        })
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap_2()
                .child(heading_info(
                    "Final review of feature criteria",
                    "delivery-review",
                    "Confirm delivery after reviewing evidence and publications from linked pulsations.",
                ))
                .child(if f.state == FeatureState::Done {
                    pill_icon(
                        Icon::Check,
                        "Completed",
                        theme::success_soft(),
                        theme::success(),
                    )
                } else if can_complete {
                    pill_icon(
                        Icon::Check,
                        "Ready to complete",
                        theme::success_soft(),
                        theme::success(),
                    )
                } else if remaining > 0 {
                    pill(
                        format!("{remaining} task(s) remaining"),
                        theme::warning_soft(),
                        theme::warning(),
                    )
                } else {
                    pill("Pending approval", theme::surface(), theme::text_muted())
                }),
        );

    if f.brief.criteria.is_empty() {
        review_card = review_card.child(label("No feature-level acceptance criteria recorded."));
    } else {
        let mut criteria_list = recessed_box();
        for c in &f.brief.criteria {
            let referenced = covered_feature_criteria.contains(c.id.as_str());
            criteria_list = criteria_list.child(
                div()
                    .flex()
                    .items_start()
                    .gap_2()
                    .text_xs()
                    .text_color(theme::text())
                    .child(div().mt(px(2.0)).child(widgets::icon(
                        if referenced { Icon::Check } else { Icon::Target },
                        px(12.0),
                        if referenced {
                            theme::success()
                        } else {
                            theme::text_subtle()
                        },
                    )))
                    .child(div().flex_1().min_w_0().child(c.description.clone()))
                    .when(referenced, |row| {
                        row.child(mono_pill(
                            "Referenced in evidence",
                            theme::success_soft(),
                            theme::success(),
                        ))
                    }),
            );
        }
        review_card = review_card.child(criteria_list);
    }

    if let Some(note) = completion_note {
        review_card = review_card.child(label(note));
    }

    review_card = review_card.child(gated_button_variant(
        "complete-feature",
        "Confirm criteria and complete feature",
        ButtonVariant::Primary,
        ForgeAction::Complete,
        !model.forge.busy && can_complete,
        Some(Icon::Check),
        cx,
    ));

    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(summary_card)
        .when(total > 0, |root| root.child(tasks_section))
        .when(has_extra_links, |root| {
            root.child(
                card()
                    .border_1()
                    .border_color(theme::border())
                    .child(label("Other linked pulsations"))
                    .child(extra_links),
            )
        })
        .child(review_card)
        .into_any_element()
}
