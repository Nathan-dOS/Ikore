//! The compact Layers card of a checkpoint brief (ADR-035 M2): the four
//! states and headlines the Layers panel shows, without the facts. The
//! Specification's Scope is its "What approving grants" card instead, and the
//! Integrator's Intent points at "Risk and coverage" rather than repeat it.

use gpui::{AnyElement, div, prelude::*, px};
use hvc_core::orchestration::layers::{LayerKind, PulsationLayersSnapshot};
use hvc_core::persistence_service::PulsationStage;

use super::stages::{detail, wide_card};
use super::{Provenance, card_header};
use crate::icons::Icon;
use crate::layers_view::{CLEAR_MEANING, NAME_WIDTH, state_badge};
use crate::theme;

/// The card of a Contract, Test or Integrator review; `None` elsewhere.
pub(crate) fn layers_card(
    layers: &PulsationLayersSnapshot,
    stage: PulsationStage,
) -> Option<AnyElement> {
    if !matches!(
        stage,
        PulsationStage::Contract | PulsationStage::Test | PulsationStage::Integrator
    ) {
        return None;
    }
    Some(
        wide_card("review-layers")
            .child(card_header(
                Icon::Layers,
                theme::accent(),
                "Layers",
                None,
                Provenance::Host,
            ))
            .children(layers.layers.iter().map(|layer| {
                let selector = format!("review-layers-row-{}", layer.kind.as_str());
                let headline =
                    if stage == PulsationStage::Integrator && layer.kind == LayerKind::Intent {
                        format!("{} · details in Risk and coverage", layer.headline)
                    } else {
                        layer.headline.clone()
                    };
                div()
                    .debug_selector(move || selector.clone())
                    .min_w_0()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(NAME_WIDTH))
                            .flex_shrink_0()
                            .text_sm()
                            .child(layer.kind.title()),
                    )
                    .child(state_badge(layer.state))
                    .child(detail(headline))
            }))
            .child(detail(CLEAR_MEANING))
            .into_any_element(),
    )
}
