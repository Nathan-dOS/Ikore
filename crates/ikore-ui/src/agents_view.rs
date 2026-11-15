use std::sync::Arc;

use gpui::{
    AnyElement, Context, FontWeight, IntoElement, Rgba, SharedString, deferred, div, prelude::*,
    px, uniform_list,
};
use ikore_core::llm::AgentRole;
use ikore_core::persistence_service::{
    CLI_VALIDATION_FAILED, CLI_VERSION_UNVALIDATED, CliFallbackCheckpoint, CliFallbackDecision,
};
use ikore_core::terminals::{
    AgentExecutionMode, CanonicalAgentDescriptor, CliAvailabilityState, CliBackend,
    CliModelCatalogEntry, CliModelCatalogState, CliModelSelectionSource, TerminalRunStatus,
};

use crate::agent_console::view::{ConsoleSurface, console};
use crate::agents::{
    AgentIdentifier, ApiActivityFilter, CliModelPickerState, CliModelPickerTarget,
};
use crate::app_model::AppModel;
use crate::icons::Icon;
use crate::pipeline_status::PipelineStatus;
use crate::theme;

/// The six-stage stepper for the active pulsation. Sized to its content so the header's two
/// `flex_1` spacers keep it centred; the stage · agent · state readout lives in the flow bar.
fn agents_pipeline_header(
    model: &AppModel,
    snapshot: Option<&ikore_core::terminals::TerminalsSnapshot>,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let status = model
        .pulsations
        .read(cx)
        .active()
        .map(|active| PipelineStatus::derive(active, snapshot));
    let Some(status) = status else {
        return div()
            .id("agents-pipeline")
            .debug_selector(|| "agents-pipeline".into())
            .flex_shrink_0()
            .text_xs()
            .text_color(theme::text_muted())
            .child("No active pulsation")
            .into_any_element();
    };

    crate::widgets::pipeline_stepper(&status, "agents-pipeline", false)
}

pub(crate) fn agents_panel(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let agents = model.agents.read(cx);
    let loading = agents.loading;
    let busy = agents.busy;
    let snapshot = agents.snapshot.clone();
    let active_model_picker = agents.active_model_picker.clone();
    let pipeline = agents_pipeline_header(model, snapshot.as_ref(), cx);
    let focus = cx.focus_handle();

    div()
        .id("agents-panel")
        .debug_selector(|| "agents-panel".into())
        .relative()
        .overflow_hidden()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .h_full()
        .track_focus(&focus)
        .tab_index(0)
        .on_key_down(cx.listener(|model, event: &gpui::KeyDownEvent, _, cx| {
            if event.keystroke.key == "escape" && model.agents.read(cx).terminal_expanded {
                model.toggle_agents_terminal_expanded(cx);
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
                .child(pipeline)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .justify_end()
                        .gap_2()
                        .child(
                            div()
                                .min_w_0()
                                .text_xs()
                                .text_color(theme::text_muted())
                                .text_ellipsis()
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
                        let selected = model.agents.read(cx).selected_agent.clone();
                        let api_mode = match selected.as_ref() {
                            Some(AgentIdentifier::Canonical(role)) => snapshot
                                .agents
                                .iter()
                                .find(|agent| agent.config.agent_role == *role)
                                .is_some_and(|agent| {
                                    agent.config.execution_mode == AgentExecutionMode::ApiDirect
                                }),
                            Some(AgentIdentifier::FreeForm(slot_id)) => snapshot
                                .free_form_slots
                                .iter()
                                .chain(&snapshot.archived_free_form_slots)
                                .find(|slot| slot.slot.id == *slot_id)
                                .is_some_and(|slot| {
                                    slot.slot.execution_mode == AgentExecutionMode::ApiDirect
                                }),
                            None => false,
                        };
                        let detail = if api_mode {
                            api_detail_panel(model, &snapshot, cx)
                        } else {
                            agent_detail_panel(model, &snapshot, busy, cx)
                        };
                        crate::split_pane::split_pane(
                            "agents-columns",
                            &model.agents_split,
                            agents_sidebar(model, &snapshot, busy, cx),
                            detail,
                            cx,
                        )
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

fn agents_sidebar(
    model: &AppModel,
    snapshot: &ikore_core::terminals::TerminalsSnapshot,
    busy: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let agents = model.agents.read(cx);
    let selected = agents.selected_agent.clone();
    let fallbacks = agents.fallbacks.clone();
    div()
        .id("agents-list")
        .debug_selector(|| "agents-list".into())
        .w_full()
        .h_full()
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
                ikore_core::terminals::canonical_agent(role).name.to_owned(),
                agent.recent_runs.first().map(|run| run.status),
                agent.config.execution_mode,
                fallbacks
                    .iter()
                    .filter(|fallback| fallback.agent_role == role)
                    .count(),
                selected.as_ref() == Some(&id),
                id,
                cx,
            )
        }))
        .child(
            div()
                .mt_3()
                .text_xs()
                .text_color(theme::text_muted())
                .child("FREEFORM AGENTS"),
        )
        .child(action_button(
            "free-form-create".into(),
            "New freeform agent".into(),
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
                        format!(
                            "{}{}",
                            slot.slot.label,
                            if archived { " · arquivado" } else { "" }
                        ),
                        slot.recent_runs.first().map(|run| run.status),
                        slot.slot.execution_mode,
                        0,
                        selected.as_ref() == Some(&id),
                        id,
                        cx,
                    )
                }),
        )
        .into_any_element()
}

fn agent_list_item(
    label: String,
    status: Option<TerminalRunStatus>,
    execution_mode: AgentExecutionMode,
    fallback_count: usize,
    selected: bool,
    agent: AgentIdentifier,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let element_id = match &agent {
        AgentIdentifier::Canonical(role) => format!("agent-select:{}", role.as_str()),
        AgentIdentifier::FreeForm(slot_id) => format!("agent-select:{slot_id}"),
    };
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
            let selected = agent.clone();
            model.agents.update(cx, |agents, _| {
                agents.selected_agent = Some(agent.clone());
                agents.selected_api_target = None;
                agents.active_model_picker = None;
            });
            if model.settings.read(cx).snapshot.is_none() {
                model.refresh_settings(cx);
            }
            match selected {
                AgentIdentifier::Canonical(role) => model.configure_agent_api(role, cx),
                AgentIdentifier::FreeForm(slot_id) => model.configure_free_form_api(slot_id, cx),
            }
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
        .child(
            div()
                .mt_1()
                .text_xs()
                .text_color(theme::text_muted())
                .child(match execution_mode {
                    AgentExecutionMode::CliProcess => "CLI Mode",
                    AgentExecutionMode::ApiDirect => "API Key Mode",
                }),
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
    snapshot: &ikore_core::terminals::TerminalsSnapshot,
    busy: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let agents = model.agents.read(cx);
    let selected = agents.selected_agent.clone();
    let fallbacks = agents.fallbacks.clone();
    let pending_cancel = agents.pending_fallback_cancel.clone();
    let expanded = agents.terminal_expanded;
    if expanded {
        return expanded_cli_detail_panel(model, snapshot, busy, cx);
    }
    let panel = div()
        .id("agents-cli-scroll")
        .debug_selector(|| "agents-cli-scroll".into())
        .min_w_0()
        .flex_1()
        .h_full()
        .min_h_0()
        .p_4()
        .flex()
        .flex_col()
        .gap_3()
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
        );
    match selected {
        Some(AgentIdentifier::Canonical(role)) => {
            let agent = snapshot.agents.iter().find(|a| a.config.agent_role == role);
            match agent {
                Some(agent) => {
                    let descriptor = ikore_core::terminals::canonical_agent(role);
                    let availability = (agent.config.backend
                        != ikore_core::terminals::CliBackend::Custom)
                        .then(|| {
                            snapshot
                                .discovery
                                .iter()
                                .find(|item| item.backend == agent.config.backend)
                        })
                        .flatten();
                    let input = model.agent_prompt_inputs.get(&role).cloned();
                    let model_input = model.agent_model_inputs.get(&role).cloned();
                    let custom_executable_input =
                        model.agent_custom_executable_inputs.get(&role).cloned();
                    let custom_args_input = model.agent_custom_args_inputs.get(&role).cloned();
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
                        .get(&role)
                        .and_then(|run_id| agent.recent_runs.iter().find(|run| &run.id == run_id))
                        .or(active_run);
                    let executable_ready =
                        if agent.config.backend == ikore_core::terminals::CliBackend::Custom {
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
                                    catalog
                                        .models
                                        .iter()
                                        .find(|entry| entry.model_id.as_deref() == Some(model_id))
                                })
                                .is_some_and(|entry| !entry.profiles.is_empty())
                        });
                    let run_quota = selected_run.and_then(|run| {
                        crate::quota_bar::run_delta_line(agents_model.quota_deltas.get(&run.id)?)
                    });
                    let history_clear_armed = agents_model.pending_history_clear == Some(role);
                    let pipeline_cancel_armed = active_run
                        .and_then(|run| run.pulsation_id.as_deref())
                        .is_some_and(|pulsation_id| {
                            agents_model.pending_pipeline_cancel.as_deref() == Some(pulsation_id)
                        });
                    let event_filter = agents_model
                        .event_filters
                        .get(role.as_str())
                        .copied()
                        .unwrap_or_default();
                    let history_copied = selected_run
                        .is_some_and(|r| model.notifications.copied.contains_key(&r.id));
                    let run_console = console(
                        model,
                        ConsoleSurface {
                            key: role.as_str().to_owned(),
                            agent_label: descriptor.name,
                            run: selected_run,
                            filter: event_filter,
                            expanded: false,
                        },
                        cx,
                    );

                    panel
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_3()
                                .child(crate::widgets::status_dot(theme::agent_color(role)))
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
                                .child(crate::select_menu::mode_control(
                                    format!("agent-mode:{}", role.as_str()),
                                    "CLI",
                                    !model.settings.read(cx).busy && !running,
                                    crate::choices::custom_mode_reason(
                                        agent.config.backend,
                                        agent.config.custom_executable.as_deref(),
                                        agent.config.custom_risk_ack,
                                    ),
                                    cx,
                                )),
                        )
                        .child(agent_card(
                            descriptor,
                            agent,
                            availability,
                            input,
                            model_input,
                            custom_executable_input,
                            custom_args_input,
                            selected_run,
                            run_console,
                            history_clear_armed,
                            pipeline_cancel_armed,
                            thinking_available,
                            prompt_ready,
                            executable_ready,
                            running,
                            busy,
                            history_copied,
                            model.quota.backend(agent.config.backend).cloned(),
                            run_quota,
                            cx,
                        ))
                        .into_any_element()
                }
                None => panel
                    .child(empty_state("Select or create an agent."))
                    .into_any_element(),
            }
        }
        Some(AgentIdentifier::FreeForm(slot_id)) => {
            let slot = snapshot
                .free_form_slots
                .iter()
                .chain(&snapshot.archived_free_form_slots)
                .find(|slot| slot.slot.id == slot_id);
            match slot {
                Some(slot) => {
                    let active_run = slot.recent_runs.first();
                    let running = active_run.is_some_and(|run| run.status.is_active());
                    let slot_index = snapshot
                        .free_form_slots
                        .iter()
                        .position(|item| item.slot.id == slot.slot.id)
                        .unwrap_or(0);
                    let delete_armed = model.agents.read(cx).pending_free_form_delete.as_deref()
                        == Some(slot.slot.id.as_str());
                    let availability = (slot.slot.backend
                        != ikore_core::terminals::CliBackend::Custom)
                        .then(|| {
                            snapshot
                                .discovery
                                .iter()
                                .find(|item| item.backend == slot.slot.backend)
                        })
                        .flatten();
                    let executable_ready =
                        if slot.slot.backend == ikore_core::terminals::CliBackend::Custom {
                            slot.slot
                                .custom_executable
                                .as_ref()
                                .is_some_and(|path| !path.trim().is_empty())
                                && slot.slot.custom_risk_ack
                        } else {
                            availability
                                .is_some_and(|item| item.state == CliAvailabilityState::Available)
                        };
                    let status =
                        active_run
                            .map(|run| run_status(run.status))
                            .unwrap_or_else(|| {
                                if slot.slot.backend == ikore_core::terminals::CliBackend::Custom {
                                    if executable_ready {
                                        "Idle"
                                    } else {
                                        "Setup required"
                                    }
                                } else {
                                    availability_status(availability)
                                }
                            });
                    let id_up = slot.slot.id.clone();
                    let id_down = slot.slot.id.clone();
                    let id_archive = slot.slot.id.clone();
                    let id_delete = slot.slot.id.clone();
                    let card = if slot.slot.archived {
                        archived_free_form_card(model, slot, busy, cx)
                    } else {
                        free_form_card(model, snapshot, slot, busy, cx)
                    };
                    panel
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_3()
                                .child(
                                    div()
                                        .text_color(theme::accent())
                                        .font_weight(FontWeight::BOLD)
                                        .child("+"),
                                )
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex_1()
                                        .child(
                                            div()
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(theme::text())
                                                .child(slot.slot.label.clone()),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(theme::text_muted())
                                                .child(format!("FreeForm · {}", status)),
                                        ),
                                )
                                .child(action_button(
                                    SharedString::from(format!("free-form-up:{}", slot.slot.id)),
                                    "↑".into(),
                                    !busy && !running && slot_index > 0,
                                    false,
                                    cx,
                                    move |model, cx| {
                                        model.move_free_form_agent(id_up.clone(), -1, cx)
                                    },
                                ))
                                .child(action_button(
                                    SharedString::from(format!("free-form-down:{}", slot.slot.id)),
                                    "↓".into(),
                                    !busy
                                        && !running
                                        && slot_index + 1 < snapshot.free_form_slots.len(),
                                    false,
                                    cx,
                                    move |model, cx| {
                                        model.move_free_form_agent(id_down.clone(), 1, cx)
                                    },
                                ))
                                .child(action_button(
                                    SharedString::from(format!(
                                        "free-form-archive:{}",
                                        slot.slot.id
                                    )),
                                    "Arquivar".into(),
                                    !busy && !running && !slot.slot.archived,
                                    false,
                                    cx,
                                    move |model, cx| {
                                        model.archive_free_form_agent(id_archive.clone(), cx)
                                    },
                                ))
                                .child(action_button(
                                    SharedString::from(format!(
                                        "free-form-delete:{}",
                                        slot.slot.id
                                    )),
                                    if delete_armed {
                                        "Confirm deletion"
                                    } else {
                                        "Delete"
                                    }
                                    .into(),
                                    !busy && !running,
                                    true,
                                    cx,
                                    move |model, cx| {
                                        model.request_delete_free_form_agent(id_delete.clone(), cx)
                                    },
                                ))
                                .child(crate::select_menu::mode_control(
                                    format!("free-form-mode:{}", slot.slot.id),
                                    "CLI",
                                    !busy && !running && !slot.slot.archived,
                                    crate::choices::custom_mode_reason(
                                        slot.slot.backend,
                                        slot.slot.custom_executable.as_deref(),
                                        slot.slot.custom_risk_ack,
                                    ),
                                    cx,
                                )),
                        )
                        .child(card)
                        .into_any_element()
                }
                None => panel
                    .child(empty_state("Select or create an agent."))
                    .into_any_element(),
            }
        }
        None => panel
            .child(empty_state("Select or create an agent."))
            .into_any_element(),
    }
}

fn expanded_cli_detail_panel(
    model: &AppModel,
    snapshot: &ikore_core::terminals::TerminalsSnapshot,
    busy: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let agents = model.agents.read(cx);
    let selected = agents.selected_agent.clone();
    let canonical = match selected.as_ref() {
        Some(AgentIdentifier::Canonical(role)) => snapshot
            .agents
            .iter()
            .find(|agent| agent.config.agent_role == *role)
            .cloned(),
        _ => None,
    };
    let free_form = match selected.as_ref() {
        Some(AgentIdentifier::FreeForm(slot_id)) => snapshot
            .free_form_slots
            .iter()
            .chain(&snapshot.archived_free_form_slots)
            .find(|slot| slot.slot.id == *slot_id)
            .cloned(),
        _ => None,
    };
    let panel = div()
        .id("agents-terminal-expanded")
        .debug_selector(|| "agents-terminal-expanded".into())
        .flex_1()
        .h_full()
        .min_h_0()
        .min_w_0()
        .p_4()
        .flex()
        .flex_col()
        .gap_2()
        .bg(theme::surface());
    if let Some(agent) = canonical {
        let role = agent.config.agent_role;
        let descriptor = ikore_core::terminals::canonical_agent(role);
        let selected_run = agents
            .selected_run_by_role
            .get(&role)
            .and_then(|run_id| agent.recent_runs.iter().find(|run| &run.id == run_id))
            .or_else(|| agent.recent_runs.first())
            .cloned();
        let filter = agents
            .event_filters
            .get(role.as_str())
            .copied()
            .unwrap_or_default();
        let running = agent
            .recent_runs
            .first()
            .is_some_and(|run| run.status.is_active());
        let input = model.agent_prompt_inputs.get(&role).cloned();
        let prompt_ready = input
            .as_ref()
            .is_some_and(|input| !input.read(cx).value().trim().is_empty());
        let run_id = agent.recent_runs.first().map(|run| run.id.clone());
        let run_console = console(
            model,
            ConsoleSurface {
                key: role.as_str().to_owned(),
                agent_label: descriptor.name,
                run: selected_run.as_ref(),
                filter,
                expanded: true,
            },
            cx,
        );
        return panel
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(crate::widgets::status_dot(theme::agent_color(role)))
                    .child(
                        div()
                            .flex_1()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(descriptor.name),
                    )
                    .child(crate::select_menu::mode_control(
                        format!("agent-mode:{}", role.as_str()),
                        "CLI",
                        !busy && !running,
                        None,
                        cx,
                    ))
                    .child(icon_action(
                        SharedString::from(format!("agent-collapse:{}", role.as_str())),
                        Icon::ChevronDown,
                        "Collapse terminal",
                        true,
                        cx,
                        |model, cx| model.toggle_agents_terminal_expanded(cx),
                    )),
            )
            .when_some(selected_run.as_ref(), |panel, run| {
                panel.child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(run_metadata(run)),
                )
            })
            .child(run_console)
            .when_some(input, |panel, input| {
                panel.child(crate::sizing::text_area(input)).child(
                    div()
                        .flex()
                        .gap_2()
                        .when(running, |row| {
                            row.when_some(run_id, |row, run_id| {
                                row.child(action_button(
                                    SharedString::from(format!(
                                        "agent-expanded-stop:{}",
                                        role.as_str()
                                    )),
                                    "Stop".into(),
                                    !busy,
                                    true,
                                    cx,
                                    move |model, cx| {
                                        model.request_cancel_agent_run(run_id.clone(), None, cx)
                                    },
                                ))
                            })
                        })
                        .when(!running, |row| {
                            row.child(action_button(
                                SharedString::from(format!(
                                    "agent-expanded-start:{}",
                                    role.as_str()
                                )),
                                "Start".into(),
                                !busy && prompt_ready,
                                false,
                                cx,
                                move |model, cx| model.start_manual_agent(role, cx),
                            ))
                        }),
                )
            })
            .into_any_element();
    }
    if let Some(free_form) = free_form {
        let slot_id = free_form.slot.id.clone();
        let selected_run = agents
            .selected_run_by_slot
            .get(&slot_id)
            .and_then(|run_id| free_form.recent_runs.iter().find(|run| &run.id == run_id))
            .or_else(|| free_form.recent_runs.first())
            .cloned();
        let filter_key = format!("free-form:{slot_id}");
        let filter = agents
            .event_filters
            .get(&filter_key)
            .copied()
            .unwrap_or_default();
        let running = free_form
            .recent_runs
            .first()
            .is_some_and(|run| run.status.is_active());
        let input = model.free_form_prompt_inputs.get(&slot_id).cloned();
        let prompt_ready = input
            .as_ref()
            .is_some_and(|input| !input.read(cx).value().trim().is_empty());
        let run_id = free_form.recent_runs.first().map(|run| run.id.clone());
        let slot_for_start = slot_id.clone();
        let run_console = console(
            model,
            ConsoleSurface {
                key: filter_key,
                agent_label: &free_form.slot.label,
                run: selected_run.as_ref(),
                filter,
                expanded: true,
            },
            cx,
        );
        return panel
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(free_form.slot.label.clone()),
                    )
                    .child(crate::select_menu::mode_control(
                        format!("free-form-mode:{slot_id}"),
                        "CLI",
                        !busy && !running && !free_form.slot.archived,
                        None,
                        cx,
                    ))
                    .child(icon_action(
                        SharedString::from(format!("free-form-collapse:{slot_id}")),
                        Icon::ChevronDown,
                        "Collapse terminal",
                        true,
                        cx,
                        |model, cx| model.toggle_agents_terminal_expanded(cx),
                    )),
            )
            .when_some(selected_run.as_ref(), |panel, run| {
                panel.child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(run_metadata(run)),
                )
            })
            .child(run_console)
            .when_some(input, |panel, input| {
                panel.child(crate::sizing::text_area(input)).child(
                    div()
                        .flex()
                        .gap_2()
                        .when(running, |row| {
                            row.when_some(run_id, |row, run_id| {
                                row.child(action_button(
                                    SharedString::from(format!(
                                        "free-form-expanded-stop:{slot_id}"
                                    )),
                                    "Stop".into(),
                                    !busy,
                                    true,
                                    cx,
                                    move |model, cx| {
                                        model.request_cancel_agent_run(run_id.clone(), None, cx)
                                    },
                                ))
                            })
                        })
                        .when(!running, |row| {
                            row.child(action_button(
                                SharedString::from(format!("free-form-expanded-start:{slot_id}")),
                                "New session".into(),
                                !busy && prompt_ready && !free_form.slot.archived,
                                false,
                                cx,
                                move |model, cx| {
                                    model.start_free_form_agent(slot_for_start.clone(), cx)
                                },
                            ))
                        }),
                )
            })
            .into_any_element();
    }
    panel
        .child(empty_state("Select an agent."))
        .into_any_element()
}

fn archived_free_form_card(
    model: &AppModel,
    free_form: &ikore_core::terminals::FreeFormTerminalSnapshot,
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
        .bg(theme::surface_raised())
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
    snapshot: &ikore_core::terminals::TerminalsSnapshot,
    free_form: &ikore_core::terminals::FreeFormTerminalSnapshot,
    busy: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let slot = &free_form.slot;
    let slot_id = slot.id.clone();
    let availability = (slot.backend != ikore_core::terminals::CliBackend::Custom)
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
    let event_filter = agents
        .event_filters
        .get(&format!("free-form:{}", slot.id))
        .copied()
        .unwrap_or_default();
    let history_clear_armed =
        agents.pending_free_form_history_clear.as_deref() == Some(slot.id.as_str());
    let executable_ready = if slot.backend == ikore_core::terminals::CliBackend::Custom {
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
    let (can_resume, new_session) = selected_run
        .map(|run| {
            session_continuation(
                run,
                slot.backend,
                slot.model.as_deref(),
                availability.is_some_and(|item| item.capabilities.native_resume),
                executable_ready,
            )
        })
        .unwrap_or_default();
    let can_resume =
        can_resume && slot.session_policy == ikore_core::terminals::SessionPolicy::ResumeExplicit;
    let status = active_run
        .map(|run| run_status(run.status))
        .unwrap_or_else(|| {
            if slot.backend == ikore_core::terminals::CliBackend::Custom {
                if executable_ready {
                    "Idle"
                } else {
                    "Setup required"
                }
            } else {
                availability_status(availability)
            }
        });
    let run_console = console(
        model,
        ConsoleSurface {
            key: format!("free-form:{}", slot.id),
            agent_label: &slot.label,
            run: selected_run,
            filter: event_filter,
            expanded: false,
        },
        cx,
    );
    div()
        .id(SharedString::from(format!("free-form-card:{}", slot.id)))
        .min_w_0()
        .w_full()
        .flex_1()
        .p_4()
        .rounded_lg()
        .border_1()
        .border_color(theme::border())
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
                        .child(format!("CLI Configuration · {}", slot.label)),
                )
                .child(chip(status, theme::accent()))
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
                        ikore_core::terminals::SessionPolicy::FreshPerRun => "new per run",
                        ikore_core::terminals::SessionPolicy::ResumePerPulsation => "per pulsation",
                        ikore_core::terminals::SessionPolicy::ResumeExplicit => "explicit resume",
                    },
                    !busy && !running,
                    cx,
                )),
        )
        .when_some(label_input, |card, input| {
            card.child(field("NAME", input, crate::sizing::INPUT_MEDIUM))
        })
        .when(slot.backend != CliBackend::Custom, |card| {
            let model_value = cli_model_label(slot.model_source, slot.model.as_deref());
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
                card.child(field(
                    "MODEL (OPTIONAL)",
                    input,
                    crate::sizing::INPUT_MEDIUM,
                ))
            })
        })
        .when(
            slot.backend == ikore_core::terminals::CliBackend::Custom,
            |card| {
                card.when_some(custom_executable_input, |card, input| {
                    card.child(field("CUSTOM EXECUTABLE", input, crate::sizing::INPUT_LONG))
                })
                .when_some(custom_args_input, |card, input| {
                    card.child(field(
                        "EXACT ARGUMENTS (JSON)",
                        input,
                        crate::sizing::INPUT_LONG,
                    ))
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
                    .child(icon_action(
                        SharedString::from(format!("free-form-older:{}", slot.id)),
                        Icon::ChevronLeft,
                        "Older run",
                        !busy && index + 1 < free_form.recent_runs.len(),
                        cx,
                        {
                            let slot_id = slot_id.clone();
                            move |model, cx| model.select_free_form_history(slot_id.clone(), 1, cx)
                        },
                    ))
                    .child(icon_action(
                        SharedString::from(format!("free-form-newer:{}", slot.id)),
                        Icon::ChevronRight,
                        "Newer run",
                        !busy && index > 0,
                        cx,
                        {
                            let slot_id = slot_id.clone();
                            move |model, cx| model.select_free_form_history(slot_id.clone(), -1, cx)
                        },
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
        .child(run_console)
        .when_some(input, |card, input| {
            let selected_run_id = selected_run.map(|run| run.id.clone());
            card.child(crate::sizing::text_area(input)).child(
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
                            resume_label(new_session),
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
                                        new_session,
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
    agent: &ikore_core::terminals::AgentTerminalSnapshot,
    availability: Option<&ikore_core::terminals::CliAvailability>,
    input: Option<gpui::Entity<crate::text_input::NativeTextInput>>,
    model_input: Option<gpui::Entity<crate::text_input::NativeTextInput>>,
    custom_executable_input: Option<gpui::Entity<crate::text_input::NativeTextInput>>,
    custom_args_input: Option<gpui::Entity<crate::text_input::NativeTextInput>>,
    selected_run: Option<&ikore_core::terminals::TerminalRun>,
    run_console: AnyElement,
    history_clear_armed: bool,
    pipeline_cancel_armed: bool,
    thinking_available: bool,
    prompt_ready: bool,
    executable_ready: bool,
    running: bool,
    busy: bool,
    history_copied: bool,
    quota: Option<ikore_core::quota::BackendQuota>,
    run_quota: Option<String>,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let role = agent.config.agent_role;
    let color = theme::agent_color(role);
    let status = agent
        .recent_runs
        .first()
        .map(|run| run_status(run.status))
        .unwrap_or_else(|| {
            if agent.config.backend == ikore_core::terminals::CliBackend::Custom {
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
    let (can_resume, new_session) = selected_run
        .map(|run| {
            session_continuation(
                run,
                agent.config.backend,
                agent.config.model.as_deref(),
                availability.is_some_and(|item| item.capabilities.native_resume),
                executable_ready,
            )
        })
        .unwrap_or_default();
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
        .border_color(theme::border())
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
                // The agent's colour marks the title instead of outlining the whole card.
                .child(crate::widgets::status_dot(color))
                .child(
                    div()
                        .flex_1()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(format!("CLI Configuration · {}", descriptor.name)),
                )
                .child(chip(
                    status,
                    status_color(agent.recent_runs.first().map(|run| run.status)),
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
                ))
                .when(!thinking_available, |row| {
                    row.child(crate::widgets::info_tip(
                        format!("agent-thinking:{}", role.as_str()),
                        "This adapter does not declare stable thinking control.",
                    ))
                }),
        )
        .when(agent.config.backend != CliBackend::Custom, |card| {
            let model_value =
                cli_model_label(agent.config.model_source, agent.config.model.as_deref());
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
                card.child(field(
                    "MODEL (OPTIONAL)",
                    model_input,
                    crate::sizing::INPUT_MEDIUM,
                ))
            })
        })
        .when(
            agent.config.backend == ikore_core::terminals::CliBackend::Custom,
            |card| {
                card.when_some(custom_executable_input, |card, input| {
                    card.child(field("CUSTOM EXECUTABLE", input, crate::sizing::INPUT_LONG))
                })
                .when_some(custom_args_input, |card, input| {
                    card.child(field(
                        "EXACT ARGUMENTS (JSON)",
                        input,
                        crate::sizing::INPUT_LONG,
                    ))
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
        // An available CLI may still carry an advisory (e.g. newer than the last validated
        // version): worth a muted line, not a warning box.
        .when_some(
            availability.and_then(|availability| {
                (availability.state == CliAvailabilityState::Available)
                    .then(|| availability.diagnostic.clone())
                    .flatten()
            }),
            |card, note| {
                card.child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(note),
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
        // The same plan-usage bar as the header popover (ADR-028), for this card's CLI.
        .when_some(
            quota.as_ref().and_then(|quota| {
                crate::quota_bar::card_rows(quota, role.as_str(), chrono::Utc::now())
            }),
            |card, rows| card.child(rows),
        )
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
                    .when_some(run_quota.clone(), |row, line| {
                        let selector = format!("agent-run-quota:{}", role.as_str());
                        row.child(
                            div()
                                .debug_selector(move || selector.clone())
                                .w_full()
                                .text_xs()
                                .text_color(theme::text_muted())
                                .child(line),
                        )
                    })
                    .child(icon_action(
                        SharedString::from(format!("agent-history-older:{}", role.as_str())),
                        Icon::ChevronLeft,
                        "Older run",
                        !busy && index + 1 < agent.recent_runs.len(),
                        cx,
                        move |model, cx| model.select_agent_history(role, 1, cx),
                    ))
                    .child(icon_action(
                        SharedString::from(format!("agent-history-newer:{}", role.as_str())),
                        Icon::ChevronRight,
                        "Newer run",
                        !busy && index > 0,
                        cx,
                        move |model, cx| model.select_agent_history(role, -1, cx),
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
        .child(run_console)
        .when_some(input, |card, input| {
            let resume_run_id = selected_run.map(|run| run.id.clone());
            card.child(crate::sizing::text_area(input)).child(
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
                                "Confirm stop"
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
                            resume_label(new_session),
                            !busy && !running && prompt_ready && can_resume,
                            false,
                            cx,
                            move |model, cx| {
                                model.resume_agent_run(
                                    source_run_id.clone(),
                                    format!("canonical:{}", role.as_str()),
                                    Some(role),
                                    new_session,
                                    cx,
                                )
                            },
                        ))
                    })
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .text_xs()
                            .text_color(theme::text_muted())
                            .map(|status| {
                                if agent.config.execution_mode == AgentExecutionMode::ApiDirect {
                                    status.child("Manual execution blocked while agent uses API.")
                                } else if running {
                                    status.child("Input blocked during execution.")
                                } else {
                                    status.child(crate::widgets::info_tip(
                                        format!("agent-stdin:{}", role.as_str()),
                                        "Prompt will be sent via stdin; this is not a PTY terminal.",
                                    ))
                                }
                            }),
                    ),
            )
        })
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

/// A labelled single-line input at one of the scale widths (`crate::sizing`).
fn field(
    label: &'static str,
    input: gpui::Entity<crate::text_input::NativeTextInput>,
    width: gpui::Pixels,
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
        .child(crate::sizing::sized_input(input, width))
}

/// A labelled input that fills its grid cell (the cell, not the content type, sets the width).
fn grid_field(
    label: &'static str,
    input: gpui::Entity<crate::text_input::NativeTextInput>,
) -> impl IntoElement {
    div()
        .min_w_0()
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
        .child(div().w_full().child(input))
}

fn api_detail_panel(
    model: &AppModel,
    snapshot: &ikore_core::terminals::TerminalsSnapshot,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let agents = model.agents.read(cx);
    let selected_agent = agents.selected_agent.clone();
    let expanded = agents.terminal_expanded;
    let panel = div()
        .id("agents-api-scroll")
        .debug_selector(|| "agents-api-scroll".into())
        .flex_1()
        .h_full()
        .min_h_0()
        .p_4()
        .flex()
        .flex_col()
        .gap_3()
        .when(!expanded, |panel| panel.overflow_y_scroll());
    match selected_agent {
        Some(AgentIdentifier::Canonical(role)) => {
            let descriptor = ikore_core::terminals::canonical_agent(role);
            let activity = model
                .agents
                .read(cx)
                .api_activity
                .get(&AgentIdentifier::Canonical(role))
                .cloned()
                .unwrap_or_default();
            let history = model
                .agents
                .read(cx)
                .api_executions
                .get(&AgentIdentifier::Canonical(role).stable_key())
                .cloned()
                .unwrap_or_default();
            let panel = panel.child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(crate::widgets::status_dot(theme::agent_color(role)))
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
                    .child(crate::select_menu::mode_control(
                        format!("agent-mode:{}", role.as_str()),
                        "API",
                        !model.settings.read(cx).busy,
                        None,
                        cx,
                    )),
            );
            if expanded {
                panel
                    .child(api_activity_viewer(
                        model,
                        format!("canonical:{}", role.as_str()),
                        activity,
                        history,
                        cx,
                    ))
                    .into_any_element()
            } else {
                panel
                    .child(api_inline_editor(model, role, cx))
                    .into_any_element()
            }
        }
        Some(AgentIdentifier::FreeForm(slot_id)) => {
            let slot = snapshot
                .free_form_slots
                .iter()
                .chain(&snapshot.archived_free_form_slots)
                .find(|slot| slot.slot.id == slot_id);
            match slot {
                Some(slot) => {
                    let inner_slot = &slot.slot;
                    let inner_slot_id = inner_slot.id.clone();
                    let busy = model.settings.read(cx).busy || agents.busy;
                    let api_running = agents
                        .active_api_execution_by_slot
                        .contains_key(&inner_slot_id);
                    let delete_armed =
                        agents.pending_free_form_delete.as_deref() == Some(inner_slot_id.as_str());
                    let slot_index = snapshot
                        .free_form_slots
                        .iter()
                        .position(|item| item.slot.id == inner_slot_id)
                        .unwrap_or(0);
                    let id_up = inner_slot_id.clone();
                    let id_down = inner_slot_id.clone();
                    let id_archive = inner_slot_id.clone();
                    let id_delete = inner_slot_id.clone();
                    panel
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_3()
                                .child(
                                    div()
                                        .text_color(theme::accent())
                                        .font_weight(FontWeight::BOLD)
                                        .child("+"),
                                )
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex_1()
                                        .child(
                                            div()
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(theme::text())
                                                .child(inner_slot.label.clone()),
                                        )
                                        .child(
                                            div().text_xs().text_color(theme::text_muted()).child(
                                                format!(
                                                    "FreeForm · {}",
                                                    if inner_slot.archived {
                                                        "archived"
                                                    } else {
                                                        "active"
                                                    }
                                                ),
                                            ),
                                        ),
                                )
                                .child(action_button(
                                    SharedString::from(format!("free-form-up:{inner_slot_id}")),
                                    "↑".into(),
                                    !busy && !api_running && slot_index > 0,
                                    false,
                                    cx,
                                    move |model, cx| {
                                        model.move_free_form_agent(id_up.clone(), -1, cx)
                                    },
                                ))
                                .child(action_button(
                                    SharedString::from(format!("free-form-down:{inner_slot_id}")),
                                    "↓".into(),
                                    !busy
                                        && !api_running
                                        && slot_index + 1 < snapshot.free_form_slots.len(),
                                    false,
                                    cx,
                                    move |model, cx| {
                                        model.move_free_form_agent(id_down.clone(), 1, cx)
                                    },
                                ))
                                .child(action_button(
                                    SharedString::from(format!(
                                        "free-form-archive:{inner_slot_id}"
                                    )),
                                    "Arquivar".into(),
                                    !busy && !api_running && !inner_slot.archived,
                                    false,
                                    cx,
                                    move |model, cx| {
                                        model.archive_free_form_agent(id_archive.clone(), cx)
                                    },
                                ))
                                .child(action_button(
                                    SharedString::from(format!("free-form-delete:{inner_slot_id}")),
                                    if delete_armed {
                                        "Confirm deletion"
                                    } else {
                                        "Delete"
                                    }
                                    .into(),
                                    !busy && !api_running,
                                    true,
                                    cx,
                                    move |model, cx| {
                                        model.request_delete_free_form_agent(id_delete.clone(), cx)
                                    },
                                ))
                                .child(crate::select_menu::mode_control(
                                    format!("free-form-mode:{inner_slot_id}"),
                                    "API",
                                    !busy && !api_running && !inner_slot.archived,
                                    None,
                                    cx,
                                )),
                        )
                        .child(free_form_api_inline_editor(model, snapshot, slot, cx))
                        .into_any_element()
                }
                None => panel
                    .child(empty_state("Select an agent."))
                    .into_any_element(),
            }
        }
        None => panel
            .child(empty_state("Select an agent."))
            .into_any_element(),
    }
}

fn api_inline_editor(
    model: &AppModel,
    role: AgentRole,
    cx: &mut Context<AppModel>,
) -> impl IntoElement {
    let descriptor = ikore_core::terminals::canonical_agent(role);
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
        .get(&AgentIdentifier::Canonical(role))
        .cloned()
        .unwrap_or_default();
    let history = model
        .agents
        .read(cx)
        .api_executions
        .get(&AgentIdentifier::Canonical(role).stable_key())
        .cloned()
        .unwrap_or_default();
    div()
        .mt_3()
        .p_4()
        .rounded_lg()
        .border_1()
        .border_color(theme::border())
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
                // Same marker as the CLI card: the agent colour beside the title, not the outline.
                .child(crate::widgets::status_dot(theme::agent_color(role)))
                .child(
                    div()
                        .flex_1()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(format!("API Configuration · {}", descriptor.name)),
                )
                .child(crate::select_menu::control(
                    (SharedString::from(format!("api-cycle-route:{}", role.as_str()))).to_string(),
                    "Route",
                    selected_route
                        .as_ref()
                        .map(|route| route.name.as_str())
                        .unwrap_or("none")
                        .to_string(),
                    !busy,
                    cx,
                ))
                .child(crate::select_menu::control(
                    (SharedString::from(format!("api-cycle-connection:{}", role.as_str())))
                        .to_string(),
                    "Connection",
                    selected_connection
                        .as_ref()
                        .map(|connection| connection.display_name.as_str())
                        .unwrap_or("none")
                        .to_string(),
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
            // Two even columns, every field filling its cell, capped so a wide window does not
            // stretch a model id across the screen.
            div()
                .debug_selector(|| "api-form-grid".into())
                .w_full()
                .max_w(px(920.0))
                .grid()
                .grid_cols(2)
                .gap_x_4()
                .gap_y_3()
                .child(grid_field("CONNECTION NAME", connection_name))
                .child(crate::select_menu::field_control(
                    format!("api-provider:{}", role.as_str()),
                    "PROVIDER",
                    provider_id,
                    !busy,
                    cx,
                ))
                .child(grid_field("MODEL", connection_model))
                .child(grid_field("ENDPOINT (WHEN ALLOWED)", connection_endpoint))
                .child(grid_field("API KEY · KEYRING", connection_secret))
                .child(grid_field("ROUTE NAME", route_name)),
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
        .child(api_activity_viewer(
            model,
            format!("canonical:{}", role.as_str()),
            activity,
            history,
            cx,
        ))
}

fn free_form_api_inline_editor(
    model: &AppModel,
    snapshot: &ikore_core::terminals::TerminalsSnapshot,
    free_form: &ikore_core::terminals::FreeFormTerminalSnapshot,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let slot_id = free_form.slot.id.clone();
    let agents = model.agents.read(cx);
    let binding = agents
        .free_form_bindings
        .iter()
        .find(|binding| binding.project_id == snapshot.project_id && binding.slot_id == slot_id)
        .cloned();
    let activity = agents
        .api_activity
        .get(&AgentIdentifier::FreeForm(slot_id.clone()))
        .cloned()
        .unwrap_or_default();
    let history = agents
        .api_executions
        .get(&AgentIdentifier::FreeForm(slot_id.clone()).stable_key())
        .cloned()
        .unwrap_or_default();
    let running = agents.active_api_execution_by_slot.contains_key(&slot_id);
    let expanded = agents.terminal_expanded;
    let settings = model.settings.read(cx);
    let selected_route = settings.route().cloned();
    let bound_route = binding.as_ref().and_then(|binding| {
        settings
            .snapshot
            .as_ref()?
            .routes
            .iter()
            .find(|route| route.id == binding.route_id)
            .cloned()
    });
    let primary = bound_route.as_ref().and_then(|route| {
        route
            .targets
            .iter()
            .filter(|target| target.enabled)
            .min_by_key(|target| target.position)
    });
    let connection = primary.and_then(|target| {
        settings
            .snapshot
            .as_ref()?
            .connections
            .iter()
            .find(|connection| connection.id == target.connection_id)
    });
    let provider_label = connection
        .map(|connection| connection.provider_id.clone())
        .unwrap_or_else(|| "—".into());
    let model_label = primary
        .map(|target| target.model_id.clone())
        .filter(|model| !model.is_empty())
        .or_else(|| connection.map(|connection| connection.default_model.clone()))
        .unwrap_or_else(|| "—".into());
    let key_ready = connection.is_some_and(|connection| connection.has_api_key);
    let prompt = model.free_form_prompt_inputs.get(&slot_id).cloned();
    let prompt_ready = prompt
        .as_ref()
        .is_some_and(|input| !input.read(cx).value().trim().is_empty());
    let busy = settings.busy || model.agents.read(cx).busy;
    let slot_for_configure = slot_id.clone();
    let slot_for_binding = slot_id.clone();
    let slot_for_start = slot_id.clone();
    let slot_for_cancel = slot_id.clone();
    div()
        .id(SharedString::from(format!("api-free-form:{slot_id}")))
        .flex_1()
        .min_h_0()
        .p_4()
        .rounded_lg()
        .border_1()
        .border_color(theme::border())
        .bg(theme::surface())
        .flex()
        .flex_col()
        .gap_3()
        .when(!expanded, |panel| {
            panel.child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format!("API Configuration · {}", free_form.slot.label)),
                    )
                    .child(chip(
                        binding
                            .as_ref()
                            .map(|_| "Route bound")
                            .unwrap_or("Route required"),
                        if binding.is_some() {
                            theme::accent()
                        } else {
                            theme::warning()
                        },
                    ))
                    .child(div().text_sm().child(format!(
                            "Route: {}",
                            bound_route
                                .as_ref()
                                .map(|route| route.name.as_str())
                                .unwrap_or("not configured")
                        )))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::text_muted())
                            .child(format!(
                                "Provider: {} · Model: {} · Key: {}",
                                provider_label,
                                model_label,
                                if key_ready { "ready" } else { "pending" }
                            )),
                    )
                    .child(crate::select_menu::control(
                        format!("api-cycle-route:free-form:{slot_id}"),
                        "Route",
                        selected_route
                            .as_ref()
                            .map(|route| route.name.as_str())
                            .unwrap_or("none")
                            .to_string(),
                        !busy,
                        cx,
                    ))
                    .child(action_button(
                        SharedString::from(format!("api-configure-free-form:{slot_id}")),
                        "Use current binding".into(),
                        !busy,
                        false,
                        cx,
                        move |model, cx| {
                            model.configure_free_form_api(slot_for_configure.clone(), cx)
                        },
                    ))
                    .child(action_button(
                        SharedString::from(format!("api-bind-free-form:{slot_id}")),
                        "Bind selected route".into(),
                        !busy && selected_route.is_some(),
                        false,
                        cx,
                        move |model, cx| {
                            model.save_free_form_api_binding(slot_for_binding.clone(), cx)
                        },
                    ))
                    .child(action_button(
                        SharedString::from(format!("api-advanced-free-form:{slot_id}")),
                        "Advanced settings".into(),
                        true,
                        false,
                        cx,
                        |model, cx| model.select_view(crate::app_model::AppView::Settings, cx),
                    )),
            )
        })
        .child(api_activity_viewer(
            model,
            format!("free_form:{slot_id}"),
            activity,
            history,
            cx,
        ))
        .when_some(prompt, |panel, prompt| {
            panel.child(prompt).child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .when(!running, |row| {
                        row.child(action_button(
                            SharedString::from(format!("api-send-free-form:{slot_id}")),
                            "Send".into(),
                            !busy && prompt_ready && binding.is_some() && !free_form.slot.archived,
                            false,
                            cx,
                            move |model, cx| {
                                model.start_free_form_api_agent(slot_for_start.clone(), cx)
                            },
                        ))
                    })
                    .when(running, |row| {
                        row.child(action_button(
                            SharedString::from(format!("api-stop-free-form:{slot_id}")),
                            "Stop".into(),
                            !busy,
                            true,
                            cx,
                            move |model, cx| {
                                model.cancel_free_form_api_agent(slot_for_cancel.clone(), cx)
                            },
                        ))
                    })
                    .child(crate::widgets::info_tip(
                        format!("api-free-form:{slot_id}"),
                        "One stateless API round; no CLI session or resume.",
                    )),
            )
        })
        .into_any_element()
}

fn api_activity_viewer(
    model: &AppModel,
    key: String,
    activity: Vec<String>,
    history: Vec<ikore_core::llm::LlmExecutionListItem>,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let agents = model.agents.read(cx);
    let expanded = agents.terminal_expanded;
    let viewer_enabled = agents.viewer_enabled;
    let filter_key = format!("api:{key}");
    let filter = agents
        .api_filters
        .get(&filter_key)
        .copied()
        .unwrap_or(ApiActivityFilter::All);
    let target = if let Some(role) = key.strip_prefix("canonical:").and_then(AgentRole::parse) {
        AgentIdentifier::Canonical(role)
    } else {
        AgentIdentifier::FreeForm(key.trim_start_matches("free_form:").to_string())
    };
    // Oldest first and the newest at the bottom, like a run's console
    // (ADR-040, M2): the persisted executions, then what arrived live.
    let visible = activity
        .into_iter()
        .filter(|line| filter.accepts(line))
        .collect::<Vec<_>>();
    let copy_text = visible.join("\n");
    let key_for_filter = filter_key.clone();
    let key_for_follow = filter_key.clone();
    let key_for_pill = filter_key.clone();
    let target_for_clear = target.clone();
    let target_for_export = target.clone();
    let latest_execution_id = history
        .first()
        .map(|execution| execution.execution_id.clone());
    let summary = history.first().map_or_else(
        || "No persisted execution".to_owned(),
        |execution| format!("{} · {}", execution.status, execution.route_id),
    );
    let history_rows = history
        .iter()
        .take(20)
        .rev()
        .map(|execution| {
            let preview = execution
                .visible_output
                .as_deref()
                .unwrap_or("No visible output")
                .chars()
                .take(240)
                .collect::<String>();
            (
                format!(
                    "{} · {} · {}",
                    execution.status, execution.route_id, execution.started_at
                ),
                preview,
            )
        })
        .collect::<Vec<_>>();
    let empty = visible.is_empty() && history_rows.is_empty();
    let lines = agents.consoles.lines_view(
        &filter_key,
        history_rows.len() + visible.len(),
        visible.last().map_or_else(
            || history_rows.last().map_or(0, |row| row.1.len()),
            String::len,
        ),
    );
    let viewer_id = key.clone();
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child("ACTIVITY AND OUTPUT"))
                        .child(crate::widgets::info_tip(
                            format!("api-activity:{key}"),
                            "Displays visible events and output only; internal reasoning is not captured.",
                        )),
                )
                .child(action_button(
                    SharedString::from(format!("api-viewer-toggle:{key}")),
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
                    SharedString::from(format!("api-filter:{key}")),
                    format!("Filter: {}", filter.label()).into(),
                    true,
                    false,
                    cx,
                    move |model, cx| model.cycle_agent_api_filter(key_for_filter.clone(), cx),
                ))
                .child(action_button(
                    SharedString::from(format!("api-copy:{key}")),
                    "Copy".into(),
                    !copy_text.is_empty(),
                    false,
                    cx,
                    move |_, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy_text.clone()));
                    },
                ))
                .child(action_button(
                    SharedString::from(format!("api-export:{key}")),
                    "Export".into(),
                    latest_execution_id.is_some(),
                    false,
                    cx,
                    move |model, cx| {
                        if let Some(execution_id) = latest_execution_id.clone() {
                            model.export_agent_api_execution(
                                target_for_export.clone(),
                                execution_id,
                                cx,
                            );
                        }
                    },
                ))
                .child(action_button(
                    SharedString::from(format!("api-clear:{key}")),
                    "Clear local".into(),
                    true,
                    false,
                    cx,
                    move |model, cx| model.clear_agent_api_activity(target_for_clear.clone(), cx),
                ))
                .child(icon_action(
                    SharedString::from(format!("api-expand:{key}")),
                    if expanded {
                        Icon::ChevronDown
                    } else {
                        Icon::ChevronUp
                    },
                    if expanded {
                        "Collapse viewer"
                    } else {
                        "Expand viewer"
                    },
                    true,
                    cx,
                    |model, cx| model.toggle_agents_terminal_expanded(cx),
                )),
        )
        .child({
            // The console's frame and header, with the lines in place of
            // blocks (ADR-040, M2).
            let mut frame = crate::agent_console::view::frame(
                format!("api-activity:{key}"),
                format!("api-activity:{viewer_id}"),
                expanded,
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .border_b_1()
                    .border_color(theme::border())
                    .text_xs()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme::text())
                            .child(summary),
                    )
                    .child(crate::agent_console::view::follow_control(
                        format!("api-follow:{key}"),
                        lines.following,
                        cx,
                        move |model, cx| model.console_follow_lines(&key_for_follow, cx),
                    )),
            )
            .child(
                div()
                    .id(SharedString::from(format!("api-activity-lines:{key}")))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&lines.scroll)
                    .p_2()
                    .when(!viewer_enabled, |viewer| {
                        viewer.child(
                            div().text_xs().text_color(theme::text_muted()).child(
                                "Live viewer is paused. Execution and persisted history continue.",
                            ),
                        )
                    })
                    .when(viewer_enabled && empty, |viewer| {
                        viewer.child(
                            div()
                                .text_xs()
                                .text_color(theme::text_muted())
                                .child("No API activity or persisted execution for this agent."),
                        )
                    })
                    .when(viewer_enabled && !history_rows.is_empty(), |viewer| {
                        viewer
                            .child(
                                div()
                                    .pb_1()
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::text_muted())
                                    .child("Persisted history"),
                            )
                            .children(history_rows.into_iter().map(|(title, preview)| {
                                div()
                                    .py_1()
                                    .border_b_1()
                                    .border_color(theme::border())
                                    .child(div().text_xs().child(title))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme::text_muted())
                                            .child(preview),
                                    )
                            }))
                    })
                    .when(viewer_enabled, |viewer| {
                        viewer.children(visible.into_iter().map(|line| {
                            div()
                                .py_1()
                                .text_xs()
                                .text_color(theme::text_muted())
                                .child(line)
                        }))
                    }),
            );
            if !lines.following && lines.unseen > 0 {
                frame = frame.child(crate::agent_console::view::new_pill(
                    format!("api-new-pill:{key}"),
                    lines.unseen,
                    cx,
                    move |model, cx| model.console_follow_lines(&key_for_pill, cx),
                ));
            }
            frame
        })
        .into_any_element()
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
        match run.origin {
            ikore_core::terminals::ExecutionOrigin::Pipeline => "pipeline",
            ikore_core::terminals::ExecutionOrigin::Learning => "learning",
            ikore_core::terminals::ExecutionOrigin::Manual => "manual",
            ikore_core::terminals::ExecutionOrigin::Feature => "feature",
            ikore_core::terminals::ExecutionOrigin::Review => "review",
            ikore_core::terminals::ExecutionOrigin::Notes => "notes",
            ikore_core::terminals::ExecutionOrigin::Reconciliation => "reconciliation",
        },
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

fn config_button(
    id: String,
    label: &'static str,
    value: impl Into<SharedString>,
    enabled: bool,
    cx: &mut Context<AppModel>,
    handler: impl Fn(&mut AppModel, &mut Context<AppModel>) + 'static,
) -> impl IntoElement {
    let value = value.into();
    // A picker (MODEL) reads like the selects beside it; a toggle (EXECUTABLE RISK) has no chevron.
    let opens_list = label != "EXECUTABLE RISK";
    div()
        .id(SharedString::from(id))
        // Label-over-value controls read at a fixed width; they no longer fill the row.
        .flex_shrink_0()
        .min_w(px(160.0))
        .max_w(px(360.0))
        .px_3()
        .py_2()
        .rounded_md()
        .border_1()
        .border_color(theme::border_strong())
        .bg(theme::surface_raised())
        .opacity(if enabled { 1.0 } else { 0.55 })
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .hover(|style| style.bg(theme::surface_hover()))
                .on_click(cx.listener(move |model, _, _, cx| handler(model, cx)))
        })
        .child(div().text_xs().text_color(theme::text_muted()).child(label))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .text_sm()
                .child(div().min_w_0().text_ellipsis().child(value))
                .when(opens_list, |row| {
                    row.child(crate::widgets::icon(
                        crate::icons::Icon::ChevronDown,
                        px(14.0),
                        theme::text_muted(),
                    ))
                }),
        )
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
