//! The Agents page: the canvas of agents (feature 25, M1) and, on the right,
//! the synthesis of the node clicked. The picker lives here; the agents'
//! model, the synthesis and its drawers, API Mode and the shared
//! configuration controls in their own modules. The console is the dock's.

use std::sync::Arc;

use gpui::{
    AnyElement, Context, FontWeight, IntoElement, KeyDownEvent, Rgba, SharedString, deferred, div,
    prelude::*, px, uniform_list,
};
use ikore_core::persistence_service::{
    CLI_VALIDATION_FAILED, CLI_VERSION_UNVALIDATED, CliFallbackCheckpoint, CliFallbackDecision,
    PulsationStatus,
};
use ikore_core::terminals::{
    AgentExecutionMode, CliAvailabilityState, CliBackend, CliModelCatalogEntry,
    CliModelCatalogState, CliModelSelectionSource, TerminalRunStatus,
};

use crate::agents::{AgentIdentifier, CliModelPickerState, CliModelPickerTarget};
use crate::agents_canvas::{CanvasAgents, CanvasNode, FREE_FORM_LIMIT, NodeState};
use crate::app_model::AppModel;
use crate::icons::Icon;
use crate::pipeline_status::PipelineStatus;
use crate::theme;

mod api;
mod config;
mod detail;
mod free_form;
mod history;
mod model;
pub(crate) mod pipeline_editor;
mod pipelines;
mod synthesis;

pub(crate) use pipeline_editor::PipelineEditor;

use api::api_detail_panel;
use config::field;
use detail::agent_detail_panel;
use model::{AgentItem, AgentItems, agent_items};

/// The default box's title: `Pipeline`, and the active pulsation's title,
/// stage and state when there is one.
fn pipeline_title(
    model: &AppModel,
    snapshot: &ikore_core::terminals::TerminalsSnapshot,
    cx: &gpui::App,
) -> String {
    let pulsations = model.pulsations.read(cx);
    let Some(active) = pulsations.active() else {
        return "Pipeline".to_owned();
    };
    let status = PipelineStatus::derive(active, Some(snapshot));
    format!(
        "Pipeline · {} · {} · {}",
        active.feature_name,
        crate::pipeline_status::step_label(active.stage),
        status.state
    )
}

/// The stage waiting on the developer at its checkpoint, by its agent.
fn waiting_role(model: &AppModel, cx: &gpui::App) -> Option<ikore_core::llm::AgentRole> {
    let pulsations = model.pulsations.read(cx);
    let active = pulsations.active()?;
    (active.status == PulsationStatus::Checkpoint).then(|| {
        ikore_core::llm::AgentRole::for_stage(crate::pulsations::stage_label(active.stage))
    })
}

/// A node from an agent: its state by the feature's table, and the line
/// under its name.
fn canvas_node(item: &AgentItem, waiting: bool, checkpoint: bool) -> CanvasNode {
    let run = item.run.as_ref();
    let time = run.and_then(crate::agent_dock_view::run_time);
    let (state, status) = if item.needs_attention() {
        let status = match run.map(|run| run.status) {
            Some(status @ (TerminalRunStatus::Failed | TerminalRunStatus::AwaitingFallback)) => {
                run_status(status).to_owned()
            }
            _ => "Fallback pending".to_owned(),
        };
        (NodeState::Attention, status)
    } else if run.is_some_and(|run| run.status.is_active()) {
        let mut status = match time {
            Some(time) => format!("Running {time}"),
            None => "Running".to_owned(),
        };
        if item.units > 1 {
            status.push_str(&format!(" · {} units", item.units));
        }
        (NodeState::Running, status)
    } else if waiting {
        (NodeState::Waiting, "Waiting on you".to_owned())
    } else if let Some(run) = run {
        let label = if run.status == TerminalRunStatus::Completed {
            "Done"
        } else {
            run_status(run.status)
        };
        let status = match time {
            Some(time) => format!("{label} · {time}"),
            None => label.to_owned(),
        };
        (NodeState::Done, status)
    } else {
        (NodeState::Idle, "Idle".to_owned())
    };
    CanvasNode {
        agent: item.id.clone(),
        label: item.label.clone(),
        color: item.color,
        state,
        status,
        transport: item.transport.clone(),
        checkpoint,
    }
}

/// What the canvas draws, from the agents of the snapshot.
fn canvas_agents(
    model: &AppModel,
    snapshot: &ikore_core::terminals::TerminalsSnapshot,
    items: &AgentItems,
    busy: bool,
    cx: &gpui::App,
) -> CanvasAgents {
    let waiting = waiting_role(model, cx);
    let node = |item: &AgentItem| {
        let role = match &item.id {
            AgentIdentifier::Canonical(role) => Some(*role),
            AgentIdentifier::FreeForm(_) | AgentIdentifier::PipelineStep { .. } => None,
        };
        let checkpoint = role
            .and_then(|role| {
                ikore_contracts::stages::STAGES
                    .iter()
                    .find(|(_, _, stage_role)| *stage_role == role)
            })
            .is_some_and(|(stage, _, _)| stage.requires_checkpoint());
        canvas_node(item, role.is_some() && role == waiting, checkpoint)
    };
    CanvasAgents {
        title: pipeline_title(model, snapshot, cx),
        stages: items.stages.iter().map(node).collect(),
        loose: items
            .support
            .iter()
            .chain(&items.free_form)
            .map(node)
            .collect(),
        ghost: (items.free_form.len() < FREE_FORM_LIMIT).then_some(!busy),
        archived: items.archived.len(),
        pipelines: pipelines::canvas_pipelines(model, cx),
        selected_pipeline: model.agents.read(cx).selected_pipeline.clone(),
    }
}

/// The right column: the synthesis of the node clicked, or the archived
/// free-form agents.
fn synthesis_panel(
    model: &AppModel,
    snapshot: &ikore_core::terminals::TerminalsSnapshot,
    busy: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let agents = model.agents.read(cx);
    let show_archived = agents.show_archived;
    let api_mode = match agents.selected_agent.as_ref() {
        Some(AgentIdentifier::Canonical(role)) => snapshot
            .agents
            .iter()
            .find(|agent| agent.config.agent_role == *role)
            .is_some_and(|agent| agent.config.execution_mode == AgentExecutionMode::ApiDirect),
        Some(AgentIdentifier::FreeForm(slot_id)) => snapshot
            .free_form_slots
            .iter()
            .find(|slot| slot.slot.id == *slot_id)
            .is_some_and(|slot| slot.slot.execution_mode == AgentExecutionMode::ApiDirect),
        Some(AgentIdentifier::PipelineStep { .. }) | None => false,
    };
    // Feature 25: the editor, the archived pipelines, a pipeline's synthesis
    // or a step's come before the agents' panels.
    let step = match agents.selected_agent.as_ref() {
        Some(AgentIdentifier::PipelineStep {
            pipeline_id,
            step_id,
        }) => agents
            .pipeline(pipeline_id)
            .cloned()
            .map(|pipeline| (pipeline, step_id.clone())),
        _ => None,
    };
    let pipeline = agents
        .selected_pipeline
        .as_ref()
        .and_then(|id| agents.pipeline(id))
        .cloned();
    let show_archived_pipelines = agents.show_archived_pipelines;
    let show_global_pipelines = agents.show_global_pipelines;
    let content = if model.pipeline_editor.is_some() {
        pipeline_editor::pipeline_editor_panel(model, cx)
    } else if show_archived_pipelines {
        pipelines::archived_pipelines(model, busy, cx)
    } else if show_global_pipelines {
        pipelines::global_pipelines(model, busy, cx)
    } else if let Some(pipeline) = pipeline {
        pipelines::pipeline_synthesis(model, &pipeline, busy, cx)
    } else if let Some((pipeline, step_id)) = step {
        pipelines::step_synthesis(model, &pipeline, &step_id, busy, cx)
    } else if show_archived {
        archived_panel(model, snapshot, busy, cx)
    } else if api_mode {
        api_detail_panel(model, snapshot, cx)
    } else {
        agent_detail_panel(model, snapshot, busy, cx)
    };
    div()
        .id("agents-synthesis")
        .debug_selector(|| "agents-synthesis".into())
        .size_full()
        .min_w_0()
        .min_h_0()
        .flex()
        .flex_col()
        .border_l_1()
        .border_color(theme::border())
        .bg(theme::surface())
        .child(content)
        .into_any_element()
}

/// `Archived · N`: the archived free-form agents, each with `Restore` and
/// `Delete`.
fn archived_panel(
    model: &AppModel,
    snapshot: &ikore_core::terminals::TerminalsSnapshot,
    busy: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let rows = snapshot
        .archived_free_form_slots
        .iter()
        .map(|slot| free_form::archived_free_form_card(model, slot, busy, cx))
        .collect::<Vec<_>>();
    let count = rows.len();
    div()
        .id("agents-drawer:archived")
        .debug_selector(|| "agents-drawer:archived".into())
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .p_4()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(format!("Archived · {count}")),
                )
                .child(icon_action(
                    "agents-drawer-close".into(),
                    Icon::X,
                    "Close",
                    true,
                    cx,
                    |model, cx| model.close_agents_synthesis(cx),
                )),
        )
        .when(count == 0, |panel| {
            panel.child(empty_state("No archived agents."))
        })
        .children(rows)
        .into_any_element()
}

pub(crate) fn agents_panel(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let agents = model.agents.read(cx);
    let loading = agents.loading;
    let busy = agents.busy;
    let snapshot = agents.snapshot.clone();
    let active_model_picker = agents.active_model_picker.clone();
    let selected = agents.selected_agent.clone();
    let synthesis_open = selected.is_some()
        || agents.show_archived
        || agents.selected_pipeline.is_some()
        || agents.show_archived_pipelines
        || agents.show_global_pipelines
        || model.pipeline_editor.is_some();
    let can_create = pipelines::can_create_pipeline(agents);
    let archived_pipelines = agents.pipelines.archived.len();
    let available_globals = agents.pipelines.available_globals.len();

    div()
        .id("agents-panel")
        .debug_selector(|| "agents-panel".into())
        .relative()
        .overflow_hidden()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .h_full()
        // Esc closes the drawer first, then the synthesis. The page holds no
        // focus of its own: the canvas keeps a lasting one (feature 25, M1),
        // and a handle made anew on every frame would drop the keyboard.
        .on_key_down(cx.listener(|model, event: &KeyDownEvent, _, cx| {
            if event.keystroke.key == "escape" && model.dismiss_agents_synthesis(cx) {
                cx.stop_propagation();
            }
        }))
        .flex()
        .flex_col()
        .bg(theme::canvas())
        .child(
            div()
                .id("agents-header")
                .debug_selector(|| "agents-header".into())
                .px_5()
                .py_3()
                .flex_shrink_0()
                .border_b_1()
                .border_color(theme::border())
                .flex()
                .items_center()
                .gap_2()
                .child(div().flex_1().min_w_0())
                .child(
                    div()
                        .min_w_0()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .text_ellipsis()
                        .child("Project-isolated configuration"),
                )
                .when(available_globals > 0, |header| {
                    header.child(action_button(
                        "agents-add-global-pipeline".into(),
                        "Add global pipeline\u{2026}".into(),
                        snapshot.is_some() && !busy,
                        false,
                        cx,
                        |model, cx| model.open_global_pipelines(cx),
                    ))
                })
                .when(archived_pipelines > 0, |header| {
                    header.child(action_button(
                        "agents-archived-pipelines".into(),
                        format!("Archived pipelines · {archived_pipelines}").into(),
                        snapshot.is_some(),
                        false,
                        cx,
                        |model, cx| model.open_archived_pipelines(cx),
                    ))
                })
                .child(
                    div()
                        .id("agents-new-pipeline-frame")
                        .when(!can_create, |frame| {
                            frame.tooltip(|_, cx| {
                                cx.new(|_| {
                                    crate::widgets::Tooltip::new(
                                        "A project has at most 5 pipelines of its own. Archive one first.",
                                    )
                                })
                                .into()
                            })
                        })
                        .child(action_button(
                            "agents-new-pipeline".into(),
                            "New pipeline".into(),
                            snapshot.is_some() && can_create && !busy,
                            false,
                            cx,
                            |model, cx| model.open_new_pipeline(cx),
                        )),
                )
                .child(action_button(
                    "agents-fit".into(),
                    "Fit".into(),
                    snapshot.is_some(),
                    false,
                    cx,
                    |model, cx| model.fit_agents_canvas(cx),
                ))
                .child(action_button(
                    "agents-refresh".into(),
                    if loading { "Checking…" } else { "Refresh" }.into(),
                    !loading && !busy,
                    false,
                    cx,
                    |model, cx| model.refresh_agent_diagnostics(cx),
                )),
        )
        .child(
            div()
                .id("agents-content")
                .debug_selector(|| "agents-content".into())
                .flex_1()
                .min_w_0()
                .min_h_0()
                .overflow_hidden()
                .child(match snapshot {
                    None => empty_state(if loading {
                        "Checking CLIs and loading the seven agents…"
                    } else {
                        "Open a workspace to configure and monitor agents."
                    }),
                    Some(snapshot) => {
                        let items = agent_items(model, &snapshot, cx);
                        let drawn = canvas_agents(model, &snapshot, &items, busy, cx);
                        let canvas = crate::agents_canvas::agents_canvas(
                            model,
                            drawn,
                            selected.as_ref(),
                            cx,
                        );
                        if synthesis_open {
                            crate::split_pane::split_pane(
                                "agents-columns",
                                &model.agents_split,
                                canvas,
                                synthesis_panel(model, &snapshot, busy, cx),
                                cx,
                            )
                        } else {
                            canvas
                        }
                    }
                }),
        )
        .when_some(
            active_model_picker.filter(|picker| picker.target != CliModelPickerTarget::Companion),
            |panel, picker| panel.child(cli_model_picker_panel(model, picker, cx)),
        )
        .into_any_element()
}

pub(crate) fn cli_model_picker_panel(
    model: &AppModel,
    picker: CliModelPickerState,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let agents = model.agents.read(cx);
    let catalog = agents.model_catalogs.get(&picker.backend).cloned();
    let loading = agents.catalog_loading_backend == Some(picker.backend);
    let error = agents.catalog_error.clone();
    let raw_query = model
        .cli_model_search_input
        .read(cx)
        .value()
        .trim()
        .to_string();
    let query = raw_query.to_lowercase();
    // The Companion chooses a model, whatever serves it: its picker spans every route. An
    // agent's picker lists the catalog of the agent's own CLI.
    let companion = picker.target == CliModelPickerTarget::Companion;
    let loading = if companion {
        loading || agents.catalogs_refreshing
    } else {
        loading
    };
    let entries = if companion {
        companion_picker_rows(model.chat.read(cx), &agents.model_catalogs, &query)
    } else {
        catalog
            .as_ref()
            .map(|catalog| picker_rows(&catalog.models, &query))
            .unwrap_or_default()
    };
    let entries = Arc::new(entries);
    let list_entries = Arc::clone(&entries);
    let title = match &picker.target {
        CliModelPickerTarget::Canonical(role) => format!(
            "Choose {} model for {} agent",
            picker.backend.label(),
            ikore_core::terminals::canonical_agent(*role).name
        ),
        CliModelPickerTarget::FreeForm(_) => {
            format!("Choose {} model for FreeForm agent", picker.backend.label())
        }
        CliModelPickerTarget::PipelineStep(_) => {
            format!(
                "Choose {} model for the pipeline step",
                picker.backend.label()
            )
        }
        CliModelPickerTarget::Companion => "Choose the Companion's model".into(),
    };
    let status = if loading {
        "loading".to_string()
    } else if companion {
        format!(
            "{} models across every route · the route in use is kept when it serves the model",
            entries.iter().filter(|row| row.routed.is_some()).count()
        )
    } else if let Some(catalog) = &catalog {
        let state = match catalog.state {
            CliModelCatalogState::Ready => "ready",
            CliModelCatalogState::Stale => "stale — showing the last valid snapshot",
            CliModelCatalogState::Missing => "CLI missing",
            CliModelCatalogState::AuthRequired => "authentication required",
            CliModelCatalogState::UnsupportedVersion => "unsupported CLI version",
            CliModelCatalogState::Failed => "catalog unavailable",
        };
        let version_label = format!(
            "{} v{}",
            picker.backend.label(),
            catalog.cli_version.as_deref().unwrap_or("—")
        );
        // Live catalogs come from the CLI itself, not from the embedded preset.
        let catalog_version_label = if matches!(
            picker.backend,
            CliBackend::Opencode | CliBackend::Antigravity
        ) {
            String::new()
        } else {
            format!(
                " · Catalog v{}",
                ikore_core::terminals::cli_model_catalog_version()
            )
        };
        format!(
            "{version_label}{catalog_version_label} · {state} · {} models",
            catalog.models.len()
        )
    } else {
        "not loaded".into()
    };
    let diagnostic = error.or_else(|| catalog.as_ref().and_then(|item| item.diagnostic.clone()));
    // An id typed by hand goes to a CLI; an API connection answers with its own model.
    let manual_enabled = !query.is_empty()
        && !loading
        && (!companion
            || model.chat.read(cx).selected_transport
                == ikore_core::chat_service::ChatTransportKind::Cli);

    let content = div()
        .id("cli-model-picker-content")
        .debug_selector(|| "cli-model-picker-content".into())
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .w(px(480.0))
        .max_w_full()
        .max_h_full()
        .overflow_y_scroll()
        .shadow_lg()
        .occlude()
        .p_3()
        .rounded_lg()
        .border_1()
        .border_color(theme::border_strong())
        .bg(theme::surface_raised())
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .flex_wrap()
                .flex_shrink_0()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::text_muted())
                                .child(status),
                        ),
                )
                .child(action_button(
                    "cli-model-refresh".into(),
                    if loading {
                        "Refreshing…"
                    } else {
                        "Refresh catalog"
                    }
                    .into(),
                    !loading,
                    false,
                    cx,
                    |model, cx| model.refresh_cli_model_catalog(cx),
                ))
                .child(icon_action(
                    "cli-model-close".into(),
                    Icon::X,
                    "Close",
                    true,
                    cx,
                    |model, cx| model.close_cli_model_picker(cx),
                )),
        )
        .child(field(
            "MODEL ID (SEARCH CATALOG OR TYPE/PASTE MANUALLY)",
            model.cli_model_search_input.clone(),
            crate::sizing::INPUT_LONG,
        ))
        .when_some(diagnostic, |panel, message| {
            panel.child(div().text_xs().text_color(theme::warning()).child(message))
        })
        .child(
            div()
                .h(px(300.0))
                .min_h_0()
                .border_1()
                .border_color(theme::border())
                .rounded_md()
                .when(entries.is_empty(), |list| {
                    list.child(div().p_3().text_sm().text_color(theme::text_muted()).child(
                        if loading {
                            "Loading model catalog…"
                        } else {
                            "No catalog model matches this search."
                        },
                    ))
                })
                .when(!entries.is_empty(), |list| {
                    list.child(
                        uniform_list(
                            "cli-model-virtual-list",
                            entries.len(),
                            cx.processor(
                                move |_model, range: std::ops::Range<usize>, _window, cx| {
                                    range
                                        .filter_map(|index| {
                                            let PickerRow {
                                                entry,
                                                recent,
                                                source,
                                                routed,
                                            } = list_entries.get(index)?.clone();
                                            let model_id = entry.model_id.clone();
                                            let is_default_entry = model_id.is_none();
                                            let has_profiles = !entry.profiles.is_empty();
                                            let provider = entry
                                                .provider_id
                                                .clone()
                                                .unwrap_or_else(|| "CLI".into());
                                            let identifier =
                                                entry.model_id.clone().unwrap_or_else(|| {
                                                    "Use the CLI configured default".into()
                                                });
                                            let mut details = Vec::new();
                                            if let Some(window) = entry.context_window {
                                                details.push(format_context_window(window));
                                            }
                                            if !entry.capabilities.is_empty() {
                                                details.push(entry.capabilities.join(", "));
                                            }
                                            if has_profiles {
                                                details.push(entry.profiles.join(", "));
                                            }
                                            if source == CliModelSelectionSource::Manual {
                                                details.push("manual ID".into());
                                            }
                                            if let Some((_, via)) = &routed {
                                                details.push(format!("via {via}"));
                                            }
                                            if let (Some(input_cost), Some(output_cost)) = (
                                                entry.input_cost_usd_per_million,
                                                entry.output_cost_usd_per_million,
                                            ) {
                                                details.push(format!(
                                                    "${input_cost:.2} / ${output_cost:.2} per 1M tokens"
                                                ));
                                            }
                                            let detail_line = if details.is_empty() {
                                                identifier.clone()
                                            } else {
                                                format!("{identifier} · {}", details.join(" · "))
                                            };
                                            Some(
                                                div()
                                                    .id(SharedString::from(format!(
                                                        "cli-model-option:{index}:{identifier}"
                                                    )))
                                                    .h(px(64.0))
                                                    .px_3()
                                                    .py_2()
                                                    .border_b_1()
                                                    .border_color(theme::border())
                                                    .cursor_pointer()
                                                    .hover(|style| {
                                                        style.bg(theme::surface_raised())
                                                    })
                                                    .on_click(cx.listener(
                                                        move |model, _, _, cx| match &routed {
                                                            Some((identity, _)) => model
                                                                .select_companion_model(
                                                                    identity, cx,
                                                                ),
                                                            None => model.select_cli_model(
                                                                model_id.clone(),
                                                                source,
                                                                cx,
                                                            ),
                                                        },
                                                    ))
                                                    .child(
                                                        div()
                                                            .flex()
                                                            .items_center()
                                                            .gap_2()
                                                            .child(
                                                                div()
                                                                    .w(px(110.0))
                                                                    .text_xs()
                                                                    .text_color(theme::accent())
                                                                    .child(provider),
                                                            )
                                                            .child(
                                                                div()
                                                                    .min_w_0()
                                                                    .flex_1()
                                                                    .font_weight(
                                                                        FontWeight::SEMIBOLD,
                                                                    )
                                                                    .child(entry.display_name),
                                                            )
                                                            .when(is_default_entry, |row| {
                                                                row.child(chip(
                                                                    "DEFAULT",
                                                                    theme::accent(),
                                                                ))
                                                            })
                                                            .when(recent, |row| {
                                                                row.child(chip(
                                                                    "RECENT",
                                                                    theme::accent(),
                                                                ))
                                                            })
                                                            .when(has_profiles, |row| {
                                                                row.child(chip(
                                                                    "REASONING",
                                                                    theme::text_muted(),
                                                                ))
                                                            }),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(theme::text_muted())
                                                            .child(detail_line),
                                                    )
                                                    .into_any_element(),
                                            )
                                        })
                                        .collect::<Vec<_>>()
                                },
                            ),
                        )
                        .h_full(),
                    )
                }),
        )
        .child({
            let no_catalog_match = manual_enabled && entries.is_empty();
            let manual_label = if raw_query.is_empty() {
                "Type or paste a model ID to apply it manually".to_string()
            } else {
                format!("Apply \"{raw_query}\" as manual model ID")
            };
            div()
                .flex()
                .flex_wrap()
                .flex_shrink_0()
                .items_center()
                .gap_2()
                .child(action_button(
                    "cli-model-manual".into(),
                    manual_label.into(),
                    manual_enabled,
                    no_catalog_match,
                    cx,
                    |model, cx| model.select_manual_cli_model(cx),
                ))
                .child(crate::widgets::info_tip(
                    "cli-model-manual",
                    "Manual IDs are sent directly via --model. No profile verification.",
                ))
        })
        .into_any_element();
    deferred(
        div()
            .id("cli-model-picker-overlay")
            .debug_selector(|| "cli-model-picker-overlay".into())
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|model, _, _, cx| {
                    model.close_cli_model_picker(cx);
                    cx.stop_propagation();
                }),
            )
            .absolute()
            .inset_0()
            .p_2()
            .flex()
            .justify_end()
            .items_start()
            .occlude()
            .bg(gpui::rgba(0x00000066))
            .on_key_down(cx.listener(|model, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    model.close_cli_model_picker(cx);
                    cx.stop_propagation();
                }
            }))
            .child(content),
    )
    .with_priority(10)
    .into_any_element()
}

/// Renders a token context window compactly, e.g. `200_000` -> `"200k"`, `1_000_000` -> `"1M"`.
pub(crate) fn format_context_window(window: u64) -> String {
    if window >= 1_000_000 && window.is_multiple_of(1_000_000) {
        format!("{}M", window / 1_000_000)
    } else if window >= 1_000 && window.is_multiple_of(1_000) {
        format!("{}k", window / 1_000)
    } else {
        window.to_string()
    }
}

/// A row of the model picker: a catalog entry, or a recent model the catalog does not list.
#[derive(Debug, Clone, PartialEq)]
struct PickerRow {
    entry: CliModelCatalogEntry,
    /// One of the models the Companion used last in this project.
    recent: bool,
    source: CliModelSelectionSource,
    /// In the Companion's picker a row is a model on all its routes: its identity, and the
    /// routes that serve it as one line.
    routed: Option<(ikore_core::llm::ModelIdentity, String)>,
}

/// The rows of the Companion's picker: every model it can be pointed at, by vendor, each one
/// once with the routes that serve it. Without a search, the CLI's own default comes first, then
/// the models this project's chats used last.
fn companion_picker_rows(
    chat: &crate::chat::ChatModel,
    catalogs: &std::collections::HashMap<
        CliBackend,
        ikore_core::terminals::CliModelCatalogSnapshot,
    >,
    query: &str,
) -> Vec<PickerRow> {
    use ikore_core::llm::{ModelIdentity, ModelRoute};
    let recent: Vec<ModelIdentity> = CliBackend::ALL
        .into_iter()
        .flat_map(|backend| {
            chat.recent_cli_models(backend)
                .into_iter()
                .filter_map(move |id| ModelIdentity::of(&id, backend.as_str()))
        })
        .collect();
    let mut rows: Vec<PickerRow> = Vec::new();
    for group in crate::companion_models::companion_models(chat, catalogs) {
        for model in group.models {
            let via = model
                .routes
                .iter()
                .map(|offer| crate::companion_models::route_text(chat, &offer.route))
                .collect::<Vec<_>>()
                .join(", ");
            // What a catalog says about the model, from the first CLI that lists it.
            let listed = model.routes.iter().find_map(|offer| match &offer.route {
                ModelRoute::Cli(backend) => catalogs
                    .get(backend)?
                    .models
                    .iter()
                    .find(|entry| entry.model_id.as_deref() == Some(offer.model_id.as_str())),
                ModelRoute::Api { .. } | ModelRoute::Gateway { .. } => None,
            });
            let entry = CliModelCatalogEntry {
                provider_id: Some(group.vendor.label().into()),
                model_id: Some(model.identity.model.clone()),
                display_name: listed.map_or_else(
                    || model.identity.model.clone(),
                    |entry| entry.display_name.clone(),
                ),
                context_window: listed.and_then(|entry| entry.context_window),
                input_cost_usd_per_million: listed
                    .and_then(|entry| entry.input_cost_usd_per_million),
                output_cost_usd_per_million: listed
                    .and_then(|entry| entry.output_cost_usd_per_million),
                capabilities: listed
                    .map(|entry| entry.capabilities.clone())
                    .unwrap_or_default(),
                profiles: listed
                    .map(|entry| entry.profiles.clone())
                    .unwrap_or_default(),
                raw_metadata: serde_json::Value::Null,
            };
            if !model_entry_matches(&entry, query) && !via.to_lowercase().contains(query) {
                continue;
            }
            rows.push(PickerRow {
                entry,
                recent: recent.contains(&model.identity),
                source: CliModelSelectionSource::Catalog,
                routed: Some((model.identity, via)),
            });
        }
    }
    if !query.is_empty() {
        return rows;
    }
    // A stable sort: the recent models keep their order by vendor and name, and so do the rest.
    rows.sort_by_key(|row| !row.recent);
    if chat.selected_transport != ikore_core::chat_service::ChatTransportKind::Cli {
        return rows;
    }
    // On a CLI, two more choices come first: its own default, and the ids typed by hand for it
    // before, which no catalog lists.
    let backend = chat.selected_cli_backend;
    let cli = format!("{} CLI", backend.label());
    let mut first = vec![PickerRow {
        entry: bare_entry(cli.clone(), None, "CLI default".into()),
        recent: false,
        source: CliModelSelectionSource::Default,
        routed: None,
    }];
    for id in chat.recent_cli_models(backend) {
        let listed = ModelIdentity::of(&id, backend.as_str()).is_some_and(|identity| {
            rows.iter().any(|row| {
                row.routed
                    .as_ref()
                    .is_some_and(|(listed, _)| *listed == identity)
            })
        });
        if !listed {
            first.push(PickerRow {
                entry: bare_entry(cli.clone(), Some(id.clone()), id),
                recent: true,
                source: CliModelSelectionSource::Manual,
                routed: None,
            });
        }
    }
    first.extend(rows);
    first
}

/// The rows an agent's picker lists: the catalog of its CLI, filtered by the search.
fn picker_rows(catalog: &[CliModelCatalogEntry], query: &str) -> Vec<PickerRow> {
    catalog
        .iter()
        .filter(|entry| model_entry_matches(entry, query))
        .map(|entry| PickerRow {
            entry: entry.clone(),
            recent: false,
            source: if entry.model_id.is_some() {
                CliModelSelectionSource::Catalog
            } else {
                CliModelSelectionSource::Default
            },
            routed: None,
        })
        .collect()
}

/// A model that only its id describes: the CLI's own default, or an id typed by hand.
fn bare_entry(provider: String, model_id: Option<String>, name: String) -> CliModelCatalogEntry {
    CliModelCatalogEntry {
        provider_id: Some(provider),
        model_id,
        display_name: name,
        context_window: None,
        input_cost_usd_per_million: None,
        output_cost_usd_per_million: None,
        capabilities: Vec::new(),
        profiles: Vec::new(),
        raw_metadata: serde_json::Value::Null,
    }
}

fn model_entry_matches(entry: &CliModelCatalogEntry, query: &str) -> bool {
    query.is_empty()
        || entry.display_name.to_lowercase().contains(query)
        || entry
            .model_id
            .as_deref()
            .is_some_and(|value| value.to_lowercase().contains(query))
        || entry
            .provider_id
            .as_deref()
            .is_some_and(|value| value.to_lowercase().contains(query))
        || entry
            .capabilities
            .iter()
            .any(|value| value.to_lowercase().contains(query))
}

pub(crate) fn fallback_panel(
    fallback: &CliFallbackCheckpoint,
    cancel_armed: bool,
    busy: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let checkpoint_id = fallback.checkpoint_id.clone();
    let checkpoint_for_retry = checkpoint_id.clone();
    let checkpoint_for_validate = checkpoint_id.clone();
    let checkpoint_for_cancel = checkpoint_id.clone();
    let state_version = fallback.state_version;
    let descriptor = ikore_core::terminals::canonical_agent(fallback.agent_role);
    // Three shapes: an updated CLI awaiting validation (validate / switch / abort), a CLI that
    // failed validation (switch / abort), and every other transport failure (API / retry /
    // cancel).
    let unvalidated = fallback.error_code == CLI_VERSION_UNVALIDATED;
    let validation_failed = fallback.error_code == CLI_VALIDATION_FAILED;
    // ADR-028 pre-flight: nothing failed; the stage asks before spending the last of a window.
    let quota_low = fallback.error_code == ikore_core::persistence_service::CLI_QUOTA_LOW;
    let (title, provider_label, is_error) = if quota_low {
        (
            format!(
                "Plan quota low — decide before this stage · {} · {}",
                descriptor.name, fallback.feature_name
            ),
            "Use API",
            false,
        )
    } else if unvalidated {
        (
            format!(
                "CLI updated — validation required · {} · {}",
                descriptor.name, fallback.feature_name
            ),
            "Switch provider (API)",
            false,
        )
    } else if validation_failed {
        (
            format!(
                "CLI failed validation · {} · {}",
                descriptor.name, fallback.feature_name
            ),
            "Switch provider (API)",
            true,
        )
    } else {
        (
            format!(
                "Fallback awaiting decision · {} · {}",
                descriptor.name, fallback.feature_name
            ),
            "Use API",
            false,
        )
    };
    let tone = if is_error {
        theme::danger()
    } else {
        theme::warning()
    };
    div()
        .id(SharedString::from(format!(
            "cli-fallback:{}",
            fallback.checkpoint_id
        )))
        .debug_selector({
            let id = fallback.checkpoint_id.clone();
            move || format!("cli-fallback:{id}")
        })
        .mx_4()
        .mt_3()
        .p_3()
        .rounded_lg()
        .border_1()
        .border_color(tone)
        .bg(if is_error {
            theme::danger_soft()
        } else {
            theme::warning_soft()
        })
        .flex()
        .items_center()
        .flex_wrap()
        .gap_3()
        .child(
            div()
                .flex_1()
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(tone)
                        .child(title),
                )
                .child(
                    div()
                        .mt_1()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(format!("{} · {}", fallback.error_code, fallback.diagnostic)),
                ),
        )
        .when(unvalidated, |card| {
            card.child(action_button(
                SharedString::from(format!("fallback-validate:{checkpoint_for_validate}")),
                "Update & validate CLI".into(),
                !busy,
                false,
                cx,
                move |model, cx| {
                    model.validate_agent_cli_fallback(
                        checkpoint_for_validate.clone(),
                        state_version,
                        cx,
                    )
                },
            ))
        })
        .child(action_button(
            SharedString::from(format!("fallback-api:{checkpoint_id}")),
            provider_label.into(),
            !busy,
            false,
            cx,
            move |model, cx| {
                model.resolve_agent_cli_fallback(
                    checkpoint_id.clone(),
                    state_version,
                    CliFallbackDecision::UseApi,
                    cx,
                )
            },
        ))
        .when(!unvalidated && !validation_failed, |card| {
            card.child(action_button(
                SharedString::from(format!("fallback-retry:{checkpoint_for_retry}")),
                if quota_low {
                    "Continue with CLI"
                } else {
                    "Retry CLI"
                }
                .into(),
                !busy,
                false,
                cx,
                move |model, cx| {
                    model.resolve_agent_cli_fallback(
                        checkpoint_for_retry.clone(),
                        state_version,
                        CliFallbackDecision::RetryCli,
                        cx,
                    )
                },
            ))
        })
        .child(action_button(
            SharedString::from(format!("fallback-cancel:{checkpoint_for_cancel}")),
            if cancel_armed {
                "Confirm abort"
            } else if unvalidated || validation_failed {
                "Abort pulsation"
            } else {
                "Cancel pulse"
            }
            .into(),
            !busy,
            true,
            cx,
            move |model, cx| {
                model.request_cancel_agent_cli_fallback(
                    checkpoint_for_cancel.clone(),
                    state_version,
                    cx,
                )
            },
        ))
        .into_any_element()
}

/// Label shown for a configured CLI model, e.g. `"gpt-5.6-sol"`, `"gpt-5.6-sol [manual]"`
/// or `"CLI default"`. Shared by every agent-card/chat-view model readout so the three
/// previously-duplicated call sites can't drift from each other.
pub(crate) fn cli_model_label(source: CliModelSelectionSource, model: Option<&str>) -> String {
    let resolved = model.unwrap_or("CLI default");
    match source {
        CliModelSelectionSource::Manual => format!("{resolved} [manual]"),
        _ => resolved.to_string(),
    }
}

/// Where a run came from, in one word.
fn origin_label(origin: ikore_core::terminals::ExecutionOrigin) -> &'static str {
    use ikore_core::terminals::ExecutionOrigin;
    match origin {
        ExecutionOrigin::Pipeline => "pipeline",
        ExecutionOrigin::Learning => "learning",
        ExecutionOrigin::Manual => "manual",
        ExecutionOrigin::Feature => "feature",
        ExecutionOrigin::Review => "review",
        ExecutionOrigin::Notes => "notes",
        ExecutionOrigin::Reconciliation => "reconciliation",
        ExecutionOrigin::Automation => "pipeline step",
    }
}

fn run_metadata(run: &ikore_core::terminals::TerminalRun) -> String {
    let duration = crate::time::format_time_range(
        run.started_at.as_deref().or(Some(&run.created_at)),
        run.finished_at.as_deref(),
        &crate::time::format_timestamp(&run.created_at),
    );
    let session = run
        .native_session_id
        .as_deref()
        .map(|id| id.chars().take(8).collect::<String>())
        .unwrap_or_else(|| "no session".into());
    let mut metadata = format!(
        "{} · {} · {} · exit {} · version {} · model {} · session {}",
        run_status(run.status),
        origin_label(run.origin),
        duration,
        run.exit_code
            .map_or_else(|| "—".into(), |code| code.to_string()),
        run.detected_cli_version.as_deref().unwrap_or("—"),
        run.model.as_deref().unwrap_or("default"),
        session,
    );
    if let Some(error_message) = run
        .error_message
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        metadata.push_str(" · error: ");
        metadata.push_str(error_message);
    }
    if let Some(reason) = run
        .termination_reason
        .as_deref()
        .filter(|value| !value.is_empty() && Some(*value) != run.error_message.as_deref())
    {
        metadata.push_str(" · reason: ");
        metadata.push_str(reason);
    }
    metadata
}

/// An arrow or an X in place of a worded button; the tooltip names the action.
fn icon_action(
    id: SharedString,
    glyph: Icon,
    tooltip: &'static str,
    enabled: bool,
    cx: &mut Context<AppModel>,
    handler: impl Fn(&mut AppModel, &mut Context<AppModel>) + 'static,
) -> AnyElement {
    crate::widgets::icon_button(
        id,
        glyph,
        tooltip,
        false,
        None,
        enabled,
        cx,
        move |model, _, cx| handler(model, cx),
    )
}

/// Whether the selected run's session can go on, and whether it goes on in a
/// new session. A CLI session keeps its backend and model, so after either
/// changed the resume opens a new session with a summary of this one (PD10):
/// that needs the CLI ready, not native resuming.
pub(crate) fn session_continuation(
    run: &ikore_core::terminals::TerminalRun,
    backend: ikore_core::terminals::CliBackend,
    model: Option<&str>,
    native_resume: bool,
    executable_ready: bool,
) -> (bool, bool) {
    let new_session = ikore_core::terminals::needs_handoff(run, backend, model);
    let possible = !run.status.is_active()
        && run.native_session_id.is_some()
        && run.session_fingerprint.is_some()
        && if new_session {
            executable_ready
        } else {
            native_resume
        };
    (possible, new_session)
}

fn resume_label(new_session: bool) -> SharedString {
    if new_session {
        "Continue with summary".into()
    } else {
        "Resume session".into()
    }
}

fn action_button(
    id: SharedString,
    label: SharedString,
    enabled: bool,
    danger: bool,
    cx: &mut Context<AppModel>,
    handler: impl Fn(&mut AppModel, &mut Context<AppModel>) + 'static,
) -> impl IntoElement {
    let is_copy = id.starts_with("agent-copy:") || id.starts_with("free-form-copy:");
    // Starting, resuming, sending, validating and saving are the panel's primary acts; refresh,
    // copy, filter, paging and layout controls stay secondary so one button leads at a time. In
    // the API form only binding leads: saving a connection or a route are intermediate steps.
    const PRIMARY: [&str; 13] = [
        "agent-start:",
        "agent-expanded-start:",
        "agent-resume:",
        "agent-save-fields:",
        "free-form-start:",
        "free-form-expanded-start:",
        "free-form-resume:",
        "free-form-save:",
        "api-send-free-form:",
        "api-save-binding:",
        "api-bind-free-form:",
        "fallback-validate:",
        "fallback-retry:",
    ];
    let primary = PRIMARY.iter().any(|prefix| id.starts_with(prefix));
    let variant = if danger {
        crate::widgets::ButtonVariant::Danger
    } else if primary {
        crate::widgets::ButtonVariant::Primary
    } else {
        crate::widgets::ButtonVariant::Secondary
    };
    let glyph = crate::widgets::action_icon(&id);
    crate::widgets::button_frame(
        div()
            .id(id.clone())
            .when(is_copy, |d| d.w(px(84.0)))
            .debug_selector(move || id.to_string()),
        variant,
        enabled,
    )
    .when(enabled, |button| {
        button.on_click(cx.listener(move |model, _, _, cx| handler(model, cx)))
    })
    .gap_2()
    .when_some(glyph, |button, glyph| {
        button.child(crate::widgets::icon(
            glyph,
            px(14.0),
            crate::widgets::button_ink(variant, enabled),
        ))
    })
    .child(label)
}

fn chip(label: &'static str, color: Rgba) -> gpui::Div {
    div()
        .px_2()
        .py_1()
        .rounded_full()
        .border_1()
        .border_color(color)
        .text_xs()
        .text_color(color)
        .child(label)
}

fn empty_state(message: &'static str) -> AnyElement {
    crate::widgets::empty_state(message)
}

fn availability_status(
    availability: Option<&ikore_core::terminals::CliAvailability>,
) -> &'static str {
    match availability.map(|item| item.state) {
        Some(CliAvailabilityState::Available) => "Idle",
        Some(CliAvailabilityState::Unsupported) => "Unsupported",
        Some(CliAvailabilityState::AuthRequired) => "Auth required",
        Some(CliAvailabilityState::Probing) => "Probing",
        Some(CliAvailabilityState::UnsupportedVersion) => "Unsupported version",
        Some(CliAvailabilityState::CapabilityMissing) => "Capability missing",
        Some(CliAvailabilityState::ProbeFailed) => "Probe failed",
        _ => "Missing",
    }
}

pub(crate) fn run_status(status: TerminalRunStatus) -> &'static str {
    match status {
        TerminalRunStatus::Queued => "Queued",
        TerminalRunStatus::Starting => "Starting",
        TerminalRunStatus::Running => "Running",
        TerminalRunStatus::AwaitingFallback => "Awaiting fallback",
        TerminalRunStatus::Completed => "Completed",
        TerminalRunStatus::Failed => "Failed",
        TerminalRunStatus::Cancelled => "Cancelled",
        TerminalRunStatus::Interrupted => "Interrupted",
    }
}

fn status_color(status: Option<TerminalRunStatus>) -> Rgba {
    match status {
        Some(TerminalRunStatus::Failed | TerminalRunStatus::Interrupted) => theme::danger(),
        Some(TerminalRunStatus::AwaitingFallback) => theme::warning(),
        Some(TerminalRunStatus::Completed) => theme::accent(),
        _ => theme::text_muted(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ADR-040 M7 (12.3): the Agents page speaks English; the Portuguese words
    /// that once lived here do not come back.
    #[test]
    fn agents_strings_are_english() {
        let sources = [
            ("mod.rs", include_str!("mod.rs")),
            ("api.rs", include_str!("api.rs")),
            ("config.rs", include_str!("config.rs")),
            ("detail.rs", include_str!("detail.rs")),
            ("free_form.rs", include_str!("free_form.rs")),
            ("history.rs", include_str!("history.rs")),
            ("model.rs", include_str!("model.rs")),
            ("synthesis.rs", include_str!("synthesis.rs")),
            ("pipelines.rs", include_str!("pipelines.rs")),
            ("pipeline_editor.rs", include_str!("pipeline_editor.rs")),
            ("canvas", include_str!("../agents_canvas/mod.rs")),
            ("canvas paint", include_str!("../agents_canvas/paint.rs")),
            ("canvas layout", include_str!("../agents_canvas/layout.rs")),
        ];
        // Spelled apart, so this test does not find itself.
        let words = [
            ["Arqui", "var"],
            ["Expor", "tar"],
            ["pend", "ente"],
            ["arqui", "vado"],
            ["Resta", "urar"],
            ["Exclu", "ir"],
            ["Hist", "órico"],
            ["Ativi", "dade"],
            ["Config", "uração"],
        ];
        for (file, source) in sources {
            for [head, tail] in words {
                let word = format!("{head}{tail}");
                assert!(!source.contains(&word), "{file} says `{word}`");
            }
        }
    }

    /// Feature 25, M1: a node's state and line come from its run, as the
    /// feature's table of states says.
    #[test]
    fn a_node_takes_its_state_from_its_run() {
        use crate::agents_canvas::NodeState;
        use ikore_core::llm::AgentRole;

        let run = |status: TerminalRunStatus| {
            let mut run = crate::performance_agent_dock::agent(AgentRole::BackendAgent)
                .recent_runs
                .remove(0);
            run.status = status;
            run
        };
        let item =
            |run: Option<ikore_core::terminals::TerminalRun>, fallbacks, units| model::AgentItem {
                id: AgentIdentifier::Canonical(AgentRole::BackendAgent),
                label: "BackendAgent".into(),
                color: theme::accent(),
                run,
                transport: "CLI · claude".into(),
                fallbacks,
                units,
            };
        let idle = canvas_node(&item(None, 0, 0), false, false);
        assert_eq!(
            (idle.state, idle.status.as_str()),
            (NodeState::Idle, "Idle")
        );
        let running = canvas_node(
            &item(Some(run(TerminalRunStatus::Running)), 0, 2),
            false,
            false,
        );
        assert_eq!(running.state, NodeState::Running);
        assert!(running.status.starts_with("Running"), "{}", running.status);
        assert!(running.status.ends_with(" · 2 units"), "{}", running.status);
        let waiting = canvas_node(
            &item(Some(run(TerminalRunStatus::Completed)), 0, 0),
            true,
            true,
        );
        assert_eq!(
            (waiting.state, waiting.status.as_str(), waiting.checkpoint),
            (NodeState::Waiting, "Waiting on you", true)
        );
        let failed = canvas_node(
            &item(Some(run(TerminalRunStatus::Failed)), 0, 0),
            false,
            false,
        );
        assert_eq!(failed.state, NodeState::Attention);
        assert_eq!(failed.status, run_status(TerminalRunStatus::Failed));
        let fallback = canvas_node(
            &item(Some(run(TerminalRunStatus::Completed)), 1, 0),
            false,
            false,
        );
        assert_eq!(
            (fallback.state, fallback.status.as_str()),
            (NodeState::Attention, "Fallback pending")
        );
        let done = canvas_node(
            &item(Some(run(TerminalRunStatus::Completed)), 0, 0),
            false,
            false,
        );
        assert_eq!(done.state, NodeState::Done);
        assert!(done.status.starts_with("Done"), "{}", done.status);
    }

    #[test]
    fn searchable_model_picker_filters_a_thousand_entries_by_all_supported_fields() {
        let entries = (0..1_000)
            .map(|index| CliModelCatalogEntry {
                provider_id: Some(format!("provider-{}", index % 10)),
                model_id: Some(format!("provider-{}/model-{index}", index % 10)),
                display_name: format!("Model {index}"),
                context_window: Some(128_000),
                input_cost_usd_per_million: None,
                output_cost_usd_per_million: None,
                capabilities: if index == 777 {
                    vec!["vision-special".into()]
                } else {
                    vec!["tools".into()]
                },
                profiles: vec!["standard".into()],
                raw_metadata: serde_json::json!({}),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            entries
                .iter()
                .filter(|entry| model_entry_matches(entry, "provider-4/model-994"))
                .count(),
            1
        );
        assert_eq!(
            entries
                .iter()
                .filter(|entry| model_entry_matches(entry, "vision-special"))
                .count(),
            1
        );
        assert_eq!(
            entries
                .iter()
                .filter(|entry| model_entry_matches(entry, "provider-3"))
                .count(),
            100
        );
    }

    #[test]
    fn the_companion_picker_lists_each_model_once_across_its_routes() {
        use ikore_core::chat_service::{ChatConversation, ChatTransportKind};
        use ikore_core::llm::ProviderConnection;
        use ikore_core::terminals::{CliModelCatalogSnapshot, CliModelCatalogSource};
        let catalog = |backend: CliBackend, models: &[&str]| CliModelCatalogSnapshot {
            project_id: "project".into(),
            backend,
            adapter_contract_version: 2,
            state: CliModelCatalogState::Ready,
            source: CliModelCatalogSource::Cli,
            cli_version: None,
            diagnostic: None,
            refreshed_at: "now".into(),
            models: models
                .iter()
                .map(|id| CliModelCatalogEntry {
                    provider_id: None,
                    model_id: Some((*id).into()),
                    display_name: id.to_uppercase(),
                    context_window: Some(200_000),
                    input_cost_usd_per_million: None,
                    output_cost_usd_per_million: None,
                    capabilities: Vec::new(),
                    profiles: vec!["high".into()],
                    raw_metadata: serde_json::json!({}),
                })
                .collect(),
        };
        let catalogs = std::collections::HashMap::from([
            (
                CliBackend::Codex,
                catalog(CliBackend::Codex, &["gpt-5", "gpt-5-codex"]),
            ),
            (
                CliBackend::Claude,
                catalog(CliBackend::Claude, &["claude-sonnet-4-5"]),
            ),
        ]);
        let connection = |id: &str, provider: &str, name: &str, model: &str| ProviderConnection {
            id: id.into(),
            provider_id: provider.into(),
            display_name: name.into(),
            endpoint_url: None,
            default_model: model.into(),
            enabled: true,
            state: "active".into(),
            blocked_reason: None,
            cooldown_until: None,
            max_concurrency: 1,
            version: 1,
            has_api_key: true,
            chat_eligible: true,
            created_at: "now".into(),
            updated_at: "now".into(),
        };
        let used = |id: &str, model: &str| ChatConversation {
            id: id.into(),
            project_id: "project".into(),
            connection_id: None,
            provider_id: "codex".into(),
            model_id: model.into(),
            transport: ChatTransportKind::Cli,
            cli_backend: Some(CliBackend::Codex),
            model_profile: None,
            model_source: CliModelSelectionSource::Catalog,
            title: id.into(),
            connection_available: true,
            created_at: "now".into(),
            updated_at: "now".into(),
        };
        let mut chat = crate::chat::ChatModel::default();
        chat.connections = vec![
            connection("key", "openai", "OpenAI key", "gpt-5"),
            connection(
                "router",
                "openrouter",
                "OpenRouter",
                "anthropic/claude-sonnet-4-5",
            ),
        ];
        chat.conversations = vec![used("a", "gpt-5-codex"), used("b", "typed/by-hand")];
        chat.selected_transport = ChatTransportKind::Cli;
        chat.selected_cli_backend = CliBackend::Codex;
        // A recent model carries a star; a model on several routes names them all.
        let listed = |rows: &[PickerRow]| {
            rows.iter()
                .map(|row| {
                    format!(
                        "{}{}{}",
                        row.entry.display_name,
                        if row.recent { "*" } else { "" },
                        row.routed
                            .as_ref()
                            .map(|(_, via)| format!(" via {via}"))
                            .unwrap_or_default()
                    )
                })
                .collect::<Vec<_>>()
        };
        let rows = companion_picker_rows(&chat, &catalogs, "");
        assert_eq!(
            listed(&rows),
            [
                "CLI default",
                "typed/by-hand*",
                "GPT-5-CODEX* via Codex CLI",
                "GPT-5 via Codex CLI, API · OpenAI key",
                "CLAUDE-SONNET-4-5 via Claude CLI, Gateway · OpenRouter",
            ]
        );
        // The default and an id typed by hand belong to the CLI in use, not to a routed model.
        assert_eq!(rows[0].source, CliModelSelectionSource::Default);
        assert_eq!(rows[1].source, CliModelSelectionSource::Manual);
        assert!(rows[..2].iter().all(|row| row.routed.is_none()));
        // The vendor is a column of the row, and the catalog fills in what it knows.
        assert_eq!(rows[3].entry.provider_id.as_deref(), Some("OpenAI"));
        assert_eq!(rows[3].entry.context_window, Some(200_000));
        assert_eq!(rows[4].entry.provider_id.as_deref(), Some("Anthropic"));

        // A search matches the model, its vendor or one of its routes, in list order.
        assert_eq!(
            listed(&companion_picker_rows(&chat, &catalogs, "openrouter")),
            ["CLAUDE-SONNET-4-5 via Claude CLI, Gateway · OpenRouter"]
        );
        assert_eq!(
            listed(&companion_picker_rows(&chat, &catalogs, "openai")),
            [
                "GPT-5 via Codex CLI, API · OpenAI key",
                "GPT-5-CODEX* via Codex CLI"
            ]
        );
        // On an API connection there is no CLI default to go back to.
        chat.selected_transport = ChatTransportKind::Api;
        assert_eq!(
            listed(&companion_picker_rows(&chat, &catalogs, ""))[0],
            "GPT-5-CODEX* via Codex CLI"
        );
    }

    #[test]
    fn an_agent_picker_lists_the_catalog_of_its_cli() {
        let entry = |id: Option<&str>| {
            bare_entry(
                "anthropic".into(),
                id.map(Into::into),
                id.unwrap_or("CLI default").into(),
            )
        };
        let catalog = [entry(None), entry(Some("opus")), entry(Some("sonnet"))];
        let rows = picker_rows(&catalog, "");
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].source, CliModelSelectionSource::Default);
        assert_eq!(rows[1].source, CliModelSelectionSource::Catalog);
        assert!(rows.iter().all(|row| row.routed.is_none() && !row.recent));
        let found = picker_rows(&catalog, "son");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].entry.model_id.as_deref(), Some("sonnet"));
    }
}
