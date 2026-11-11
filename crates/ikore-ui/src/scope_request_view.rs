//! The "Scope request" card on a failure checkpoint (ADR-035 M4): the
//! delivered files a unit asked to change, what the host refused and why, the
//! risk with and without them and the budget a retry draws on. The host
//! derived every line; "Grant and retry" is in the decision bar, and the card
//! itself decides nothing.

use gpui::{AnyElement, SharedString, div, prelude::*, px};
use ikore_core::orchestration::composition::Risk;
use ikore_core::persistence_service::{PulsationStage, ScopeRequestView};

use crate::icons::Icon;
use crate::theme;
use crate::widgets::icon;

/// Characters of a sha the card shows; the full digest is in the tooltip-free
/// report and the receipt.
const SHORT_SHA: usize = 12;

const fn risk_word(risk: Risk) -> &'static str {
    match risk {
        Risk::Low => "low",
        Risk::Medium => "medium",
        Risk::High => "high",
        Risk::Unknown => "unknown",
    }
}

fn stage_word(stage: PulsationStage) -> &'static str {
    match stage {
        PulsationStage::Test => "Test",
        _ => "Code",
    }
}

/// The notice that opens the Review tab.
pub(crate) fn scope_request_notice(request: &ScopeRequestView) -> String {
    format!(
        "{} needs write access to {} file(s)",
        stage_word(request.stage),
        request.accepted.len()
    )
}

fn state_line(request: &ScopeRequestView) -> Option<(&'static str, gpui::Rgba)> {
    match request.state.as_str() {
        "pending" => None,
        "stale" => Some((
            "This request is out of date: a file or the plan changed since the request. Nothing was granted; choose another decision.",
            theme::danger(),
        )),
        "granted" => Some(("Granted.", theme::success())),
        "declined" => Some(("Declined.", theme::text_muted())),
        _ => Some(("Superseded by a revision.", theme::text_muted())),
    }
}

pub(crate) fn scope_request_card(request: &ScopeRequestView) -> AnyElement {
    let risk_changes = request.risk_after != request.risk_before;
    div()
        .debug_selector(|| "scope-request".into())
        .min_w_0()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded_lg()
        .border_1()
        .border_color(theme::accent())
        .bg(theme::surface())
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(icon(Icon::ShieldCheck, px(14.0), theme::accent()))
                .child(
                    div()
                        .flex_shrink_0()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("Scope request"),
                )
                .child(
                    div()
                        .debug_selector(|| "scope-request-unit".into())
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(format!(
                            "{} unit `{}`",
                            stage_word(request.stage),
                            request.unit_key
                        )),
                ),
        )
        .when_some(state_line(request), |card, (text, color)| {
            card.child(
                div()
                    .debug_selector(|| "scope-request-state".into())
                    .text_sm()
                    .text_color(color)
                    .child(text),
            )
        })
        .children(request.accepted.iter().enumerate().map(|(index, path)| {
            // The path takes what the marks leave and wraps inside it.
            div()
                .debug_selector(move || format!("scope-request-path-{index}"))
                .min_w_0()
                .flex()
                .items_start()
                .gap_2()
                .child(
                    div()
                        .debug_selector(move || format!("scope-request-path-text-{index}"))
                        .flex_1()
                        .min_w_0()
                        .font_family(theme::FONT_MONO)
                        .text_sm()
                        .child(SharedString::from(path.path.clone())),
                )
                .when(path.sensitive, |row| {
                    row.child(
                        div()
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(theme::warning())
                            .child("sensitive"),
                    )
                })
                .child(
                    div()
                        .flex_shrink_0()
                        .font_family(theme::FONT_MONO)
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(format!(
                            "sha {}",
                            path.sha256.chars().take(SHORT_SHA).collect::<String>()
                        )),
                )
        }))
        .when(!request.refused.is_empty(), |card| {
            card.child(
                div()
                    .debug_selector(|| "scope-request-refused".into())
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::text_muted())
                            .child("Not granted:"),
                    )
                    .children(request.refused.iter().map(|refusal| {
                        div()
                            .min_w_0()
                            .text_xs()
                            .text_color(theme::text_muted())
                            .child(format!(
                                "{}: {}",
                                refusal.path.as_deref().unwrap_or(refusal.handle.as_str()),
                                refusal.code.describe()
                            ))
                    })),
            )
        })
        .when_some(request.agent_message.clone(), |card, message| {
            card.child(
                div()
                    .debug_selector(|| "scope-request-message".into())
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::text_muted())
                            .child("The agent's reason (model-written, sanitized):"),
                    )
                    .child(div().min_w_0().text_sm().child(SharedString::from(message))),
            )
        })
        .child(
            div()
                .debug_selector(|| "scope-request-facts".into())
                .flex()
                .flex_wrap()
                .gap_3()
                .text_xs()
                .text_color(if risk_changes {
                    theme::warning()
                } else {
                    theme::text_muted()
                })
                .child(if risk_changes {
                    format!(
                        "Risk {} → {} with these files",
                        risk_word(request.risk_before),
                        risk_word(request.risk_after)
                    )
                } else {
                    format!("Risk stays {}", risk_word(request.risk_before))
                })
                .child(div().text_color(theme::text_muted()).child(format!(
                    "{} extra invocation(s) left in the stage budget",
                    request.remaining_extra_invocations
                ))),
        )
        .into_any_element()
}
