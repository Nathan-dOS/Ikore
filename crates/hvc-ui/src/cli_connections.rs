//! Global CLI login status and provider-reported plan windows in Settings.

use chrono::{DateTime, Utc};
use gpui::{AnyElement, Context, IntoElement, SharedString, div, prelude::*, px, relative};
use hvc_core::terminals::{CliAvailabilityState, CliConnectionStatus, CliQuotaBucket};

use crate::app_model::AppModel;
use crate::settings_search::entry_by_id;
use crate::settings_ui::{note, stacked_row};
use crate::{settings_view, theme};

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
        if snapshot.connections.is_empty() {
            body = body.child(note("No CLI connection information is available."));
        }
        for connection in &snapshot.connections {
            body = body.child(connection_card(connection));
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

fn connection_card(connection: &CliConnectionStatus) -> AnyElement {
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
    let mut identity = Vec::new();
    if let Some(version) = &cli.version {
        identity.push(version.clone());
    }
    if let Some(plan) = &connection.plan {
        identity.push(format!("Plan: {plan}"));
    }
    if let Some(source) = connection.quota_source {
        identity.push(format!("Usage: {source}"));
    }
    if !identity.is_empty() {
        card = card.child(
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child(identity.join(" · ")),
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
    if cli.state == CliAvailabilityState::Available {
        for (index, bucket) in cli.quota.iter().enumerate() {
            card = card.child(quota_window(
                cli.backend.as_str(),
                index,
                bucket,
                Utc::now(),
            ));
        }
        if let Some(diagnostic) = &connection.quota_diagnostic {
            card = card.child(
                div()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(diagnostic.clone()),
            );
        }
    }
    card.into_any_element()
}

fn expired(bucket: &CliQuotaBucket, now: DateTime<Utc>) -> bool {
    bucket
        .reset_time
        .as_ref()
        .is_some_and(|reset| DateTime::parse_from_rfc3339(reset).map_or(true, |reset| reset <= now))
}

fn quota_window(
    backend: &str,
    index: usize,
    bucket: &CliQuotaBucket,
    now: DateTime<Utc>,
) -> AnyElement {
    let selector = format!("settings-cli-quota:{backend}:{index}");
    let stale = expired(bucket, now);
    let used = 100u8.saturating_sub(bucket.remaining_percent);
    let color = if stale {
        theme::text_muted()
    } else if used >= 90 {
        theme::danger()
    } else if used >= 70 {
        theme::warning()
    } else {
        theme::accent()
    };
    let window = match bucket.window.as_str() {
        "weekly" => "Weekly limit".to_string(),
        window => format!("{window} limit"),
    };
    let reset = bucket
        .reset_time
        .as_ref()
        .map(|reset| {
            format!(
                "{} {}",
                if stale { "Previous reset:" } else { "Resets:" },
                crate::time::format_timestamp(reset)
            )
        })
        .unwrap_or_else(|| "Reset time not reported".into());
    div()
        .debug_selector(move || selector.clone())
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .flex_wrap()
                .justify_between()
                .gap_2()
                .text_xs()
                .child(format!("{} · {window}", bucket.group))
                .child(div().text_color(color).child(if stale {
                    "Awaiting refresh".to_string()
                } else if used == 100 {
                    "100% used · limit reached".to_string()
                } else {
                    format!("{used}% used · {}% remaining", bucket.remaining_percent)
                })),
        )
        .when(!stale, |row| {
            let bar_selector = format!("settings-cli-quota-bar:{backend}:{index}");
            row.child(
                div()
                    .debug_selector(move || bar_selector.clone())
                    .w_full()
                    .h(px(5.0))
                    .rounded_full()
                    .bg(theme::border())
                    .overflow_hidden()
                    .child(
                        div()
                            .w(relative(f32::from(used) / 100.0))
                            .h_full()
                            .bg(color),
                    ),
            )
        })
        .child(
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child(SharedString::from(reset)),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_windows_past_their_reset_are_never_shown_as_current_usage() {
        let now = DateTime::parse_from_rfc3339("2026-10-17T12:00:00Z")
            .unwrap()
            .to_utc();
        let mut bucket = CliQuotaBucket {
            group: "Codex".into(),
            window: "5h".into(),
            remaining_percent: 0,
            reset_time: Some("2026-10-17T11:59:59Z".into()),
        };
        assert!(expired(&bucket, now));
        bucket.reset_time = Some("2026-10-17T12:00:00Z".into());
        assert!(expired(&bucket, now));
        bucket.reset_time = Some("2026-10-17T13:00:00Z".into());
        assert!(!expired(&bucket, now));
        bucket.reset_time = Some("invalid".into());
        assert!(expired(&bucket, now));
        bucket.reset_time = None;
        assert!(!expired(&bucket, now));
    }
}
