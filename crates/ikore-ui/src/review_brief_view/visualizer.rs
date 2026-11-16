//! The Visualizer's note on the Brief (ADR-027 M5): an agent-written headline
//! and short notes, each with the references it cites, labelled as the
//! agent's. The host validated every reference; layout and colour are the
//! host's. While the note is being written, or when it was omitted, one quiet
//! line says so: the brief below is complete without it.

use gpui::{AnyElement, SharedString, div, prelude::*, px};
use ikore_core::orchestration::review_brief::{CheckpointVisualization, VisualizationState};

use super::{Provenance, card, card_header, pill};
use crate::icons::Icon;
use crate::theme;
use crate::widgets::icon;

pub(crate) fn visualizer_card(visualization: &CheckpointVisualization) -> AnyElement {
    match (visualization.state, visualization.note.as_ref()) {
        (VisualizationState::Validated, Some(note)) => card("brief-visualizer")
            .w_full()
            .border_color(theme::purple_soft())
            .child(card_header(
                Icon::Sparkles,
                theme::purple(),
                "Visualizer note",
                None,
                Provenance::Agent,
            ))
            .child(
                div()
                    .debug_selector(|| "brief-visualizer-headline".into())
                    .min_w_0()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(SharedString::from(note.headline.clone())),
            )
            .children(note.notes.iter().enumerate().map(|(index, line)| {
                div()
                    .debug_selector(move || format!("brief-visualizer-note-{index}"))
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .pl_3()
                    .border_l_2()
                    .border_color(theme::purple_soft())
                    .child(
                        div()
                            .min_w_0()
                            .text_sm()
                            .child(SharedString::from(line.text.clone())),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_1()
                            .children(line.refs.iter().map(|reference| pill(reference.clone()))),
                    )
            }))
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child("Written by the Visualizer from this brief. It cannot add, remove or change any item, verdict or check."),
            )
            .into_any_element(),
        (VisualizationState::Pending, _) => quiet_line(
            "brief-visualizer-pending",
            Icon::Clock,
            "The Visualizer is writing a note on this checkpoint. The brief below is complete without it.".into(),
        ),
        (VisualizationState::Omitted, _) | (VisualizationState::Validated, None) => quiet_line(
            "brief-visualizer-omitted",
            Icon::CircleHelp,
            format!(
                "Visualizer note omitted. {}",
                visualization
                    .diagnostic
                    .as_deref()
                    .unwrap_or("Its answer could not be validated.")
            ),
        ),
    }
}

fn quiet_line(selector: &'static str, glyph: Icon, text: String) -> AnyElement {
    div()
        .debug_selector(move || selector.into())
        .min_w_0()
        .flex()
        .items_start()
        .gap_2()
        .text_xs()
        .text_color(theme::text_muted())
        .child(
            div()
                .flex_shrink_0()
                .pt(px(1.0))
                .child(icon(glyph, px(14.0), theme::purple())),
        )
        .child(div().flex_1().min_w_0().child(SharedString::from(text)))
        .into_any_element()
}
