//! Risk and coverage of a composed review (ADR-034 M3): the level the host
//! assessed, why, and what backed each criterion and each check the level
//! required. Every state here is the host's; this module only draws them.

use gpui::{AnyElement, SharedString, div, prelude::*};
use hvc_core::orchestration::composition::Risk;
use hvc_core::orchestration::composition::risk::{CoverageRowKind, CoverageState, RiskSignal};
use hvc_core::persistence_service::CheckpointRiskStatus;

use super::stages::{badge, detail, wide_card};
use super::{Provenance, card_header, mono, pill};
use crate::icons::Icon;
use crate::theme;

fn level_word(level: Risk) -> (Icon, gpui::Rgba, &'static str) {
    match level {
        Risk::Low => (Icon::ShieldCheck, theme::success(), "Low risk"),
        Risk::Medium => (Icon::Info, theme::accent(), "Medium risk"),
        Risk::High => (Icon::TriangleAlert, theme::warning(), "High risk"),
        Risk::Unknown => (Icon::CircleHelp, theme::text_muted(), "Unknown risk"),
    }
}

fn state_word(state: CoverageState) -> (Icon, gpui::Rgba, &'static str) {
    match state {
        CoverageState::Executed => (Icon::ShieldCheck, theme::success(), "executed"),
        CoverageState::ModelOnly => (Icon::CircleHelp, theme::text_muted(), "model only"),
        CoverageState::Unverified => (Icon::TriangleAlert, theme::warning(), "unverified"),
        CoverageState::NotRun => (Icon::TriangleAlert, theme::warning(), "not run"),
    }
}

fn signal_word(signal: RiskSignal) -> &'static str {
    match signal {
        RiskSignal::IntakeTask => "Intake risk of task",
        RiskSignal::NoteRisk => "Note risk",
        RiskSignal::SensitivePath => "Sensitive path",
        RiskSignal::InterfaceChange => "Changed interface",
        RiskSignal::ChangeSize => "Change size",
    }
}

fn heading(title: &'static str) -> AnyElement {
    div()
        .pt_1()
        .text_xs()
        .text_color(theme::text_muted())
        .child(title)
        .into_any_element()
}

/// The risk card of an Integrator checkpoint, when the host assessed one.
pub(crate) fn risk_panel(status: &CheckpointRiskStatus) -> Option<AnyElement> {
    let assessment = status.assessment.as_ref()?;
    let (glyph, ink, word) = level_word(assessment.level);
    let matrix = status.matrix.as_ref();
    let rows = matrix.map(|matrix| matrix.rows.as_slice()).unwrap_or(&[]);
    let mut counts = std::collections::BTreeMap::<&'static str, usize>::new();
    for row in rows {
        *counts.entry(state_word(row.state).2).or_default() += 1;
    }
    let missing = matrix
        .map(|matrix| matrix.missing_kinds.clone())
        .unwrap_or_default();
    Some(
        wide_card("review-risk")
            .child(card_header(
                Icon::Target,
                theme::accent(),
                "Risk and coverage",
                Some(rows.len()),
                Provenance::Host,
            ))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .debug_selector(|| "review-risk-level".into())
                            .child(badge(glyph, ink, word)),
                    )
                    .children(
                        counts
                            .into_iter()
                            .map(|(state, count)| pill(format!("{count} {state}"))),
                    )
                    .child(pill(format!("policy v{}", assessment.policy_version))),
            )
            .when(!assessment.reasons.is_empty(), |card| {
                card.child(heading("Why"))
            })
            .children(assessment.reasons.iter().map(|reason| {
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(detail(signal_word(reason.signal)))
                    .child(mono(reason.subject.clone()))
            }))
            .when(!assessment.unknown_inputs.is_empty(), |card| {
                card.child(detail(format!(
                    "Unknown inputs: {}",
                    assessment.unknown_inputs.join(", ")
                )))
            })
            .when(!missing.is_empty(), |card| {
                card.child(
                    div()
                        .debug_selector(|| "review-risk-missing".into())
                        .text_xs()
                        .text_color(theme::warning())
                        .child(format!(
                            "Required at this level but not configured: {}",
                            missing.join(", ")
                        )),
                )
            })
            .when(!rows.is_empty(), |card| card.child(heading("Coverage")))
            .children(rows.iter().enumerate().map(|(index, row)| {
                let (glyph, ink, word) = state_word(row.state);
                let selector = format!("review-risk-row-{index}");
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .min_w_0()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(mono(format!(
                        "{} {}",
                        match row.kind {
                            CoverageRowKind::Criterion => "criterion",
                            CoverageRowKind::Check => "check",
                        },
                        row.subject
                    )))
                    .child(badge(glyph, ink, word))
                    .child(detail(row.detail.clone()))
            }))
            .when(status.acknowledgement_required, |card| {
                card.child(
                    div()
                        .debug_selector(|| "review-risk-acknowledgement".into())
                        .text_xs()
                        .text_color(theme::warning())
                        .child(format!(
                            "High risk with {} unverified or unrun item(s): approving needs a reason in the feedback box.",
                            matrix.map(|matrix| matrix.pending()).unwrap_or_default()
                        )),
                )
            })
            .children(status.acknowledgements.iter().map(|record| {
                detail(format!(
                    "Acknowledged {} pending item(s): {}",
                    record.pending, record.reason
                ))
            }))
            .into_any_element(),
    )
}
