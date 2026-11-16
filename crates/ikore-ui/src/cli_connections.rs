//! Global CLI login status and plan usage in Settings › Connections. The usage windows come
//! from the plan-quota meter (ADR-028); this page also names each CLI's usage sources and holds
//! the switch of the collectors that read a CLI's own sign-in.

use chrono::Utc;
use gpui::{AnyElement, Context, IntoElement, div, prelude::*};
use ikore_core::quota::{BackendQuota, QuotaOverview};
use ikore_core::terminals::{CliAvailabilityState, CliBackend, CliConnectionStatus};

use crate::app_model::AppModel;
use crate::settings_search::entry_by_id;
use crate::settings_ui::{note, stacked_row};
use crate::{quota_bar, settings_view, theme};

pub(crate) fn section(
    model: &AppModel,
    highlight: Option<&str>,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let (loading, snapshot, error) = {
        let settings = model.settings.read(cx);
        (
            settings.cli_connections_loading,
            settings.cli_connections.clone(),
            settings
                .cli_connections_error
                .as_ref()
                .map(|error| error.presentable()),
        )
    };
    let mut body = div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_3()
        .child(settings_view::action(
            "settings-refresh-clis",
            if loading {
                "Checking CLIs…"
            } else {
                "Refresh CLIs & usage"
            },
            !loading,
            cx,
            |model, cx| model.refresh_cli_connections(true, cx),
        ));
    if let Some(error) = error {
        body = body.child(note(error));
    }
    if let Some(snapshot) = &snapshot {
        body = body.child(note(format!(
            "Last checked: {}{}",
            crate::time::format_timestamp(&snapshot.checked_at),
            if loading {
                " · refreshing; showing the previous check"
            } else {
                ""
            },
        )));
        if let Some(reason) = snapshot.overview.credential_collectors_off {
            body = body.child(note(reason));
        }
        if snapshot.connections.is_empty() {
            body = body.child(note("No CLI connection information is available."));
        }
        // The page's own copy can lag the header's; prefer the newer overview for the quota.
        let overview = model
            .quota
            .overview
            .as_ref()
            .filter(|overview| overview.checked_at > snapshot.overview.checked_at)
            .unwrap_or(&snapshot.overview);
        for connection in &snapshot.connections {
            body = body.child(connection_card(connection, overview, cx));
        }
    } else {
        body = body.child(note(if loading {
            "Checking installed CLIs and their login status…"
        } else {
            "Refresh to check installed CLIs and their plan usage."
        }));
    }
    crate::settings_ui::section(
        "CLI connections",
        Some(entry_by_id("cli-connections").description),
        vec![stacked_row("cli-connections", highlight, body)],
    )
}

fn connection_card(
    connection: &CliConnectionStatus,
    overview: &QuotaOverview,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let cli = &connection.availability;
    let selector = format!("settings-cli:{}", cli.backend.as_str());
    let color = match cli.state {
        CliAvailabilityState::Available => theme::success(),
        CliAvailabilityState::Missing => theme::text_muted(),
        _ => theme::warning(),
    };
    let status = match cli.state {
        CliAvailabilityState::Available if cli.capabilities.auth_probe => "Connected",
        CliAvailabilityState::Available => "Installed · login not checked",
        CliAvailabilityState::Missing => "Not installed",
        CliAvailabilityState::Unsupported => "Unavailable",
        CliAvailabilityState::AuthRequired => "Login required",
        CliAvailabilityState::Probing => "Checking…",
        CliAvailabilityState::UnsupportedVersion => "Unsupported version",
        CliAvailabilityState::CapabilityMissing => "Adapter update required",
        CliAvailabilityState::ProbeFailed => "Connection check failed",
    };
    let mut card = div()
        .debug_selector(move || selector.clone())
        .w_full()
        .min_w_0()
        .p_3()
        .flex()
        .flex_col()
        .gap_2()
        .rounded_md()
        .border_1()
        .border_color(theme::border())
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
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(cli.backend.label()),
                )
                .child(div().text_xs().text_color(color).child(status)),
        );
    if let Some(version) = &cli.version {
        card = card.child(
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child(version.clone()),
        );
    }
    if let Some(diagnostic) = &cli.diagnostic {
        card = card.child(
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child(diagnostic.clone()),
        );
    }
    if cli.state == CliAvailabilityState::Missing {
        card = card.child(
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child(cli.install_command),
        );
    } else if cli.state == CliAvailabilityState::AuthRequired {
        card = card.child(
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child("Sign in through this CLI, then refresh its connection here."),
        );
    }
    let quota = overview.backend(cli.backend).or(connection.quota.as_ref());
    match quota {
        Some(quota) => {
            if let Some(disclosure) = &quota.disclosure {
                card = card.child(quota_bar::disclosure_banner(disclosure, cx));
            }
            if cli.state == CliAvailabilityState::Available {
                card = card.child(quota_bar::windows_block(quota, "settings", Utc::now()));
            }
            card = card.child(sources(quota));
            if let Some(switch) = endpoint_switch(quota, overview, cx) {
                card = card.child(switch);
            }
        }
        None if cli.state == CliAvailabilityState::Available => {
            card = card.child(
                div()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child("This CLI does not report plan usage."),
            );
        }
        None => {}
    }
    card.into_any_element()
}

/// "Usage sources: Codex app-server (in use) · Codex session logs · …", in priority order.
fn sources(quota: &BackendQuota) -> AnyElement {
    let list = quota
        .sources
        .iter()
        .map(|source| {
            let state = if source.active {
                " (in use)"
            } else if !source.enabled {
                " (off)"
            } else {
                ""
            };
            format!("{}{state}", source.source.label())
        })
        .collect::<Vec<_>>()
        .join(" · ");
    div()
        .text_xs()
        .text_color(theme::text_subtle())
        .child(format!("Usage sources, in order: {list}"))
        .into_any_element()
}

/// The per-CLI switch of the collector that reads the CLI's sign-in (G7).
fn endpoint_switch(
    quota: &BackendQuota,
    overview: &QuotaOverview,
    cx: &mut Context<AppModel>,
) -> Option<AnyElement> {
    let on = quota.endpoint_switch?;
    let backend = quota.backend;
    let (id, host): (&'static str, &'static str) = match backend {
        CliBackend::Codex => ("quota-endpoint-codex", "chatgpt.com"),
        CliBackend::Claude => ("quota-endpoint-claude", "api.anthropic.com"),
        _ => return None,
    };
    let enabled = overview.credential_collectors_off.is_none();
    Some(
        div()
            .w_full()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(format!(
                        "Read {}'s sign-in to ask {host} for plan usage",
                        backend.label()
                    )),
            )
            .child(crate::settings_ui::switch(
                id,
                on && enabled,
                enabled,
                cx,
                move |model, cx| model.set_quota_endpoint(backend, !on, cx),
            ))
            .into_any_element(),
    )
}
