use super::{REVIEW_POLICY_NEEDS_COMPOSITION, button_variant, label, search_box, task_kind_label};
use crate::{
    AppModel,
    forge::{BacklogKindFilter, BacklogStateFilter, ForgeAction},
    sizing::{INPUT_MEDIUM, sized_input},
    theme,
    widgets::{self, ButtonVariant},
};
use gpui::{AnyElement, Context, SharedString, div, prelude::*, px};
use ikore_core::feature_forge::{TaskKind, TaskService};
use ikore_core::persistence_service::PulsationStatus;
use std::collections::HashSet;

fn kind_tab(
    id: impl Into<SharedString>,
    text: impl Into<SharedString>,
    active: bool,
    action: ForgeAction,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let id = id.into();
    let selector = id.clone();
    div()
        .id(id)
        .debug_selector(move || selector.to_string())
        .tab_index(0)
        .cursor_pointer()
        .px_2()
        .py(px(4.0))
        .rounded_md()
        .border_1()
        .border_color(if active {
            theme::border_strong()
        } else {
            gpui::rgba(0x00000000)
        })
        .bg(if active {
            theme::surface_raised()
        } else {
            gpui::rgba(0x00000000)
        })
        .text_xs()
        .font_weight(if active {
            gpui::FontWeight::SEMIBOLD
        } else {
            gpui::FontWeight::MEDIUM
        })
        .text_color(if active {
            theme::text()
        } else {
            theme::text_muted()
        })
        .hover(|s| s.bg(theme::surface_hover()).text_color(theme::text()))
        .on_click(cx.listener(move |model, _, _, cx| model.forge_action(action.clone(), cx)))
        .child(text.into())
        .into_any_element()
}

fn task_state_dot(task_state: &str) -> gpui::Rgba {
    match task_state {
        "ready" => theme::success(),
        "queued" | "running" => theme::running(),
        _ => theme::text_subtle(),
    }
}

fn task_state_label(task_state: &str) -> &'static str {
    match task_state {
        "ready" => "Ready",
        "queued" => "Queued",
        "running" => "Running",
        "done" => "Done",
        _ => "Pending",
    }
}

pub(super) fn backlog_body(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let active_tasks: Vec<_> = model
        .forge
        .snapshot
        .tasks
        .iter()
        .filter(|t| t.state != "superseded")
        .collect();
    let failed_pulsations: HashSet<String> = model
        .pulsations
        .read(cx)
        .pulsations
        .iter()
        .filter(|p| p.status == PulsationStatus::Failed)
        .map(|p| p.id.clone())
        .collect();

    let total_count = active_tasks.len();
    let bug_count = active_tasks
        .iter()
        .filter(|t| t.draft.kind == TaskKind::Bug)
        .count();
    let feature_count = active_tasks
        .iter()
        .filter(|t| t.draft.kind == TaskKind::Feature)
        .count();
    let tweaks_count = active_tasks
        .iter()
        .filter(|t| BacklogKindFilter::Tweaks.matches(t.draft.kind))
        .count();
    let foundation_count = active_tasks
        .iter()
        .filter(|t| t.draft.kind == TaskKind::Foundation)
        .count();

    let query = model
        .forge
        .backlog_search
        .read(cx)
        .value()
        .trim()
        .to_ascii_lowercase();
    let filtered_tasks: Vec<_> = active_tasks
        .iter()
        .copied()
        .filter(|t| {
            model.forge.backlog_kind_filter.matches(t.draft.kind)
                && model.forge.backlog_state_filter.matches(&t.state)
                && model.forge.backlog_task_matches(t, &query)
        })
        .collect();
    let has_unselected_selectable = filtered_tasks.iter().any(|t| {
        model.forge.is_task_selectable(t, &failed_pulsations)
            && !model.forge.selected_tasks.contains(&t.id)
    });

    // Row 1: search + stable-geometry reconcile command.
    let command_bar = div()
        .flex()
        .flex_wrap()
        .items_center()
        .justify_between()
        .gap_2()
        .child(
            div()
                .flex_1()
                .min_w(px(180.0))
                .max_w(px(320.0))
                .child(search_box(model.forge.backlog_search.clone())),
        )
        .child(button_variant(
            "retry-forge-publications",
            "Reconcile notes",
            ButtonVariant::Secondary,
            ForgeAction::RetryPublications,
            None,
            cx,
        ));

    // Row 2: quiet kind tabs + compact state segment + Select all ready.
    let mut kind_tabs = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_1()
        .child(kind_tab(
            "backlog-filter-kind-all",
            format!("All {total_count}"),
            model.forge.backlog_kind_filter == BacklogKindFilter::All,
            ForgeAction::SetBacklogKindFilter(BacklogKindFilter::All),
            cx,
        ));
    if bug_count > 0 || model.forge.backlog_kind_filter == BacklogKindFilter::Bug {
        kind_tabs = kind_tabs.child(kind_tab(
            "backlog-filter-kind-bug",
            format!("Bugs {bug_count}"),
            model.forge.backlog_kind_filter == BacklogKindFilter::Bug,
            ForgeAction::SetBacklogKindFilter(BacklogKindFilter::Bug),
            cx,
        ));
    }
    if feature_count > 0 || model.forge.backlog_kind_filter == BacklogKindFilter::Feature {
        kind_tabs = kind_tabs.child(kind_tab(
            "backlog-filter-kind-feature",
            format!("Features {feature_count}"),
            model.forge.backlog_kind_filter == BacklogKindFilter::Feature,
            ForgeAction::SetBacklogKindFilter(BacklogKindFilter::Feature),
            cx,
        ));
    }
    if tweaks_count > 0 || model.forge.backlog_kind_filter == BacklogKindFilter::Tweaks {
        kind_tabs = kind_tabs.child(kind_tab(
            "backlog-filter-kind-tweaks",
            format!("Tweaks {tweaks_count}"),
            model.forge.backlog_kind_filter == BacklogKindFilter::Tweaks,
            ForgeAction::SetBacklogKindFilter(BacklogKindFilter::Tweaks),
            cx,
        ));
    }
    if foundation_count > 0 || model.forge.backlog_kind_filter == BacklogKindFilter::Foundation {
        kind_tabs = kind_tabs.child(kind_tab(
            "backlog-filter-kind-foundation",
            format!("Foundation {foundation_count}"),
            model.forge.backlog_kind_filter == BacklogKindFilter::Foundation,
            ForgeAction::SetBacklogKindFilter(BacklogKindFilter::Foundation),
            cx,
        ));
    }

    let mut state_segment = widgets::segment_track(div());
    for (filter, id, title) in [
        (BacklogStateFilter::All, "backlog-filter-state-all", "All"),
        (
            BacklogStateFilter::Ready,
            "backlog-filter-state-ready",
            "Ready",
        ),
        (
            BacklogStateFilter::Queued,
            "backlog-filter-state-queued",
            "Queued",
        ),
        (
            BacklogStateFilter::Done,
            "backlog-filter-state-done",
            "Done",
        ),
    ] {
        state_segment = state_segment.child(
            widgets::segment(
                div()
                    .id(id)
                    .debug_selector(move || id.to_string())
                    .tab_index(0),
                model.forge.backlog_state_filter == filter,
            )
            .on_click(cx.listener(move |model, _, _, cx| {
                model.forge_action(ForgeAction::SetBacklogStateFilter(filter), cx);
            }))
            .child(title),
        );
    }

    let filter_strip = div()
        .flex()
        .flex_wrap()
        .items_center()
        .justify_between()
        .gap_2()
        .child(kind_tabs)
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(state_segment)
                .when(has_unselected_selectable, |row| {
                    row.child(button_variant(
                        "backlog-select-all-ready",
                        "Select all ready",
                        ButtonVariant::Ghost,
                        ForgeAction::SelectAllReadyTasks,
                        None,
                        cx,
                    ))
                }),
        );

    let mut list = div()
        .id("task-backlog")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_2();

    for task in &filtered_tasks {
        let selected = model.forge.selected_tasks.contains(&task.id);
        let can_select = model.forge.is_task_selectable(task, &failed_pulsations);
        let expanded = model.forge.expanded_backlog_tasks.contains(&task.id);
        let feature_title = task
            .draft
            .feature_id
            .as_ref()
            .and_then(|id| model.forge.snapshot.features.iter().find(|f| &f.id == id))
            .map(|f| f.title.clone());

        let evidence = model
            .forge
            .snapshot
            .integration_evidence
            .iter()
            .find(|e| e.task_id == task.id);

        let mut pending_prereqs = Vec::new();
        let mut all_prereq_names = Vec::new();
        for dep_id in &task.draft.dependencies {
            if let Some(dep_task) = model.forge.snapshot.tasks.iter().find(|t| &t.id == dep_id) {
                all_prereq_names.push(dep_task.draft.title.clone());
                if dep_task.state != "done" {
                    pending_prereqs.push(format!("{} ({})", dep_task.draft.title, dep_task.state));
                }
            } else {
                all_prereq_names.push(dep_id.clone());
                pending_prereqs.push(dep_id.clone());
            }
        }

        let assessment = ikore_core::orchestration::task_refine::assess(
            &task.draft.description,
            task.revision,
            40.try_into().expect("bounded threshold"),
        );
        let needs_detail = assessment.score.get() >= assessment.threshold.get();
        let suggest_feature = task.draft.feature_id.is_none()
            && task.state == "ready"
            && TaskService::suggest_feature(&task.draft);
        let covered_count = task
            .draft
            .criteria
            .iter()
            .filter(|c| evidence.is_some_and(|e| e.criterion_ids.contains(&c.id)))
            .count();

        let has_details = !task.draft.criteria.is_empty()
            || !task.draft.dependencies.is_empty()
            || needs_detail
            || suggest_feature
            || evidence.is_some()
            || task.draft.description.contains('\n')
            || task.draft.description.chars().count() > 90;

        // Actions stay anchored at the card's top-right beside the title.
        let mut actions: Vec<AnyElement> = Vec::new();
        if let Some(pulse) = task
            .active_pulsation_id
            .as_ref()
            .or(task.completed_pulsation_id.as_ref())
        {
            actions.push(button_variant(
                format!("task-pulsation-{}", task.id),
                "Pulsation",
                ButtonVariant::Ghost,
                ForgeAction::OpenPulsation(pulse.clone()),
                None,
                cx,
            ));
        }
        if can_select || selected {
            actions.push(button_variant(
                format!("select-task-{}", task.id),
                if selected { "Selected" } else { "Select" },
                if selected {
                    ButtonVariant::Selected
                } else {
                    ButtonVariant::Secondary
                },
                ForgeAction::ToggleTask(task.id.clone()),
                None,
                cx,
            ));
        }
        if task.draft.feature_id.is_none() && task.state == "ready" {
            actions.push(button_variant(
                format!("edit-task-{}", task.id),
                "Edit",
                ButtonVariant::Ghost,
                ForgeAction::EditTask(task.id.clone()),
                None,
                cx,
            ));
        }
        if suggest_feature {
            actions.push(button_variant(
                format!("convert-task-{}", task.id),
                "To feature",
                ButtonVariant::Ghost,
                ForgeAction::SuggestFeature(task.id.clone()),
                None,
                cx,
            ));
        }

        let mut meta = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .text_xs()
            .text_color(theme::text_muted())
            .child(
                div()
                    .text_color(theme::text_subtle())
                    .child(task_kind_label(task.draft.kind)),
            )
            .child("·")
            .child(task_state_label(&task.state));

        if let Some(ft) = feature_title {
            meta = meta.child("·").child(ft);
        }

        if !task.draft.criteria.is_empty() {
            let total_c = task.draft.criteria.len();
            let criteria_text = if covered_count > 0 {
                format!("{covered_count}/{total_c} verified")
            } else if total_c == 1 {
                "1 criterion".to_owned()
            } else {
                format!("{total_c} criteria")
            };
            meta = meta.child("·").child(
                div()
                    .text_color(if covered_count == total_c {
                        theme::success()
                    } else {
                        theme::text_muted()
                    })
                    .child(criteria_text),
            );
        }

        if !pending_prereqs.is_empty() {
            meta = meta.child("·").child(
                div()
                    .text_color(theme::warning())
                    .child(format!("Waiting on {} prereq", pending_prereqs.len())),
            );
        }

        if needs_detail {
            meta = meta
                .child("·")
                .child(div().text_color(theme::warning()).child("Needs detail"));
        }

        if has_details {
            let toggle_id = SharedString::from(format!("toggle-task-details-{}", task.id));
            let toggle_debug = toggle_id.clone();
            let task_id = task.id.clone();
            meta = meta.child("·").child(
                div()
                    .id(toggle_id)
                    .debug_selector(move || toggle_debug.to_string())
                    .cursor_pointer()
                    .text_color(theme::text_subtle())
                    .hover(|s| s.text_color(theme::text()))
                    .on_click(cx.listener(move |model, _, _, cx| {
                        model.forge_action(
                            ForgeAction::ToggleBacklogTaskDetails(task_id.clone()),
                            cx,
                        );
                    }))
                    .child(if expanded { "Hide" } else { "Details" }),
            );
        }

        let header = div()
            .flex()
            .items_start()
            .justify_between()
            .gap_3()
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
                            .min_w_0()
                            .child(widgets::status_dot(task_state_dot(&task.state)))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(if can_select || selected {
                                        theme::text()
                                    } else {
                                        theme::text_muted()
                                    })
                                    .child(task.draft.title.clone()),
                            ),
                    )
                    .child(meta),
            )
            .child(
                div()
                    .id(SharedString::from(format!("task-actions-{}", task.id)))
                    .debug_selector({
                        let id = task.id.clone();
                        move || format!("task-actions-{id}")
                    })
                    .flex_shrink_0()
                    .max_w(px(360.0))
                    .flex()
                    .flex_wrap()
                    .justify_end()
                    .items_center()
                    .gap_1()
                    .children(actions),
            );

        let desc_line = if expanded {
            div()
                .text_xs()
                .text_color(theme::text())
                .child(task.draft.description.clone())
        } else {
            div()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_xs()
                .text_color(theme::text_muted())
                .child(task.draft.description.clone())
        };

        let mut card = div()
            .debug_selector({
                let card_id = task.id.clone();
                move || format!("task-card-{card_id}")
            })
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded_lg()
            .min_w_0()
            .bg(if can_select || selected {
                theme::surface_raised()
            } else {
                theme::surface()
            })
            .border_1()
            .border_color(if selected {
                theme::accent()
            } else {
                theme::border()
            })
            .child(header)
            .child(desc_line);

        if expanded {
            let mut details = div()
                .pt_2()
                .border_t_1()
                .border_color(theme::border())
                .flex()
                .flex_col()
                .gap_2()
                .min_w_0();

            for criterion in &task.draft.criteria {
                let covered = evidence.is_some_and(|e| e.criterion_ids.contains(&criterion.id));
                details = details.child(
                    div()
                        .flex()
                        .items_start()
                        .justify_between()
                        .gap_2()
                        .text_xs()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_color(if covered {
                                    theme::text()
                                } else {
                                    theme::text_muted()
                                })
                                .child(format!("· {}", criterion.description)),
                        )
                        .when(covered, |row| {
                            row.child(
                                div()
                                    .flex_shrink_0()
                                    .text_color(theme::success())
                                    .child("verified"),
                            )
                        }),
                );
            }

            if !task.draft.dependencies.is_empty() {
                if !pending_prereqs.is_empty() {
                    details = details.child(
                        div()
                            .text_xs()
                            .text_color(theme::warning())
                            .child(format!("Waiting on: {}", pending_prereqs.join(", "))),
                    );
                } else {
                    details = details.child(div().text_xs().text_color(theme::text_muted()).child(
                        format!("Prerequisites satisfied: {}", all_prereq_names.join(", ")),
                    ));
                }
            }

            if needs_detail {
                for contribution in assessment.contributions {
                    details = details.child(
                        div()
                            .text_xs()
                            .text_color(theme::warning())
                            .child(format!("Detail hint: {}", contribution.reason)),
                    );
                }
            }

            if suggest_feature {
                details = details.child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child("Scope looks broad enough to promote to a feature."),
                );
            }

            if let Some(evidence) = evidence {
                details = details.child(
                    div()
                        .pl_2()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(format!("Evidence: {}", evidence.summary)),
                );
            }

            card = card.child(details);
        }

        list = list.child(card);
    }

    if active_tasks.is_empty() {
        list = list.child(
            div()
                .py_6()
                .flex()
                .justify_center()
                .child(label("No tasks yet. Add a task or approve a feature.")),
        );
    } else if filtered_tasks.is_empty() {
        list = list.child(
            div()
                .py_6()
                .flex()
                .flex_col()
                .items_center()
                .gap_2()
                .child(label("No tasks match the current filters."))
                .child(button_variant(
                    "backlog-reset-filters",
                    "Reset filters",
                    ButtonVariant::Ghost,
                    ForgeAction::SetBacklogKindFilter(BacklogKindFilter::All),
                    None,
                    cx,
                )),
        );
    }

    // Batch Composer panel (minimalist right rail)
    let staged_tasks: Vec<_> = model
        .forge
        .snapshot
        .tasks
        .iter()
        .filter(|t| model.forge.selected_tasks.contains(&t.id))
        .collect();
    let unmet_prereqs = staged_tasks
        .iter()
        .flat_map(|t| t.draft.dependencies.iter())
        .filter(|dep_id| {
            !model.forge.selected_tasks.contains(*dep_id)
                && model
                    .forge
                    .snapshot
                    .tasks
                    .iter()
                    .find(|t| &t.id == *dep_id)
                    .is_none_or(|t| t.state != "done")
        })
        .count();
    let missing_criteria = staged_tasks
        .iter()
        .filter(|t| t.draft.criteria.is_empty())
        .count();

    let mut composer_body = div()
        .id("batch-composer-body")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(sized_input(model.forge.batch_title.clone(), INPUT_MEDIUM))
                .child(
                    div()
                        .debug_selector(|| "forge-destination-guidance".into())
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::text_subtle())
                                .child("Destinations resolve from repository conventions."),
                        ),
                ),
        );

    if !staged_tasks.is_empty() {
        let mut queue_col = div().flex().flex_col();
        for task in &staged_tasks {
            let mut item = div()
                .py_2()
                .border_b_1()
                .border_color(theme::border())
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
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(task.draft.title.clone()),
                        )
                        .child(button_variant(
                            format!("remove-selected-task-{}", task.id),
                            "Remove",
                            ButtonVariant::Ghost,
                            ForgeAction::ToggleTask(task.id.clone()),
                            None,
                            cx,
                        )),
                );
            if let Some(input) = model.forge.path_hint_inputs.get(&task.id) {
                item = item.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .font_family(theme::FONT_MONO)
                                .text_color(theme::text_subtle())
                                .child("path:"),
                        )
                        .child(div().flex_1().min_w_0().child(input.clone())),
                );
            }
            queue_col = queue_col.child(item);
        }
        composer_body = composer_body.child(queue_col);

        if unmet_prereqs > 0 {
            composer_body =
                composer_body.child(div().text_xs().text_color(theme::warning()).child(format!(
                    "{unmet_prereqs} unresolved prerequisite(s) outside batch"
                )));
        }
        if model.forge.snapshot.composition_enabled && missing_criteria > 0 {
            composer_body = composer_body.child(
                div()
                    .text_xs()
                    .text_color(theme::warning())
                    .child(format!("{missing_criteria} task(s) missing criteria")),
            );
        }
    } else {
        composer_body = composer_body.child(
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child("Select ready tasks from the list to build a batch."),
        );
    }

    if !model.forge.snapshot.composition_enabled {
        // ADR-034 D1: only composed batches get the review policy.
        composer_body = composer_body.child(
            div()
                .debug_selector(|| "review-policy-needs-composition".into())
                .text_xs()
                .text_color(theme::text_subtle())
                .child(REVIEW_POLICY_NEEDS_COMPOSITION),
        );
    }

    let composer = div()
        .id("batch-composer")
        .debug_selector(|| "batch-composer".into())
        .w(px(300.0))
        .h_full()
        .min_h_0()
        .p_3()
        .rounded_lg()
        .bg(theme::surface_raised())
        .border_1()
        .border_color(if staged_tasks.is_empty() {
            theme::border()
        } else {
            theme::accent()
        })
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .pb_2()
                .border_b_1()
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
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("Compose pulsation"),
                        )
                        .child(widgets::info_tip(
                            "compose-pulsation",
                            "Select tasks to execute and integrate as a single batch.",
                        )),
                )
                .child(
                    div()
                        .text_xs()
                        .font_family(theme::FONT_MONO)
                        .text_color(if staged_tasks.is_empty() {
                            theme::text_subtle()
                        } else {
                            theme::accent()
                        })
                        .child(staged_tasks.len().to_string()),
                ),
        )
        .child(composer_body)
        .child(
            div()
                .pt_2()
                .border_t_1()
                .border_color(theme::border())
                .flex_shrink_0()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::text_muted())
                                .child(format!("{} selected", staged_tasks.len())),
                        )
                        .when(!staged_tasks.is_empty(), |row| {
                            row.child(button_variant(
                                "clear-selected-tasks",
                                "Clear",
                                ButtonVariant::Ghost,
                                ForgeAction::ClearSelectedTasks,
                                None,
                                cx,
                            ))
                        }),
                )
                .child(button_variant(
                    "dispatch-batch",
                    "Dispatch batch",
                    if staged_tasks.is_empty() {
                        ButtonVariant::Secondary
                    } else {
                        ButtonVariant::Primary
                    },
                    ForgeAction::Dispatch,
                    None,
                    cx,
                )),
        );

    div()
        .flex_1()
        .min_h_0()
        .min_w_0()
        .flex()
        .gap_3()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .flex_col()
                .gap_2()
                .child(command_bar)
                .child(filter_strip)
                .child(list),
        )
        .child(composer)
        .into_any_element()
}
