//! API Mode: the route editors and the activity viewer (ADR-040 M7, 12.4).
//! On the Agents canvas (feature 25, M1) they are the synthesis of an agent
//! that runs by API, and the History drawer shows the activity viewer.

use gpui::{AnyElement, Context, FontWeight, IntoElement, SharedString, div, prelude::*, px};
use ikore_core::llm::AgentRole;

use crate::agents::{AgentDetailTab, AgentIdentifier, ApiActivityFilter};
use crate::app_model::AppModel;
use crate::icons::Icon;
use crate::theme;

use super::config::grid_field;
use super::synthesis::{synthesis_actions, waiting_review};
use super::{action_button, chip, empty_state, icon_action};

pub(super) fn api_detail_panel(
    model: &AppModel,
    snapshot: &ikore_core::terminals::TerminalsSnapshot,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let agents = model.agents.read(cx);
    let selected_agent = agents.selected_agent.clone();
    // Feature 25, M1: the History drawer shows the activity viewer.
    let tab = agents.detail_tab;
    let expanded = tab == AgentDetailTab::History;
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
            let review = waiting_review(model, &AgentIdentifier::Canonical(role), cx);
            let panel = panel.child(synthesis_actions(role.as_str(), tab, review, false, cx));
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
                                    "Archive".into(),
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
                        .child(synthesis_actions(
                            &format!("free-form:{inner_slot_id}"),
                            tab,
                            None,
                            false,
                            cx,
                        ))
                        .child(free_form_api_inline_editor(model, snapshot, slot, cx))
                        .into_any_element()
                }
                None => panel
                    .child(empty_state("Select an agent."))
                    .into_any_element(),
            }
        }
        Some(AgentIdentifier::PipelineStep { .. }) | None => panel
            .child(empty_state("Select an agent."))
            .into_any_element(),
    }
}

pub(super) fn api_inline_editor(
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

pub(super) fn free_form_api_inline_editor(
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
    let expanded = agents.detail_tab == AgentDetailTab::History;
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

pub(super) fn api_activity_viewer(
    model: &AppModel,
    key: String,
    activity: Vec<String>,
    history: Vec<ikore_core::llm::LlmExecutionListItem>,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let agents = model.agents.read(cx);
    let expanded = agents.detail_tab == AgentDetailTab::History;
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
                    |model, cx| model.toggle_agents_history(cx),
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
