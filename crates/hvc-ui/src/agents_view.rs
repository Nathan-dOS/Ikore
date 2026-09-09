use std::sync::Arc;

use gpui::{
    AnyElement, Context, FontWeight, IntoElement, Rgba, SharedString, deferred, div, prelude::*,
    px, uniform_list,
};
use hvc_core::llm::AgentRole;
use hvc_core::persistence_service::{CliFallbackCheckpoint, CliFallbackDecision};
use hvc_core::terminals::{
    AgentExecutionMode, CanonicalAgentDescriptor, CliAvailabilityState, CliBackend,
    CliModelCatalogEntry, CliModelCatalogState, CliModelSelectionSource, TerminalEventKind,
    TerminalRunStatus,
};

use crate::agents::{
    AgentIdentifier, AgentsTab, CliModelPickerState, CliModelPickerTarget, TerminalEventFilter,
};
use crate::app_model::AppModel;
use crate::theme;

pub(crate) fn agents_panel(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let agents = model.agents.read(cx);
    let tab = agents.tab;
    let loading = agents.loading;
    let busy = agents.busy;
    let snapshot = agents.snapshot.clone();
    let active_model_picker = agents.active_model_picker.clone();

    div()
        .relative()
        .overflow_hidden()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .h_full()
        .flex()
        .flex_col()
        .bg(theme::canvas())
        .child(
            div()
                .px_5()
                .py_3()
                .border_b_1()
                .border_color(theme::border())
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(tab_button(
                    "agents-tab-cli",
                    "CLI Mode",
                    tab == AgentsTab::Cli,
                    cx,
                    |model, cx| model.set_agents_tab(AgentsTab::Cli, cx),
                ))
                .child(tab_button(
                    "agents-tab-api",
                    "API Key Mode",
                    tab == AgentsTab::Api,
                    cx,
                    |model, cx| model.set_agents_tab(AgentsTab::Api, cx),
                ))
                .child(
                    div()
                        .ml_auto()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::text_muted())
                                .child("Project-isolated configuration"),
                        )
                        .child(action_button(
                            "agents-refresh".into(),
                            if loading { "Checking…" } else { "Refresh" }.into(),
                            !loading && !busy,
                            false,
                            cx,
                            |model, cx| model.refresh_agent_diagnostics(cx),
                        )),
                ),
        )
        .child(match snapshot {
            None => empty_state(if loading {
                "Checking CLIs and loading the seven agents…"
            } else {
                "Open a workspace to configure and monitor agents."
            }),
            Some(snapshot) => match tab {
                AgentsTab::Cli => div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .child(agents_sidebar(model, &snapshot, busy, cx))
                    .child(agent_detail_panel(model, &snapshot, busy, cx))
                    .into_any_element(),
                AgentsTab::Api => div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .child(agents_sidebar(model, &snapshot, busy, cx))
                    .child(api_detail_panel(model, &snapshot, cx))
                    .into_any_element(),
            },
        })
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
    let query = model
        .cli_model_search_input
        .read(cx)
        .value()
        .trim()
        .to_lowercase();
    let entries = catalog
        .as_ref()
        .map(|catalog| {
            catalog
                .models
                .iter()
                .filter(|entry| model_entry_matches(entry, &query))
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let entries = Arc::new(entries);
    let list_entries = Arc::clone(&entries);
    let target_label = match &picker.target {
        CliModelPickerTarget::Canonical(role) => {
            format!("{} agent", hvc_core::terminals::canonical_agent(*role).name)
        }
        CliModelPickerTarget::FreeForm(_) => "FreeForm agent".into(),
        CliModelPickerTarget::Companion => "Companion".into(),
    };
    let status = if loading {
        "loading".to_string()
    } else if let Some(catalog) = &catalog {
        let state = match catalog.state {
            CliModelCatalogState::Ready => "ready",
            CliModelCatalogState::Stale => "stale — showing the last valid snapshot",
            CliModelCatalogState::Missing => "CLI missing",
            CliModelCatalogState::AuthRequired => "authentication required",
            CliModelCatalogState::UnsupportedVersion => "unsupported CLI version",
            CliModelCatalogState::Failed => "catalog unavailable",
        };
        format!("{state} · {} models", catalog.models.len())
    } else {
        "not loaded".into()
    };
    let diagnostic = error.or_else(|| catalog.as_ref().and_then(|item| item.diagnostic.clone()));
    let manual_enabled = !query.is_empty() && !loading;

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
        .border_color(theme::accent())
        .bg(theme::surface())
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
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(format!(
                            "Choose {} model for {target_label}",
                            picker.backend.label()
                        )))
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
                .child(action_button(
                    "cli-model-close".into(),
                    "Close".into(),
                    true,
                    false,
                    cx,
                    |model, cx| model.close_cli_model_picker(cx),
                )),
        )
        .child(field("SEARCH", model.cli_model_search_input.clone()))
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
                                            let entry = list_entries.get(index)?.clone();
                                            let model_id = entry.model_id.clone();
                                            let source = if model_id.is_some() {
                                                CliModelSelectionSource::Catalog
                                            } else {
                                                CliModelSelectionSource::Default
                                            };
                                            let provider = entry
                                                .provider_id
                                                .clone()
                                                .unwrap_or_else(|| "CLI".into());
                                            let identifier =
                                                entry.model_id.clone().unwrap_or_else(|| {
                                                    "Use the CLI configured default".into()
                                                });
                                            let capabilities = if entry.capabilities.is_empty() {
                                                "capabilities not declared".to_string()
                                            } else {
                                                entry.capabilities.join(", ")
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
                                                        move |model, _, _, cx| {
                                                            model.select_cli_model(
                                                                model_id.clone(),
                                                                source,
                                                                cx,
                                                            )
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
                                                            ),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(theme::text_muted())
                                                            .child(format!(
                                                                "{identifier} · {capabilities}"
                                                            )),
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
        .child(
            div()
                .flex()
                .flex_wrap()
                .flex_shrink_0()
                .items_center()
                .gap_2()
                .child(action_button(
                    "cli-model-manual".into(),
                    "Use typed ID (unverified)".into(),
                    manual_enabled,
                    false,
                    cx,
                    |model, cx| model.select_manual_cli_model(cx),
                ))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child("Manual IDs remain available but are not adapter-verified."),
                ),
        )
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

fn fallback_panel(
    fallback: &CliFallbackCheckpoint,
    cancel_armed: bool,
    busy: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let checkpoint_id = fallback.checkpoint_id.clone();
    let checkpoint_for_retry = checkpoint_id.clone();
    let checkpoint_for_cancel = checkpoint_id.clone();
    let state_version = fallback.state_version;
    let descriptor = hvc_core::terminals::canonical_agent(fallback.agent_role);
    div()
        .id(SharedString::from(format!(
            "cli-fallback:{}",
            fallback.checkpoint_id
        )))
        .mx_4()
        .mt_3()
        .p_3()
        .rounded_lg()
        .border_1()
        .border_color(theme::warning())
        .bg(theme::warning_soft())
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
                        .text_color(theme::warning())
                        .child(format!(
                            "Fallback awaiting decision · {} · {}",
                            descriptor.name, fallback.feature_name
                        )),
                )
                .child(
                    div()
                        .mt_1()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(format!("{} · {}", fallback.error_code, fallback.diagnostic)),
                ),
        )
        .child(action_button(
            SharedString::from(format!("fallback-api:{checkpoint_id}")),
            "Use API".into(),
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
        .child(action_button(
            SharedString::from(format!("fallback-retry:{checkpoint_for_retry}")),
            "Retry CLI".into(),
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
        .child(action_button(
            SharedString::from(format!("fallback-cancel:{checkpoint_for_cancel}")),
            if cancel_armed {
                "Confirm cancellation"
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

fn agents_sidebar(
    model: &AppModel,
    snapshot: &hvc_core::terminals::TerminalsSnapshot,
    busy: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let agents = model.agents.read(cx);
    let selected = agents.selected_agent.clone();
    let fallbacks = agents.fallbacks.clone();
    let cli = agents.tab == AgentsTab::Cli;
    div()
        .id("agents-list")
        .debug_selector(|| "agents-list".into())
        .w(px(220.0))
        .flex_shrink_0()
        .min_h_0()
        .overflow_y_scroll()
        .p_3()
        .flex()
        .flex_col()
        .gap_2()
        .border_r_1()
        .border_color(theme::border())
        .bg(theme::surface())
        .child(
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child("AGENTS"),
        )
        .children(snapshot.agents.iter().map(|agent| {
            let role = agent.config.agent_role;
            let id = AgentIdentifier::Canonical(role);
            agent_list_item(
                format!("agent-select:{}", role.as_str()),
                hvc_core::terminals::canonical_agent(role).name.to_owned(),
                agent.recent_runs.first().map(|run| run.status),
                fallbacks
                    .iter()
                    .filter(|fallback| fallback.agent_role == role)
                    .count(),
                selected.as_ref() == Some(&id),
                id,
                cx,
            )
        }))
        .when(cli, |list| {
            list.child(
                div()
                    .mt_3()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child("AGENTES LIVRES"),
            )
            .child(action_button(
                "free-form-create".into(),
                "+ Novo agente livre".into(),
                !busy && snapshot.free_form_slots.len() < 8,
                false,
                cx,
                |model, cx| model.create_free_form_agent(cx),
            ))
            .children(
                snapshot
                    .free_form_slots
                    .iter()
                    .chain(&snapshot.archived_free_form_slots)
                    .map(|slot| {
                        let id = AgentIdentifier::FreeForm(slot.slot.id.clone());
                        let archived = snapshot
                            .archived_free_form_slots
                            .iter()
                            .any(|item| item.slot.id == slot.slot.id);
                        agent_list_item(
                            format!("agent-select:{}", slot.slot.id),
                            format!(
                                "{}{}",
                                slot.slot.label,
                                if archived { " · arquivado" } else { "" }
                            ),
                            slot.recent_runs.first().map(|run| run.status),
                            0,
                            selected.as_ref() == Some(&id),
                            id,
                            cx,
                        )
                    }),
            )
        })
        .into_any_element()
}

fn agent_list_item(
    element_id: String,
    label: String,
    status: Option<TerminalRunStatus>,
    fallback_count: usize,
    selected: bool,
    agent: AgentIdentifier,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    div()
        .id(SharedString::from(element_id.clone()))
        .debug_selector(move || element_id.clone())
        .p_2()
        .rounded_md()
        .cursor_pointer()
        .bg(if selected {
            theme::accent_soft()
        } else {
            theme::surface()
        })
        .hover(|style| style.bg(theme::surface_raised()))
        .on_click(cx.listener(move |model, _, _, cx| {
            model.agents.update(cx, |agents, _| {
                agents.selected_agent = Some(agent.clone());
                agents.selected_api_role = None;
                agents.active_model_picker = None;
            });
            cx.notify();
        }))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .size_2()
                        .flex_shrink_0()
                        .rounded_full()
                        .bg(status_color(status)),
                )
                .child(div().min_w_0().text_sm().child(label)),
        )
        .when(fallback_count > 0, |item| {
            item.child(
                div()
                    .mt_1()
                    .text_xs()
                    .text_color(theme::warning())
                    .child(format!("{fallback_count} fallback(s) pendente(s)")),
            )
        })
        .into_any_element()
}

fn agent_detail_panel(
    model: &AppModel,
    snapshot: &hvc_core::terminals::TerminalsSnapshot,
    busy: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let agents = model.agents.read(cx);
    let selected = agents.selected_agent.clone();
    let fallbacks = agents.fallbacks.clone();
    let pending_cancel = agents.pending_fallback_cancel.clone();
    div()
        .id("agents-cli-scroll")
        .debug_selector(|| "agents-cli-scroll".into())
        .min_w_0()
        .flex_1()
        .min_h_0()
        .p_4()
        .overflow_y_scroll()
        .children(
            fallbacks
                .iter()
                .filter(|fallback| {
                    selected == Some(AgentIdentifier::Canonical(fallback.agent_role))
                })
                .map(|fallback| {
                    fallback_panel(
                        fallback,
                        pending_cancel.as_deref() == Some(fallback.checkpoint_id.as_str()),
                        busy,
                        cx,
                    )
                }),
        )
        .when(selected.is_none(), |panel| {
            panel.child(empty_state("Selecione ou crie um agente."))
        })
        .child(
            div().flex().flex_wrap().gap_3().children(
                snapshot
                    .agents
                    .iter()
                    .filter(|agent| {
                        selected == Some(AgentIdentifier::Canonical(agent.config.agent_role))
                    })
                    .map(|agent| {
                        let descriptor =
                            hvc_core::terminals::canonical_agent(agent.config.agent_role);
                        let availability = (agent.config.backend
                            != hvc_core::terminals::CliBackend::Custom)
                            .then(|| {
                                snapshot
                                    .discovery
                                    .iter()
                                    .find(|item| item.backend == agent.config.backend)
                            })
                            .flatten();
                        let input = model
                            .agent_prompt_inputs
                            .get(&agent.config.agent_role)
                            .cloned();
                        let model_input = model
                            .agent_model_inputs
                            .get(&agent.config.agent_role)
                            .cloned();
                        let custom_executable_input = model
                            .agent_custom_executable_inputs
                            .get(&agent.config.agent_role)
                            .cloned();
                        let custom_args_input = model
                            .agent_custom_args_inputs
                            .get(&agent.config.agent_role)
                            .cloned();
                        let prompt_ready = input
                            .as_ref()
                            .is_some_and(|input| !input.read(cx).value().trim().is_empty());
                        let active_run = agent.recent_runs.first();
                        let running = active_run.is_some_and(|run| {
                            matches!(
                                run.status,
                                TerminalRunStatus::Queued
                                    | TerminalRunStatus::Starting
                                    | TerminalRunStatus::Running
                            )
                        });
                        let agents_model = model.agents.read(cx);
                        let selected_run = agents_model
                            .selected_run_by_role
                            .get(&agent.config.agent_role)
                            .and_then(|run_id| {
                                agent.recent_runs.iter().find(|run| &run.id == run_id)
                            })
                            .or(active_run);
                        let executable_ready = if agent.config.backend
                            == hvc_core::terminals::CliBackend::Custom
                        {
                            agent
                                .config
                                .custom_executable
                                .as_ref()
                                .is_some_and(|path| !path.trim().is_empty())
                                && agent.config.custom_risk_ack
                        } else {
                            availability
                                .is_some_and(|item| item.state == CliAvailabilityState::Available)
                        };
                        let thinking_available =
                            agent.config.model.as_deref().is_some_and(|model_id| {
                                agents_model
                                    .model_catalogs
                                    .get(&agent.config.backend)
                                    .and_then(|catalog| {
                                        catalog.models.iter().find(|entry| {
                                            entry.model_id.as_deref() == Some(model_id)
                                        })
                                    })
                                    .is_some_and(|entry| !entry.profiles.is_empty())
                            });
                        let run_events = selected_run
                            .and_then(|run| agents_model.events_by_run.get(&run.id))
                            .cloned();
                        let history_clear_armed =
                            agents_model.pending_history_clear == Some(agent.config.agent_role);
                        let pipeline_cancel_armed = active_run
                            .and_then(|run| run.pulsation_id.as_deref())
                            .is_some_and(|pulsation_id| {
                                agents_model.pending_pipeline_cancel.as_deref()
                                    == Some(pulsation_id)
                            });
                        let event_filter = agents_model
                            .event_filters
                            .get(agent.config.agent_role.as_str())
                            .copied()
                            .unwrap_or(TerminalEventFilter::All);
                        let follow_tail = agents_model
                            .follow_tail
                            .get(agent.config.agent_role.as_str())
                            .copied()
                            .unwrap_or(true);
                        agent_card(
                            descriptor,
                            agent,
                            availability,
                            input,
                            model_input,
                            custom_executable_input,
                            custom_args_input,
                            selected_run,
                            run_events,
                            history_clear_armed,
                            pipeline_cancel_armed,
                            event_filter,
                            follow_tail,
                            thinking_available,
                            prompt_ready,
                            executable_ready,
                            running,
                            busy,
                            selected_run
                                .is_some_and(|r| model.notifications.copied.contains_key(&r.id)),
                            cx,
                        )
                    }),
            ),
        )
        .child(
            div().flex().flex_wrap().gap_3().children(
                snapshot
                    .free_form_slots
                    .iter()
                    .filter(|slot| {
                        selected == Some(AgentIdentifier::FreeForm(slot.slot.id.clone()))
                    })
                    .map(|slot| free_form_card(model, snapshot, slot, busy, cx)),
            ),
        )
        .when(
            snapshot
                .archived_free_form_slots
                .iter()
                .any(|slot| selected == Some(AgentIdentifier::FreeForm(slot.slot.id.clone()))),
            |panel| {
                panel
                    .child(
                        div()
                            .mt_5()
                            .mb_3()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Agentes livres arquivados"),
                    )
                    .child(
                        div().flex().flex_wrap().gap_3().children(
                            snapshot
                                .archived_free_form_slots
                                .iter()
                                .filter(|slot| {
                                    selected
                                        == Some(AgentIdentifier::FreeForm(slot.slot.id.clone()))
                                })
                                .map(|slot| archived_free_form_card(model, slot, busy, cx)),
                        ),
                    )
            },
        )
        .into_any_element()
}

fn archived_free_form_card(
    model: &AppModel,
    free_form: &hvc_core::terminals::FreeFormTerminalSnapshot,
    busy: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let slot = &free_form.slot;
    let slot_id = slot.id.clone();
    let delete_armed =
        model.agents.read(cx).pending_free_form_delete.as_deref() == Some(slot.id.as_str());
    div()
        .id(SharedString::from(format!(
            "free-form-archived:{}",
            slot.id
        )))
        .min_w(px(320.0))
        .flex_1()
        .p_3()
        .rounded_lg()
        .border_1()
        .border_color(theme::border())
        .bg(theme::surface())
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .flex_1()
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(slot.label.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(format!(
                            "{} · {} local run(s)",
                            slot.backend.label(),
                            free_form.recent_runs.len()
                        )),
                ),
        )
        .child(action_button(
            SharedString::from(format!("free-form-restore:{}", slot.id)),
            "Restore".into(),
            !busy,
            false,
            cx,
            {
                let slot_id = slot_id.clone();
                move |model, cx| model.restore_free_form_agent(slot_id.clone(), cx)
            },
        ))
        .child(action_button(
            SharedString::from(format!("free-form-archived-delete:{}", slot.id)),
            if delete_armed {
                "Confirm deletion"
            } else {
                "Delete"
            }
            .into(),
            !busy,
            true,
            cx,
            move |model, cx| model.request_delete_free_form_agent(slot_id.clone(), cx),
        ))
        .into_any_element()
}

fn free_form_card(
    model: &AppModel,
    snapshot: &hvc_core::terminals::TerminalsSnapshot,
    free_form: &hvc_core::terminals::FreeFormTerminalSnapshot,
    busy: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let slot = &free_form.slot;
    let slot_id = slot.id.clone();
    let slot_index = snapshot
        .free_form_slots
        .iter()
        .position(|item| item.slot.id == slot.id)
        .unwrap_or(0);
    let availability = (slot.backend != hvc_core::terminals::CliBackend::Custom)
        .then(|| {
            snapshot
                .discovery
                .iter()
                .find(|item| item.backend == slot.backend)
        })
        .flatten();
    let input = model.free_form_prompt_inputs.get(&slot.id).cloned();
    let label_input = model.free_form_label_inputs.get(&slot.id).cloned();
    let model_input = model.free_form_model_inputs.get(&slot.id).cloned();
    let custom_executable_input = model
        .free_form_custom_executable_inputs
        .get(&slot.id)
        .cloned();
    let custom_args_input = model.free_form_custom_args_inputs.get(&slot.id).cloned();
    let prompt_ready = input
        .as_ref()
        .is_some_and(|input| !input.read(cx).value().trim().is_empty());
    let active_run = free_form.recent_runs.first();
    let running = active_run.is_some_and(|run| run.status.is_active());
    let agents = model.agents.read(cx);
    let selected_run = agents
        .selected_run_by_slot
        .get(&slot.id)
        .and_then(|run_id| free_form.recent_runs.iter().find(|run| &run.id == run_id))
        .or(active_run);
    let events = selected_run
        .and_then(|run| agents.events_by_run.get(&run.id))
        .cloned();
    let event_filter = agents
        .event_filters
        .get(&format!("free-form:{}", slot.id))
        .copied()
        .unwrap_or(TerminalEventFilter::All);
    let follow_tail = agents
        .follow_tail
        .get(&format!("free-form:{}", slot.id))
        .copied()
        .unwrap_or(true);
    let delete_armed = agents.pending_free_form_delete.as_deref() == Some(slot.id.as_str());
    let history_clear_armed =
        agents.pending_free_form_history_clear.as_deref() == Some(slot.id.as_str());
    let executable_ready = if slot.backend == hvc_core::terminals::CliBackend::Custom {
        slot.custom_executable
            .as_ref()
            .is_some_and(|path| !path.trim().is_empty())
            && slot.custom_risk_ack
    } else {
        availability.is_some_and(|item| item.state == CliAvailabilityState::Available)
    };
    let thinking_available = slot.model.as_deref().is_some_and(|model_id| {
        agents
            .model_catalogs
            .get(&slot.backend)
            .and_then(|catalog| {
                catalog
                    .models
                    .iter()
                    .find(|entry| entry.model_id.as_deref() == Some(model_id))
            })
            .is_some_and(|entry| !entry.profiles.is_empty())
    });
    let profile_value = slot
        .model_profile
        .clone()
        .unwrap_or_else(|| "CLI default".into());
    let can_resume = selected_run.is_some_and(|run| {
        !run.status.is_active()
            && run.native_session_id.is_some()
            && run.session_fingerprint.is_some()
            && availability.is_some_and(|item| item.capabilities.native_resume)
    }) && slot.session_policy
        == hvc_core::terminals::SessionPolicy::ResumeExplicit;
    let status = active_run
        .map(|run| run_status(run.status))
        .unwrap_or_else(|| {
            if slot.backend == hvc_core::terminals::CliBackend::Custom {
                if executable_ready {
                    "Idle"
                } else {
                    "Setup required"
                }
            } else {
                availability_status(availability)
            }
        });
    div()
        .id(SharedString::from(format!("free-form-card:{}", slot.id)))
        .min_w_0()
        .w_full()
        .flex_1()
        .p_4()
        .rounded_lg()
        .border_1()
        .border_color(theme::accent())
        .bg(theme::surface())
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .size_9()
                        .rounded_md()
                        .bg(theme::accent_soft())
                        .text_color(theme::accent())
                        .font_weight(FontWeight::BOLD)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child("+"),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(slot.label.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::text_muted())
                                .child(format!("FreeForm · {status}")),
                        ),
                )
                .child(action_button(
                    SharedString::from(format!("free-form-up:{}", slot.id)),
                    "↑".into(),
                    !busy && !running && slot_index > 0,
                    false,
                    cx,
                    {
                        let slot_id = slot_id.clone();
                        move |model, cx| model.move_free_form_agent(slot_id.clone(), -1, cx)
                    },
                ))
                .child(action_button(
                    SharedString::from(format!("free-form-down:{}", slot.id)),
                    "↓".into(),
                    !busy && !running && slot_index + 1 < snapshot.free_form_slots.len(),
                    false,
                    cx,
                    {
                        let slot_id = slot_id.clone();
                        move |model, cx| model.move_free_form_agent(slot_id.clone(), 1, cx)
                    },
                ))
                .child(action_button(
                    SharedString::from(format!("free-form-archive:{}", slot.id)),
                    "Arquivar".into(),
                    !busy && !running,
                    false,
                    cx,
                    {
                        let slot_id = slot_id.clone();
                        move |model, cx| model.archive_free_form_agent(slot_id.clone(), cx)
                    },
                ))
                .child(action_button(
                    SharedString::from(format!("free-form-delete:{}", slot.id)),
                    if delete_armed {
                        "Confirm deletion"
                    } else {
                        "Delete"
                    }
                    .into(),
                    !busy && !running,
                    true,
                    cx,
                    {
                        let slot_id = slot_id.clone();
                        move |model, cx| model.request_delete_free_form_agent(slot_id.clone(), cx)
                    },
                )),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .child(crate::select_menu::mode_control(
                    (format!("free-form-mode:{}", slot.id)).to_string(),
                    if slot.execution_mode == AgentExecutionMode::CliProcess {
                        "CLI"
                    } else {
                        "API"
                    },
                    !busy && !running,
                    crate::choices::custom_mode_reason(
                        slot.backend,
                        slot.custom_executable.as_deref(),
                        slot.custom_risk_ack,
                    ),
                    cx,
                ))
                .child(crate::select_menu::control(
                    (format!("free-form-backend:{}", slot.id)).to_string(),
                    "BACKEND",
                    slot.backend.label(),
                    !busy && !running,
                    cx,
                ))
                .child(crate::select_menu::control(
                    (format!("free-form-thinking:{}", slot.id)).to_string(),
                    "PROFILE",
                    profile_value,
                    !busy && !running && thinking_available,
                    cx,
                ))
                .child(crate::select_menu::control(
                    (format!("free-form-session:{}", slot.id)).to_string(),
                    "SESSION",
                    match slot.session_policy {
                        hvc_core::terminals::SessionPolicy::FreshPerRun => "new per run",
                        hvc_core::terminals::SessionPolicy::ResumePerPulsation => "per pulsation",
                        hvc_core::terminals::SessionPolicy::ResumeExplicit => "explicit resume",
                    },
                    !busy && !running,
                    cx,
                )),
        )
        .when_some(label_input, |card, input| card.child(field("NAME", input)))
        .when(slot.backend != CliBackend::Custom, |card| {
            let model_value = match slot.model_source {
                CliModelSelectionSource::Manual => format!(
                    "{} · unverified",
                    slot.model.as_deref().unwrap_or("CLI default")
                ),
                _ => slot.model.as_deref().unwrap_or("CLI default").to_string(),
            };
            card.child(config_button(
                format!("free-form-model:{}", slot.id),
                "MODEL",
                model_value,
                !busy && !running,
                cx,
                {
                    let slot_id = slot_id.clone();
                    move |model, cx| model.open_free_form_model_picker(slot_id.clone(), cx)
                },
            ))
        })
        .when(slot.backend == CliBackend::Custom, |card| {
            card.when_some(model_input, |card, input| {
                card.child(field("MODEL (OPTIONAL)", input))
            })
        })
        .when(
            slot.backend == hvc_core::terminals::CliBackend::Custom,
            |card| {
                card.when_some(custom_executable_input, |card, input| {
                    card.child(field("CUSTOM EXECUTABLE", input))
                })
                .when_some(custom_args_input, |card, input| {
                    card.child(field("EXACT ARGUMENTS (JSON)", input))
                })
                .child(config_button(
                    format!("free-form-risk:{}", slot.id),
                    "EXECUTABLE RISK",
                    if slot.custom_risk_ack {
                        "confirmed"
                    } else {
                        "pending"
                    },
                    !busy && !running,
                    cx,
                    {
                        let slot_id = slot_id.clone();
                        move |model, cx| model.toggle_free_form_risk(slot_id.clone(), cx)
                    },
                ))
            },
        )
        .child(action_button(
            SharedString::from(format!("free-form-save:{}", slot.id)),
            "Save configuration".into(),
            !busy && !running,
            false,
            cx,
            {
                let slot_id = slot_id.clone();
                move |model, cx| model.save_free_form_text_config(slot_id.clone(), cx)
            },
        ))
        .when_some(selected_run, |card, run| {
            let index = free_form
                .recent_runs
                .iter()
                .position(|item| item.id == run.id)
                .unwrap_or(0);
            card.child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w_full()
                            .text_xs()
                            .text_color(theme::text_muted())
                            .child(run_metadata(run)),
                    )
                    .child(action_button(
                        SharedString::from(format!("free-form-newer:{}", slot.id)),
                        "Mais nova".into(),
                        !busy && index > 0,
                        false,
                        cx,
                        {
                            let slot_id = slot_id.clone();
                            move |model, cx| model.select_free_form_history(slot_id.clone(), -1, cx)
                        },
                    ))
                    .child(action_button(
                        SharedString::from(format!("free-form-follow:{}", slot.id)),
                        if follow_tail {
                            "Follow ativo"
                        } else {
                            "Follow pausado"
                        }
                        .into(),
                        true,
                        false,
                        cx,
                        {
                            let key = format!("free-form:{}", slot.id);
                            move |model, cx| model.toggle_agent_follow_tail(key.clone(), cx)
                        },
                    ))
                    .child(action_button(
                        SharedString::from(format!("free-form-older:{}", slot.id)),
                        "Mais antiga".into(),
                        !busy && index + 1 < free_form.recent_runs.len(),
                        false,
                        cx,
                        {
                            let slot_id = slot_id.clone();
                            move |model, cx| model.select_free_form_history(slot_id.clone(), 1, cx)
                        },
                    ))
                    .child(crate::select_menu::control(
                        (SharedString::from(format!("free-form-filter:{}", slot.id))).to_string(),
                        "Filter",
                        event_filter.label(),
                        true,
                        cx,
                    ))
                    .child(action_button(
                        SharedString::from(format!("free-form-copy:{}", slot.id)),
                        if model.notifications.copied.contains_key(&run.id) {
                            "Copied"
                        } else {
                            "Copy"
                        }
                        .into(),
                        !busy,
                        false,
                        cx,
                        {
                            let run_id = run.id.clone();
                            move |model, cx| model.copy_agent_history(run_id.clone(), cx)
                        },
                    ))
                    .child(action_button(
                        SharedString::from(format!("free-form-export:{}", slot.id)),
                        "Exportar".into(),
                        !busy,
                        false,
                        cx,
                        {
                            let run_id = run.id.clone();
                            move |model, cx| model.export_agent_history(run_id.clone(), cx)
                        },
                    ))
                    .child(action_button(
                        SharedString::from(format!("free-form-clear:{}", slot.id)),
                        if history_clear_armed {
                            "Confirm clear"
                        } else {
                            "Clear history"
                        }
                        .into(),
                        !busy && !running,
                        true,
                        cx,
                        {
                            let slot_id = slot_id.clone();
                            move |model, cx| {
                                model.request_clear_free_form_history(slot_id.clone(), cx)
                            }
                        },
                    )),
            )
        })
        .child(activity_panel_for_key(
            events.as_ref(),
            format!("free-form:{}", slot.id),
            event_filter,
            follow_tail,
            cx,
        ))
        .when_some(input, |card, input| {
            let selected_run_id = selected_run.map(|run| run.id.clone());
            card.child(input).child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .when(running, |row| {
                        let run_id = active_run.map(|run| run.id.clone()).unwrap_or_default();
                        row.child(action_button(
                            SharedString::from(format!("free-form-stop:{}", slot.id)),
                            "Stop".into(),
                            !busy,
                            true,
                            cx,
                            move |model, cx| {
                                model.request_cancel_agent_run(run_id.clone(), None, cx)
                            },
                        ))
                    })
                    .when(!running, |row| {
                        row.child(action_button(
                            SharedString::from(format!("free-form-start:{}", slot.id)),
                            "New session".into(),
                            !busy
                                && prompt_ready
                                && executable_ready
                                && slot.execution_mode == AgentExecutionMode::CliProcess,
                            false,
                            cx,
                            {
                                let slot_id = slot_id.clone();
                                move |model, cx| model.start_free_form_agent(slot_id.clone(), cx)
                            },
                        ))
                    })
                    .when_some(selected_run_id, |row, source_run_id| {
                        row.child(action_button(
                            SharedString::from(format!("free-form-resume:{}", slot.id)),
                            "Resume session".into(),
                            !busy && !running && prompt_ready && can_resume,
                            false,
                            cx,
                            {
                                let slot_id = slot_id.clone();
                                move |model, cx| {
                                    model.resume_agent_run(
                                        source_run_id.clone(),
                                        slot_id.clone(),
                                        None,
                                        cx,
                                    )
                                }
                            },
                        ))
                    }),
            )
        })
        .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn agent_card(
    descriptor: &CanonicalAgentDescriptor,
    agent: &hvc_core::terminals::AgentTerminalSnapshot,
    availability: Option<&hvc_core::terminals::CliAvailability>,
    input: Option<gpui::Entity<crate::text_input::NativeTextInput>>,
    model_input: Option<gpui::Entity<crate::text_input::NativeTextInput>>,
    custom_executable_input: Option<gpui::Entity<crate::text_input::NativeTextInput>>,
    custom_args_input: Option<gpui::Entity<crate::text_input::NativeTextInput>>,
    selected_run: Option<&hvc_core::terminals::TerminalRun>,
    events: Option<Vec<hvc_core::terminals::TerminalEvent>>,
    history_clear_armed: bool,
    pipeline_cancel_armed: bool,
    event_filter: TerminalEventFilter,
    follow_tail: bool,
    thinking_available: bool,
    prompt_ready: bool,
    executable_ready: bool,
    running: bool,
    busy: bool,
    history_copied: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let role = agent.config.agent_role;
    let color = theme::agent_color(role);
    let mode = match agent.config.execution_mode {
        AgentExecutionMode::ApiDirect => "API",
        AgentExecutionMode::CliProcess => "CLI",
    };
    let status = agent
        .recent_runs
        .first()
        .map(|run| run_status(run.status))
        .unwrap_or_else(|| {
            if agent.config.backend == hvc_core::terminals::CliBackend::Custom {
                if executable_ready {
                    "Idle"
                } else {
                    "Setup required"
                }
            } else {
                availability_status(availability)
            }
        });
    let can_start = !busy
        && !running
        && prompt_ready
        && executable_ready
        && agent.config.execution_mode == AgentExecutionMode::CliProcess;
    let can_resume = selected_run.is_some_and(|run| {
        !run.status.is_active()
            && run.native_session_id.is_some()
            && run.session_fingerprint.is_some()
            && availability.is_some_and(|item| item.capabilities.native_resume)
    });
    let cli_identity = availability.map(|item| {
        format!(
            "Version {} · {}",
            item.version.as_deref().unwrap_or("unconfirmed"),
            item.executable_path
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_else(|| "executable not found".into())
        )
    });
    let install_guidance = availability
        .filter(|item| item.state != CliAvailabilityState::Available)
        .map(|item| {
            (
                item.install_command.to_string(),
                item.install_url.to_string(),
            )
        });
    let profile_value = agent
        .config
        .model_profile
        .clone()
        .unwrap_or_else(|| "CLI default".into());
    div()
        .id(SharedString::from(format!("agent-card:{}", role.as_str())))
        .debug_selector(move || format!("agent-card:{}", role.as_str()))
        .min_w_0()
        .w_full()
        .flex_1()
        .p_4()
        .rounded_lg()
        .border_1()
        .border_color(color)
        .bg(theme::surface())
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .size_9()
                        .rounded_md()
                        .bg(theme::agent_soft_color(role))
                        .text_color(color)
                        .font_weight(FontWeight::BOLD)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(descriptor.glyph),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme::text())
                                .child(descriptor.name),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::text_muted())
                                .child(descriptor.stage),
                        ),
                )
                .child(chip(mode, color))
                .child(chip(
                    status,
                    status_color(agent.recent_runs.first().map(|run| run.status)),
                )),
        )
        .child(
            div()
                .flex()
                .gap_2()
                .child(crate::select_menu::mode_control(
                    (format!("agent-mode:{}", role.as_str())).to_string(),
                    mode,
                    !busy && !running,
                    crate::choices::custom_mode_reason(
                        agent.config.backend,
                        agent.config.custom_executable.as_deref(),
                        agent.config.custom_risk_ack,
                    ),
                    cx,
                ))
                .child(crate::select_menu::control(
                    (format!("agent-backend:{}", role.as_str())).to_string(),
                    "BACKEND",
                    agent.config.backend.label(),
                    !busy && !running,
                    cx,
                ))
                .child(crate::select_menu::control(
                    (format!("agent-thinking:{}", role.as_str())).to_string(),
                    "PROFILE",
                    profile_value,
                    !busy && !running && thinking_available,
                    cx,
                )),
        )
        .when(!thinking_available, |card| {
            card.child(
                div()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child("This adapter does not declare stable thinking control."),
            )
        })
        .when(agent.config.backend != CliBackend::Custom, |card| {
            let model_value = match agent.config.model_source {
                CliModelSelectionSource::Manual => format!(
                    "{} · unverified",
                    agent.config.model.as_deref().unwrap_or("CLI default")
                ),
                _ => agent
                    .config
                    .model
                    .as_deref()
                    .unwrap_or("CLI default")
                    .to_string(),
            };
            card.child(config_button(
                format!("agent-model:{}", role.as_str()),
                "MODEL",
                model_value,
                !busy && !running,
                cx,
                move |model, cx| model.open_agent_model_picker(role, cx),
            ))
        })
        .when(agent.config.backend == CliBackend::Custom, |card| {
            card.when_some(model_input, |card, model_input| {
                card.child(field("MODEL (OPTIONAL)", model_input))
            })
        })
        .when(
            agent.config.backend == hvc_core::terminals::CliBackend::Custom,
            |card| {
                card.when_some(custom_executable_input, |card, input| {
                    card.child(field("CUSTOM EXECUTABLE", input))
                })
                .when_some(custom_args_input, |card, input| {
                    card.child(field("EXACT ARGUMENTS (JSON)", input))
                })
                .child(config_button(
                    format!("agent-custom-risk:{}", role.as_str()),
                    "EXECUTABLE RISK",
                    if agent.config.custom_risk_ack {
                        "confirmed"
                    } else {
                        "pending"
                    },
                    !busy && !running,
                    cx,
                    move |model, cx| model.toggle_agent_custom_risk_ack(role, cx),
                ))
                .child(action_button(
                    SharedString::from(format!("agent-save-fields:{}", role.as_str())),
                    "Save model/configuration".into(),
                    !busy && !running,
                    false,
                    cx,
                    move |model, cx| model.save_agent_text_config(role, cx),
                ))
            },
        )
        .when_some(
            availability.and_then(|availability| {
                (availability.state != CliAvailabilityState::Available).then(|| {
                    availability.diagnostic.clone().unwrap_or_else(|| {
                        format!(
                            "{} not found. {}",
                            availability.backend.label(),
                            availability.install_command
                        )
                    })
                })
            }),
            |card, diagnostic| {
                card.child(
                    div()
                        .p_2()
                        .rounded_md()
                        .bg(theme::warning_soft())
                        .text_xs()
                        .text_color(theme::warning())
                        .child(diagnostic),
                )
            },
        )
        .when_some(cli_identity, |card, identity| {
            card.child(
                div()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(identity),
            )
        })
        .when_some(install_guidance, |card, (command, url)| {
            let command_for_copy = command.clone();
            let url_for_copy = url.clone();
            card.child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(action_button(
                        SharedString::from(format!("agent-install-copy:{}", role.as_str())),
                        "Copy install command".into(),
                        true,
                        false,
                        cx,
                        move |_, cx| {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                command_for_copy.clone(),
                            ));
                        },
                    ))
                    .child(action_button(
                        SharedString::from(format!("agent-install-link:{}", role.as_str())),
                        "Copy official link".into(),
                        !url.is_empty(),
                        false,
                        cx,
                        move |_, cx| {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                url_for_copy.clone(),
                            ));
                        },
                    )),
            )
        })
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::text_muted())
                .child("ACTIVITY AND OUTPUT"),
        )
        .when_some(selected_run, |card, selected_run| {
            let index = agent
                .recent_runs
                .iter()
                .position(|run| run.id == selected_run.id)
                .unwrap_or(0);
            card.child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w_full()
                            .text_xs()
                            .text_color(theme::text_muted())
                            .child(format!(
                                "Run {}/{} · {}",
                                index + 1,
                                agent.recent_runs.len(),
                                run_metadata(selected_run)
                            )),
                    )
                    .child(action_button(
                        SharedString::from(format!("agent-history-newer:{}", role.as_str())),
                        "Newer".into(),
                        !busy && index > 0,
                        false,
                        cx,
                        move |model, cx| model.select_agent_history(role, -1, cx),
                    ))
                    .child(action_button(
                        SharedString::from(format!("agent-history-older:{}", role.as_str())),
                        "Older".into(),
                        !busy && index + 1 < agent.recent_runs.len(),
                        false,
                        cx,
                        move |model, cx| model.select_agent_history(role, 1, cx),
                    ))
                    .child(action_button(
                        SharedString::from(format!("agent-history-clear:{}", role.as_str())),
                        if history_clear_armed {
                            "Confirm clear"
                        } else {
                            "Clear history"
                        }
                        .into(),
                        !busy && !running,
                        true,
                        cx,
                        move |model, cx| model.request_clear_agent_history(role, cx),
                    ))
                    .child(crate::select_menu::control(
                        (SharedString::from(format!("agent-filter:{}", role.as_str()))).to_string(),
                        "Filter",
                        event_filter.label(),
                        true,
                        cx,
                    ))
                    .child(action_button(
                        SharedString::from(format!("agent-follow:{}", role.as_str())),
                        if follow_tail {
                            "Follow ativo"
                        } else {
                            "Follow pausado"
                        }
                        .into(),
                        true,
                        false,
                        cx,
                        move |model, cx| {
                            model.toggle_agent_follow_tail(role.as_str().to_string(), cx)
                        },
                    ))
                    .child(action_button(
                        SharedString::from(format!("agent-copy:{}", role.as_str())),
                        if history_copied { "Copied" } else { "Copy" }.into(),
                        !busy,
                        false,
                        cx,
                        {
                            let run_id = selected_run.id.clone();
                            move |model, cx| model.copy_agent_history(run_id.clone(), cx)
                        },
                    ))
                    .child(action_button(
                        SharedString::from(format!("agent-export:{}", role.as_str())),
                        "Exportar".into(),
                        !busy,
                        false,
                        cx,
                        {
                            let run_id = selected_run.id.clone();
                            move |model, cx| model.export_agent_history(run_id.clone(), cx)
                        },
                    )),
            )
        })
        .child(activity_panel(
            events.as_ref(),
            role,
            event_filter,
            follow_tail,
            cx,
        ))
        .when_some(input, |card, input| {
            let resume_run_id = selected_run.map(|run| run.id.clone());
            card.child(input).child(
                div()
                    .flex()
                    .gap_2()
                    .when(running, |row| {
                        let run_id = agent
                            .recent_runs
                            .first()
                            .map(|run| run.id.clone())
                            .unwrap_or_default();
                        let pulsation_id = agent
                            .recent_runs
                            .first()
                            .and_then(|run| run.pulsation_id.clone());
                        row.child(action_button(
                            SharedString::from(format!("agent-stop:{}", role.as_str())),
                            if pulsation_id.is_some() && pipeline_cancel_armed {
                                "Confirmar Stop"
                            } else {
                                "Stop"
                            }
                            .into(),
                            !busy,
                            true,
                            cx,
                            move |model, cx| {
                                model.request_cancel_agent_run(
                                    run_id.clone(),
                                    pulsation_id.clone(),
                                    cx,
                                )
                            },
                        ))
                    })
                    .when(!running, |row| {
                        row.child(action_button(
                            SharedString::from(format!("agent-start:{}", role.as_str())),
                            "Start".into(),
                            can_start,
                            false,
                            cx,
                            move |model, cx| model.start_manual_agent(role, cx),
                        ))
                    })
                    .when_some(resume_run_id, |row, source_run_id| {
                        row.child(action_button(
                            SharedString::from(format!("agent-resume:{}", role.as_str())),
                            "Resume session".into(),
                            !busy && !running && prompt_ready && can_resume,
                            false,
                            cx,
                            move |model, cx| {
                                model.resume_agent_run(
                                    source_run_id.clone(),
                                    format!("canonical:{}", role.as_str()),
                                    Some(role),
                                    cx,
                                )
                            },
                        ))
                    })
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(theme::text_muted())
                            .child(
                                if agent.config.execution_mode == AgentExecutionMode::ApiDirect {
                                    "Manual execution blocked while agent uses API."
                                } else if running {
                                    "Input blocked during execution."
                                } else {
                                    "Prompt will be sent via stdin; this is not a PTY terminal."
                                },
                            ),
                    ),
            )
        })
        .into_any_element()
}

fn field(
    label: &'static str,
    input: gpui::Entity<crate::text_input::NativeTextInput>,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::text_muted())
                .child(label),
        )
        .child(input)
}

fn api_detail_panel(
    model: &AppModel,
    snapshot: &hvc_core::terminals::TerminalsSnapshot,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let settings = model.settings.read(cx).snapshot.clone();
    let agents = model.agents.read(cx);
    let api_activity = agents.api_activity.clone();
    let viewer_enabled = agents.viewer_enabled;
    let selected_agent = agents.selected_agent.clone();
    let selected_api_role = agents.selected_api_role;
    let settings_busy = model.settings.read(cx).busy;
    div()
        .id("agents-api-scroll")
        .flex_1()
        .min_h_0()
        .p_4()
        .overflow_y_scroll()
        .child(
            div()
                .mb_3()
                .p_3()
                .rounded_lg()
                .border_1()
                .border_color(theme::border())
                .bg(theme::surface())
                .flex()
                .flex_wrap()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .flex_1()
                        .child("Real-time activity and output viewer")
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::text_muted())
                                .child("Displays visible events and output only; internal reasoning is not captured."),
                        ),
                )
                .child(action_button(
                    "agents-api-viewer".into(),
                    if viewer_enabled {
                        "Viewer enabled"
                    } else {
                        "Viewer disabled"
                    }
                    .into(),
                    true,
                    false,
                    cx,
                    |model, cx| model.toggle_agents_viewer(cx),
                ))
                .child(action_button(
                    "agents-api-logs".into(),
                    "Open history in Logs".into(),
                    true,
                    false,
                    cx,
                    |model, cx| model.select_view(crate::app_model::AppView::Logs, cx),
                )),
        )
        .child(
            div()
                .p_4()
                .rounded_lg()
                .border_1()
                .border_color(theme::border())
                .bg(theme::surface())
                .child(
                    div()
                        .mb_3()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Per-agent routing"),
                )
                .children(snapshot.agents.iter().filter(|agent| selected_agent == Some(AgentIdentifier::Canonical(agent.config.agent_role))).map(|agent| {
                    let role = agent.config.agent_role;
                    let descriptor = hvc_core::terminals::canonical_agent(agent.config.agent_role);
                    let binding = settings.as_ref().and_then(|settings| {
                        settings.bindings.iter().find(|binding| {
                            binding.project_id == snapshot.project_id
                                && binding.agent_role == agent.config.agent_role.as_str()
                                && binding.stage.eq_ignore_ascii_case(descriptor.stage)
                        })
                    });
                    let route = binding.and_then(|binding| {
                        settings.as_ref()?.routes.iter().find(|route| route.id == binding.route_id)
                    });
                    let primary = route.and_then(|route| route.targets.iter().filter(|target| target.enabled).min_by_key(|target| target.position));
                    let connection = primary.and_then(|target| {
                        settings.as_ref()?.connections.iter().find(|connection| connection.id == target.connection_id)
                    });
                    let fallback_count = route.map(|route| route.targets.iter().filter(|target| target.enabled).count().saturating_sub(1)).unwrap_or(0);
                    let route_label: SharedString = route.map(|route|route.name.clone()).unwrap_or_else(||"No specific route".into()).into();
                    let target_label: SharedString = connection.map(|connection|format!("{} · {}",connection.display_name,primary.map(|target|target.model_id.as_str()).unwrap_or(&connection.default_model))).unwrap_or_else(||"Fallback to legacy runtime".into()).into();
                    let provider_label: SharedString = connection.map(|connection|connection.provider_id.clone()).unwrap_or_else(||"—".into()).into();
                    let model_label: SharedString = primary.map(|target|target.model_id.clone()).or_else(||connection.map(|connection|connection.default_model.clone())).unwrap_or_else(||"—".into()).into();
                    let activity_label: SharedString = api_activity
                        .get(&agent.config.agent_role)
                        .and_then(|items|items.last())
                        .map(|line| {
                            let mut value=line.chars().take(80).collect::<String>();
                            if line.chars().count()>80{value.push('…');}
                            value
                        })
                        .unwrap_or_else(||"No API activity in this session".into())
                        .into();
                    div()
                        .id(SharedString::from(format!("api-agent:{}",agent.config.agent_role.as_str())))
                        .debug_selector(move || format!("api-agent:{}", role.as_str()))
                        .py_3()
                        .border_t_1()
                        .border_color(theme::border())
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_3()
                        .child(div().w(px(28.0)).text_color(theme::agent_color(agent.config.agent_role)).font_weight(FontWeight::BOLD).child(descriptor.glyph))
                        .child(div().w(px(110.0)).child(descriptor.name))
                        .child(chip(if agent.config.execution_mode==AgentExecutionMode::ApiDirect{"API"}else{"CLI"},theme::agent_color(agent.config.agent_role)))
                        .child(div().w(px(160.0)).text_sm().text_color(theme::text_muted()).child(format!("Route: {route_label}")))
                        .child(div().w(px(180.0)).text_sm().child(format!("Connection: {target_label}")))
                        .child(div().w(px(100.0)).text_xs().text_color(theme::text_muted()).child(format!("Provider: {provider_label}")))
                        .child(div().w(px(140.0)).text_xs().text_color(theme::text_muted()).child(format!("Model: {model_label}")))
                        .child(chip(if connection.is_some_and(|connection|connection.has_api_key){"Key OK"}else{"Key pending"},if connection.is_some_and(|connection|connection.has_api_key){theme::accent()}else{theme::warning()}))
                        .child(div().w(px(80.0)).text_xs().text_color(theme::text_muted()).child(format!("+{fallback_count} fallback")))
                        .child(div().flex_1().min_w(px(180.0)).text_xs().text_color(theme::text_muted()).child(activity_label))
                        .child(action_button(
                            SharedString::from(format!("api-configure:{}",role.as_str())),
                            "Configure".into(),
                            !settings_busy,
                            false,
                            cx,
                            move |model,cx| model.configure_agent_api(role,cx),
                        ))
                })),
        )
        .when_some(selected_api_role, |panel, role| {
            panel.child(api_inline_editor(model, role, cx))
        })
        .into_any_element()
}

fn api_inline_editor(
    model: &AppModel,
    role: AgentRole,
    cx: &mut Context<AppModel>,
) -> impl IntoElement {
    let descriptor = hvc_core::terminals::canonical_agent(role);
    let settings = model.settings.read(cx);
    let busy = settings.busy;
    let selected_route = settings.route().cloned();
    let selected_connection = settings.connection().cloned();
    let provider_id = settings.connection_provider_id.clone();
    let route_name = model.settings_route_name_input.clone();
    let connection_name = model.settings_connection_name_input.clone();
    let connection_model = model.settings_connection_model_input.clone();
    let connection_endpoint = model.settings_connection_endpoint_input.clone();
    let connection_secret = model.settings_connection_secret_input.clone();
    let activity = model
        .agents
        .read(cx)
        .api_activity
        .get(&role)
        .cloned()
        .unwrap_or_default();
    div()
        .mt_3()
        .p_4()
        .rounded_lg()
        .border_1()
        .border_color(theme::agent_color(role))
        .bg(theme::surface())
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(format!("API Configuration · {}", descriptor.name)),
                )
                .child(crate::select_menu::control(
                    (SharedString::from(format!("api-cycle-route:{}", role.as_str()))).to_string(),
                    "Route",
                    format!(
                        "Route: {}",
                        selected_route
                            .as_ref()
                            .map(|route| route.name.as_str())
                            .unwrap_or("none")
                    ),
                    !busy,
                    cx,
                ))
                .child(crate::select_menu::control(
                    (SharedString::from(format!("api-cycle-connection:{}", role.as_str())))
                        .to_string(),
                    "Connection",
                    format!(
                        "Connection: {}",
                        selected_connection
                            .as_ref()
                            .map(|connection| connection.display_name.as_str())
                            .unwrap_or("none")
                    ),
                    !busy,
                    cx,
                ))
                .child(action_button(
                    SharedString::from(format!("api-advanced:{}", role.as_str())),
                    "Advanced configuration".into(),
                    !busy,
                    false,
                    cx,
                    move |model, cx| model.open_advanced_api_settings(role, cx),
                )),
        )
        .child(
            div()
                .grid()
                .grid_cols(2)
                .gap_2()
                .child(field("CONNECTION NAME", connection_name))
                .child(crate::select_menu::control(
                    (format!("api-provider:{}", role.as_str())).to_string(),
                    "PROVIDER",
                    provider_id,
                    !busy,
                    cx,
                ))
                .child(field("MODEL", connection_model))
                .child(field("ENDPOINT (WHEN ALLOWED)", connection_endpoint))
                .child(field("API KEY · KEYRING", connection_secret))
                .child(field("ROUTE NAME", route_name)),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .child(action_button(
                    SharedString::from(format!("api-new-connection:{}", role.as_str())),
                    "New connection".into(),
                    !busy,
                    false,
                    cx,
                    |model, cx| model.new_settings_connection(cx),
                ))
                .child(action_button(
                    SharedString::from(format!("api-save-connection:{}", role.as_str())),
                    "Save connection/key".into(),
                    !busy,
                    false,
                    cx,
                    |model, cx| model.save_settings_connection(cx),
                ))
                .child(action_button(
                    SharedString::from(format!("api-test-connection:{}", role.as_str())),
                    "Test connection".into(),
                    !busy && selected_connection.is_some(),
                    false,
                    cx,
                    |model, cx| model.test_settings_connection(cx),
                ))
                .child(action_button(
                    SharedString::from(format!("api-new-route:{}", role.as_str())),
                    "New route".into(),
                    !busy,
                    false,
                    cx,
                    |model, cx| model.new_settings_route(cx),
                ))
                .child(action_button(
                    SharedString::from(format!("api-save-route:{}", role.as_str())),
                    "Save route/target".into(),
                    !busy && selected_connection.is_some(),
                    false,
                    cx,
                    |model, cx| model.save_settings_route(cx),
                ))
                .child(action_button(
                    SharedString::from(format!("api-save-binding:{}", role.as_str())),
                    "Bind to agent".into(),
                    !busy && selected_route.is_some(),
                    false,
                    cx,
                    move |model, cx| model.save_agent_api_binding(role, cx),
                )),
        )
        .child(
            div().text_xs().text_color(theme::text_muted()).child(
                "Displays visible events and output only; internal reasoning is not captured.",
            ),
        )
        .child(
            div()
                .id(SharedString::from(format!(
                    "api-activity:{}",
                    role.as_str()
                )))
                .max_h(px(180.0))
                .overflow_y_scroll()
                .p_2()
                .rounded_md()
                .bg(theme::canvas())
                .when(activity.is_empty(), |viewer| {
                    viewer.child(div().text_xs().text_color(theme::text_muted()).child(
                        "No API activity in this session. Persisted history remains in Logs.",
                    ))
                })
                .children(activity.into_iter().map(|line| {
                    div()
                        .py_1()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(line)
                })),
        )
}

fn activity_panel(
    events: Option<&Vec<hvc_core::terminals::TerminalEvent>>,
    role: AgentRole,
    filter: TerminalEventFilter,
    follow_tail: bool,
    cx: &mut Context<AppModel>,
) -> impl IntoElement {
    activity_panel_for_key(events, role.as_str().to_string(), filter, follow_tail, cx)
}

fn activity_panel_for_key(
    events: Option<&Vec<hvc_core::terminals::TerminalEvent>>,
    key: String,
    filter: TerminalEventFilter,
    follow_tail: bool,
    cx: &mut Context<AppModel>,
) -> impl IntoElement {
    let mut events = events
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|event| filter.accepts(event.kind))
        .collect::<Vec<_>>();
    if follow_tail {
        events.reverse();
    }
    let visible_count = events.len().min(100);
    div()
        .id(SharedString::from(format!("agent-activity:{}", key)))
        .h(px(250.0))
        .p_2()
        .rounded_md()
        .bg(theme::canvas())
        .border_1()
        .border_color(theme::border())
        .overflow_y_scroll()
        .child(
            div()
                .pb_1()
                .text_xs()
                .text_color(theme::text_muted())
                .child(format!(
                    "{} · {visible_count} visible event(s){}",
                    filter.label(),
                    if follow_tail {
                        " · new events on top"
                    } else {
                        " · position preserved"
                    }
                )),
        )
        .when(events.is_empty(), |panel| {
            panel.child(
                div()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child("No activity recorded for the selected run."),
            )
        })
        .children(events.into_iter().take(100).map(|event| {
            let prefix = match event.kind {
                TerminalEventKind::System => "system",
                TerminalEventKind::Message => "message",
                TerminalEventKind::ToolUse => "tool",
                TerminalEventKind::ToolResult => "result",
                TerminalEventKind::Stdout => "stdout",
                TerminalEventKind::Stderr => "stderr",
                TerminalEventKind::Warning => "warning",
                TerminalEventKind::Error => "error",
                TerminalEventKind::Completed => "done",
            };
            let content = event.content.clone();
            let event_id = event.id.clone();
            let time_label = crate::time::format_timestamp_time_only(&event.created_at);
            let display_text = if time_label.is_empty() {
                format!("{prefix} · {}", event.content)
            } else {
                format!("{time_label} · {prefix} · {}", event.content)
            };
            div()
                .id(SharedString::from(format!("terminal-event:{event_id}")))
                .py_1()
                .text_xs()
                .cursor_pointer()
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(content.clone()));
                }))
                .text_color(
                    if matches!(
                        event.kind,
                        TerminalEventKind::Error | TerminalEventKind::Stderr
                    ) {
                        theme::danger()
                    } else {
                        theme::text_muted()
                    },
                )
                .child(display_text)
        }))
}

fn run_metadata(run: &hvc_core::terminals::TerminalRun) -> String {
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
    format!(
        "{} · {} · {} · exit {} · version {} · model {} · session {}",
        run_status(run.status),
        match run.origin {
            hvc_core::terminals::ExecutionOrigin::Pipeline => "pipeline",
            hvc_core::terminals::ExecutionOrigin::Learning => "learning",
            hvc_core::terminals::ExecutionOrigin::Manual => "manual",
            hvc_core::terminals::ExecutionOrigin::Feature => "feature",
        },
        duration,
        run.exit_code
            .map_or_else(|| "—".into(), |code| code.to_string()),
        run.detected_cli_version.as_deref().unwrap_or("—"),
        run.model.as_deref().unwrap_or("default"),
        session,
    )
}

fn tab_button(
    id: &'static str,
    label: &'static str,
    selected: bool,
    cx: &mut Context<AppModel>,
    handler: impl Fn(&mut AppModel, &mut Context<AppModel>) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .px_3()
        .py_2()
        .rounded_md()
        .cursor_pointer()
        .bg(if selected {
            theme::accent_soft()
        } else {
            theme::surface()
        })
        .text_color(if selected {
            theme::accent()
        } else {
            theme::text_muted()
        })
        .on_click(cx.listener(move |model, _, _, cx| handler(model, cx)))
        .child(label)
}

fn config_button(
    id: String,
    label: &'static str,
    value: impl Into<SharedString>,
    enabled: bool,
    cx: &mut Context<AppModel>,
    handler: impl Fn(&mut AppModel, &mut Context<AppModel>) + 'static,
) -> impl IntoElement {
    let value = value.into();
    div()
        .id(SharedString::from(id))
        .flex_1()
        .p_2()
        .rounded_md()
        .border_1()
        .border_color(theme::border())
        .bg(theme::surface_raised())
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .hover(|style| style.border_color(theme::accent()))
                .on_click(cx.listener(move |model, _, _, cx| handler(model, cx)))
        })
        .child(div().text_xs().text_color(theme::text_muted()).child(label))
        .child(div().text_sm().child(value))
}

fn action_button(
    id: SharedString,
    label: SharedString,
    enabled: bool,
    danger: bool,
    cx: &mut Context<AppModel>,
    handler: impl Fn(&mut AppModel, &mut Context<AppModel>) + 'static,
) -> impl IntoElement {
    let foreground = if enabled {
        theme::canvas()
    } else {
        theme::text_muted()
    };
    let is_copy = id.starts_with("agent-copy:") || id.starts_with("free-form-copy:");
    div()
        .id(id.clone())
        .when(is_copy, |d| d.w(px(84.0)).flex_shrink_0())
        .debug_selector(move || id.to_string())
        .px_3()
        .py_2()
        .rounded_md()
        .bg(if enabled {
            if danger {
                theme::danger()
            } else {
                theme::accent()
            }
        } else {
            theme::surface_raised()
        })
        .text_color(foreground)
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .on_click(cx.listener(move |model, _, _, cx| handler(model, cx)))
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
    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .text_color(theme::text_muted())
        .child(message)
        .into_any_element()
}

fn availability_status(
    availability: Option<&hvc_core::terminals::CliAvailability>,
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

fn run_status(status: TerminalRunStatus) -> &'static str {
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
}
