//! An agent's detail: the header, its tabs and the expanded console (ADR-040 M7, 12.2).

use gpui::{AnyElement, Context, FontWeight, IntoElement, Rgba, SharedString, div, prelude::*, px};
use ikore_core::terminals::{
    AgentExecutionMode, CanonicalAgentDescriptor, CliAvailabilityState, TerminalRunStatus,
};

use crate::agent_console::view::{ConsoleSurface, console};
use crate::agents::AgentIdentifier;
use crate::app_model::AppModel;
use crate::icons::Icon;
use crate::theme;

use super::config::{StageConfiguration, stage_configuration};
use super::free_form::{archived_free_form_card, free_form_card};
use super::history::history_tab;
use super::{
    action_button, availability_status, chip, empty_state, fallback_panel, icon_action,
    resume_label, run_metadata, run_status, session_continuation, status_color,
};

pub(super) fn agent_detail_panel(
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
                    let detail_tab = agents_model.detail_tab;
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
                            narrow: false,
                        },
                        cx,
                    );
                    let history = history_tab(
                        model,
                        &AgentIdentifier::Canonical(role),
                        &agent.recent_runs,
                        selected_run.map(|run| run.id.as_str()),
                        cx,
                    );

                    // The transport's segmented control lives in Configuration (12.2).
                    let transport = crate::select_menu::mode_control(
                        format!("agent-mode:{}", role.as_str()),
                        "CLI",
                        !model.settings.read(cx).busy && !running,
                        crate::choices::custom_mode_reason(
                            agent.config.backend,
                            agent.config.custom_executable.as_deref(),
                            agent.config.custom_risk_ack,
                        ),
                        cx,
                    )
                    .into_any_element();
                    panel
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
                            detail_tab,
                            history,
                            transport,
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
                Some(slot) if slot.slot.archived => panel
                    .child(archived_free_form_card(model, slot, busy, cx))
                    .into_any_element(),
                Some(slot) => panel
                    .child(free_form_card(model, snapshot, slot, busy, cx))
                    .into_any_element(),
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

pub(super) fn expanded_cli_detail_panel(
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
                narrow: false,
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
                narrow: false,
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

#[allow(clippy::too_many_arguments)]
pub(super) fn agent_card(
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
    tab: crate::agents::AgentDetailTab,
    history: AnyElement,
    transport: AnyElement,
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
    // ADR-040 M7: Configuration, Activity and History are tabs under a fixed header.
    let config = stage_configuration(
        StageConfiguration {
            agent,
            availability,
            model_input,
            custom_executable_input,
            custom_args_input,
            quota,
            transport,
            thinking_available,
            running,
            busy,
        },
        cx,
    );
    let activity = div()
        .flex()
        .flex_col()
        .gap_3()
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
                        "Export".into(),
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
            card.child(crate::sizing::text_area(input)).child(
                div()
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
            )
        });
    // Start, Stop and Resume stay in the header whatever the tab (12.2).
    let resume_run_id = selected_run.map(|run| run.id.clone());
    let actions = div()
        .flex()
        .items_center()
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
                    model.request_cancel_agent_run(run_id.clone(), pulsation_id.clone(), cx)
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
        .into_any_element();
    let model_chip = selected_run
        .and_then(|run| run.observed_model.clone().or_else(|| run.model.clone()))
        .or_else(|| agent.config.model.clone())
        .unwrap_or_else(|| "CLI default".into());
    let session = selected_run.and_then(|run| run.native_session_id.clone());
    let content = match tab {
        crate::agents::AgentDetailTab::Activity => activity.into_any_element(),
        crate::agents::AgentDetailTab::Configuration => config,
        crate::agents::AgentDetailTab::History => history,
    };
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
        .child(detail_header(
            DetailHeader {
                key: role.as_str().to_owned(),
                name: descriptor.name.to_owned(),
                subtitle: Some(descriptor.stage.to_owned()),
                color,
                status,
                status_color: status_color(agent.recent_runs.first().map(|run| run.status)),
                model_chip,
                session,
                actions,
            },
            cx,
        ))
        .child(detail_tabs(role.as_str(), tab, cx))
        .child(content)
        .into_any_element()
}

/// What the detail's fixed header shows (12.2).
pub(super) struct DetailHeader {
    pub(super) key: String,
    pub(super) name: String,
    /// The pipeline stage, for a stage's agent.
    pub(super) subtitle: Option<String>,
    pub(super) color: Rgba,
    pub(super) status: &'static str,
    pub(super) status_color: Rgba,
    pub(super) model_chip: String,
    pub(super) session: Option<String>,
    /// Start, Stop and Resume.
    pub(super) actions: AnyElement,
}

/// The detail's fixed header (12.2): who, how it is, the model, the session
/// and the main actions, the same over every tab.
pub(super) fn detail_header(header: DetailHeader, cx: &mut Context<AppModel>) -> AnyElement {
    let DetailHeader {
        key,
        name,
        subtitle,
        color,
        status,
        status_color,
        model_chip,
        session,
        actions,
    } = header;
    let selector = format!("agent-header:{key}");
    div()
        .debug_selector(move || selector.clone())
        .flex()
        .flex_wrap()
        .items_center()
        .gap_2()
        // The agent's colour marks the title instead of outlining the whole card.
        .child(crate::widgets::status_dot(color))
        .child(
            div()
                .min_w_0()
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::text())
                        .child(name),
                )
                .when_some(subtitle, |title, subtitle| {
                    title.child(
                        div()
                            .text_xs()
                            .text_color(theme::text_muted())
                            .child(subtitle),
                    )
                }),
        )
        .child(chip(status, status_color))
        .child(div().flex_1())
        .child(
            div()
                .px_2()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(theme::border())
                .text_xs()
                .text_color(theme::text_muted())
                .child(model_chip),
        )
        .when_some(session, |header, session| {
            let short = session.chars().take(8).collect::<String>();
            header.child(
                div()
                    .id(SharedString::from(format!("agent-session:{key}")))
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(theme::border())
                    .text_xs()
                    .text_color(theme::text_muted())
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::surface_hover()))
                    .tooltip(|_, cx| {
                        cx.new(|_| crate::widgets::Tooltip::new("Copy the session id"))
                            .into()
                    })
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(session.clone()));
                    }))
                    .child(crate::widgets::icon(
                        Icon::Copy,
                        px(12.0),
                        theme::text_muted(),
                    ))
                    .child(format!("session {short}")),
            )
        })
        .child(actions)
        .into_any_element()
}

/// Activity, Configuration and History (12.2).
pub(super) fn detail_tabs(
    key: &str,
    selected: crate::agents::AgentDetailTab,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    use crate::agents::AgentDetailTab;
    div()
        .flex()
        .items_center()
        .gap_1()
        .border_b_1()
        .border_color(theme::border())
        .children(AgentDetailTab::ALL.into_iter().map(|tab| {
            let selector = format!("agent-tab:{key}:{}", tab.id());
            let active = tab == selected;
            div()
                .id(SharedString::from(selector.clone()))
                .debug_selector(move || selector.clone())
                // Reached by Tab; Ctrl+Shift+[ and ] cycle them from anywhere on the page.
                .tab_index(0)
                .px_3()
                .py_1()
                .rounded_t_md()
                .text_sm()
                .cursor_pointer()
                .border_b_2()
                .border_color(if active {
                    gpui::Hsla::from(theme::accent())
                } else {
                    gpui::transparent_black()
                })
                .text_color(if active {
                    theme::text()
                } else {
                    theme::text_muted()
                })
                // Backgrounds only in the refinements: a text colour there replaces the whole
                // text style in GPUI, and the label would lose its size.
                .when(!active, |tab| {
                    tab.hover(|style| style.bg(theme::hover_bg()))
                })
                .focus_visible(|style| style.bg(theme::accent_soft()))
                .on_click(cx.listener(move |model, _, _, cx| {
                    model.agents.update(cx, |agents, _| agents.detail_tab = tab);
                    cx.notify();
                }))
                .child(tab.label())
        }))
        .into_any_element()
}
