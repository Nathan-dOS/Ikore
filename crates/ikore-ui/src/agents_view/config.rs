//! A stage's Configuration tab and the controls the agent cards share
//! (ADR-040 M7, 12.2 and 12.4).

use gpui::{AnyElement, Context, FontWeight, IntoElement, SharedString, div, prelude::*, px};
use ikore_core::terminals::{CliAvailabilityState, CliBackend};

use crate::app_model::AppModel;
use crate::theme;

use super::{action_button, cli_model_label};

/// A labelled single-line input at one of the scale widths (`crate::sizing`).
pub(super) fn field(
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
pub(super) fn grid_field(
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

pub(super) fn config_button(
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
    let selector = id.clone();
    div()
        .id(SharedString::from(id))
        .debug_selector(move || selector.clone())
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

/// What a stage's Configuration tab shows (ADR-040 M7, 12.2).
pub(super) struct StageConfiguration<'a> {
    pub(super) agent: &'a ikore_core::terminals::AgentTerminalSnapshot,
    pub(super) availability: Option<&'a ikore_core::terminals::CliAvailability>,
    pub(super) model_input: Option<gpui::Entity<crate::text_input::NativeTextInput>>,
    pub(super) custom_executable_input: Option<gpui::Entity<crate::text_input::NativeTextInput>>,
    pub(super) custom_args_input: Option<gpui::Entity<crate::text_input::NativeTextInput>>,
    pub(super) quota: Option<ikore_core::quota::BackendQuota>,
    /// The transport's segmented control (CLI or API).
    pub(super) transport: AnyElement,
    pub(super) thinking_available: bool,
    pub(super) running: bool,
    pub(super) busy: bool,
}

/// A stage's Configuration tab: the transport, the backend, the profile and
/// the model, a custom executable's fields, the CLI's state and the plan's
/// quota.
pub(super) fn stage_configuration(
    configuration: StageConfiguration<'_>,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let StageConfiguration {
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
    } = configuration;
    let role = agent.config.agent_role;
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
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(transport)
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
                .child(crate::select_menu::control(
                    format!("agent-cli-silence:{}", role.as_str()),
                    "CLI SILENCE LIMIT",
                    agent.config.cli_silence_timeout_ms
                        .map(|ms| format!("{} seconds", ms / 1000))
                        .unwrap_or_else(|| format!("Default ({} seconds)", ikore_core::AgentTimeoutPolicy::for_role(role).inactivity_window_ms / 1000)),
                    !busy && !running && agent.config.execution_mode == ikore_core::terminals::AgentExecutionMode::CliProcess,
                    cx,
                ))
                .when(!thinking_available, |row| {
                    row.child(crate::widgets::info_tip(
                        format!("agent-thinking:{}", role.as_str()),
                        "This adapter does not declare stable thinking control.",
                    ))
                }),
        )
        .child(div().text_xs().text_color(theme::text_muted()).child("CLI silence limit applies to this project's agent. The overall execution limit still applies; API routes use their own timeouts."))
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
            |card, note| card.child(div().text_xs().text_color(theme::text_muted()).child(note)),
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
        .into_any_element()
}
