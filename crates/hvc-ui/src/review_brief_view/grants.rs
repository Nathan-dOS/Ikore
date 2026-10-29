//! "What approving grants" (ADR-035 M1): the mandate an Approve of the waiting
//! Specification issues, derived by the host from the frozen batch, the
//! clarifications and the payload under review. The host issues these exact
//! bytes when the developer approves; this module only draws them.

use gpui::{AnyElement, SharedString, div, prelude::*};
use hvc_core::orchestration::mandate::{ExecutionMandateV1, WriteClass, WriteGrant};
use hvc_core::persistence_service::MandatePreview;

use super::stages::{badge, detail, wide_card};
use super::{Provenance, card_header, mono, pill};
use crate::icons::Icon;
use crate::theme;

/// Paths drawn before the rest is summarized; the record keeps all of them.
const MAX_PATHS: usize = 64;

fn origin_word(origin: WriteClass) -> &'static str {
    match origin {
        WriteClass::BriefSource => "brief source",
        WriteClass::TaskPath => "task",
        WriteClass::ClarificationPath => "clarification",
        WriteClass::SpecificationReference => "specification",
        WriteClass::StageDocument => "stage note",
        WriteClass::TestDestination => "test file",
        WriteClass::Amendment => "granted",
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

fn is_sensitive(preview: &MandatePreview, grant: &WriteGrant) -> bool {
    let path = grant.path.replace('\\', "/");
    preview
        .sensitive_paths
        .iter()
        .any(|sensitive| sensitive.eq_ignore_ascii_case(&path))
}

/// A text that takes what its row leaves and wraps inside it. A text with only
/// `min_w_0` beside other elements collapses to zero width in GPUI.
fn stretch(selector: String, child: AnyElement) -> AnyElement {
    div()
        .debug_selector(move || selector.clone())
        .flex_1()
        .min_w_0()
        .child(child)
        .into_any_element()
}

fn transports_line(mandate: &ExecutionMandateV1) -> Vec<AnyElement> {
    mandate
        .capabilities
        .transports
        .iter()
        .enumerate()
        .map(|(index, transport)| {
            let target = match &transport.backend {
                Some(backend) => format!("{} · {backend}", transport.transport),
                None => transport.transport.clone(),
            };
            div()
                .flex()
                .items_start()
                .gap_2()
                .child(div().flex_shrink_0().child(mono(transport.role.clone())))
                .child(stretch(
                    format!("review-grants-transport-{index}"),
                    detail(target),
                ))
                .into_any_element()
        })
        .collect()
}

fn budget_line(mandate: &ExecutionMandateV1) -> String {
    let limits = mandate
        .budget
        .stage_output_limits
        .iter()
        .map(|limit| format!("{} {}", limit.stage, limit.output_tokens))
        .collect::<Vec<_>>()
        .join(" · ");
    format!(
        "Output tokens per stage: {limits}. Context {} characters, up to {} extra invocation(s) per stage. Changing these settings during the pulsation applies to the next one.",
        mandate.budget.context_chars_limit, mandate.budget.extra_invocation_limit
    )
}

/// The grants card of a Specification checkpoint in pipeline v4.
pub(crate) fn grants_panel(preview: &MandatePreview) -> Option<AnyElement> {
    let mandate = &preview.mandate;
    if !mandate.enforcement.is_enforced() {
        return None;
    }
    let grants = &mandate.scope.grants;
    let sensitive = grants
        .iter()
        .filter(|grant| is_sensitive(preview, grant))
        .count();
    let hidden = grants.len().saturating_sub(MAX_PATHS);
    let checks = &mandate.capabilities.checks;
    Some(
        wide_card("review-grants")
            .child(card_header(
                Icon::ShieldCheck,
                theme::accent(),
                "What approving grants",
                Some(grants.len()),
                Provenance::Host,
            ))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap_2()
                    .child(stretch(
                        "review-grants-summary".into(),
                        detail(format!(
                            "Approving issues mandate revision {}: stages after this one may write only these {} path(s).",
                            mandate.revision,
                            grants.len()
                        )),
                    ))
                    .when(sensitive > 0, |row| {
                        row.child(
                            div()
                                .debug_selector(|| "review-grants-sensitive".into())
                                .flex_shrink_0()
                                .child(badge(
                                    Icon::TriangleAlert,
                                    theme::warning(),
                                    format!("{sensitive} sensitive"),
                                )),
                        )
                    }),
            )
            .child(heading("Writes"))
            .children(grants.iter().take(MAX_PATHS).enumerate().map(|(index, grant)| {
                let selector = format!("review-grants-path-{index}");
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .min_w_0()
                    .flex()
                    .items_start()
                    .gap_2()
                    .child(stretch(
                        format!("review-grants-path-text-{index}"),
                        mono(grant.path.clone()),
                    ))
                    .children(grant.origins.iter().map(|origin| pill(origin_word(*origin))))
                    .when(is_sensitive(preview, grant), |row| {
                        row.child(badge(Icon::TriangleAlert, theme::warning(), "sensitive"))
                    })
            }))
            .when(hidden > 0, |card| {
                card.child(
                    div()
                        .debug_selector(|| "review-grants-more".into())
                        .child(detail(format!("{hidden} more path(s) in the mandate."))),
                )
            })
            .child(heading("Agents"))
            .children(transports_line(mandate))
            .child(heading("Checks"))
            .when(checks.is_empty(), |card| {
                card.child(detail(
                    "No validation profile is enabled; no host check will run.",
                ))
            })
            .children(checks.iter().enumerate().map(|(index, check)| {
                div()
                    .flex()
                    .items_start()
                    .gap_2()
                    .child(stretch(
                        format!("review-grants-check-{index}"),
                        mono(check.check_ref.clone()),
                    ))
                    .child(pill(format!("{:?}", check.kind).to_lowercase()))
                    .child(div().flex_shrink_0().child(detail(if check.required {
                        "required"
                    } else {
                        "optional"
                    })))
                    .into_any_element()
            }))
            .child(heading("Budget"))
            .child(detail(budget_line(mandate)))
            .child(
                div()
                    .debug_selector(|| "review-grants-digest".into())
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(format!(
                        "Mandate {}",
                        preview.mandate_sha256.chars().take(12).collect::<String>()
                    )),
            )
            .into_any_element(),
    )
}
