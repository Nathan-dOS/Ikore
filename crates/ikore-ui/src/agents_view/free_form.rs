//! The free-form slots' cards (ADR-040 M7, 12.4).

use gpui::{AnyElement, Context, FontWeight, IntoElement, SharedString, div, prelude::*, px};
use ikore_core::terminals::{AgentExecutionMode, CliAvailabilityState, CliBackend};

use crate::agent_console::view::{ConsoleSurface, console};
use crate::agents::AgentIdentifier;
use crate::app_model::AppModel;
use crate::icons::Icon;
use crate::theme;

use super::config::{config_button, field};
use super::detail::{DetailHeader, detail_header, detail_tabs};
use super::history::history_tab;
use super::{
    action_button, availability_status, cli_model_label, icon_action, resume_label, run_metadata,
    run_status, session_continuation, status_color,
};

pub(super) fn archived_free_form_card(
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

pub(super) fn free_form_card(
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
            narrow: false,
        },
        cx,
    );
    // ADR-040 M7: the same tabs as a stage's card, under a fixed header.
    let tab = model.agents.read(cx).detail_tab;
    let history = history_tab(
        model,
        &AgentIdentifier::FreeForm(slot.id.clone()),
        &free_form.recent_runs,
        selected_run.map(|run| run.id.as_str()),
        cx,
    );
    let slot_index = snapshot
        .free_form_slots
        .iter()
        .position(|item| item.slot.id == slot.id)
        .unwrap_or(0);
    let delete_armed =
        model.agents.read(cx).pending_free_form_delete.as_deref() == Some(slot.id.as_str());
    // The slot's own controls: its place in the list, archive and delete.
    let management = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_2()
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
            "Archive".into(),
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
        ));
    let config = div()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                // The transport's segmented control (12.2).
                .child(crate::select_menu::mode_control(
                    format!("free-form-mode:{}", slot.id),
                    "CLI",
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
                        ikore_core::terminals::SessionPolicy::FreshPerRun => "new per run",
                        ikore_core::terminals::SessionPolicy::ResumePerPulsation => "per pulsation",
                        ikore_core::terminals::SessionPolicy::ResumeExplicit => "explicit resume",
                    },
                    !busy && !running,
                    cx,
                )),
        )
        .child(management)
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
        ));
    let activity = div()
        .flex()
        .flex_col()
        .gap_3()
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
                        "Export".into(),
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
            card.child(crate::sizing::text_area(input))
        });
    // Stop, New session and Resume stay in the header whatever the tab (12.2).
    let selected_run_id = selected_run.map(|run| run.id.clone());
    let actions = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_2()
        .when(running, |row| {
            let run_id = active_run.map(|run| run.id.clone()).unwrap_or_default();
            row.child(action_button(
                SharedString::from(format!("free-form-stop:{}", slot.id)),
                "Stop".into(),
                !busy,
                true,
                cx,
                move |model, cx| model.request_cancel_agent_run(run_id.clone(), None, cx),
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
        })
        .into_any_element();
    let model_chip = selected_run
        .and_then(|run| run.observed_model.clone().or_else(|| run.model.clone()))
        .or_else(|| slot.model.clone())
        .unwrap_or_else(|| "CLI default".into());
    let session = selected_run.and_then(|run| run.native_session_id.clone());
    let key = format!("free-form:{}", slot.id);
    let content = match tab {
        crate::agents::AgentDetailTab::Activity => activity.into_any_element(),
        crate::agents::AgentDetailTab::Configuration => config.into_any_element(),
        crate::agents::AgentDetailTab::History => history,
    };
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
        .child(detail_header(
            DetailHeader {
                key: key.clone(),
                name: slot.label.clone(),
                subtitle: Some("Free-form".into()),
                color: theme::accent(),
                status,
                status_color: status_color(active_run.map(|run| run.status)),
                model_chip,
                session,
                actions,
            },
            cx,
        ))
        .child(detail_tabs(&key, tab, cx))
        .child(content)
        .into_any_element()
}
