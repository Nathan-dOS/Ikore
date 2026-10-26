//! The Layers panel (ADR-035 M2): four rows on top of the pulsation detail,
//! one per trust layer, each with the host's state and headline. A row opens
//! to the rules that fired and the facts behind them. The host derived every
//! state; this module only draws it, and nothing here decides (SC-021).

use gpui::{AnyElement, Context, SharedString, div, prelude::*, px};
use hvc_core::orchestration::layers::{LayerItemV1, LayerKind, LayerState, LayerV1};

use crate::app_model::AppModel;
use crate::icons::Icon;
use crate::theme;
use crate::widgets::icon;

/// What `clear` promises, shown wherever the states are.
pub(crate) const CLEAR_MEANING: &str = "Clear means the host detected no problem, not that the change is safe. These layers only report; decisions stay at the checkpoints.";

pub(crate) fn state_style(state: LayerState) -> (Icon, gpui::Rgba) {
    match state {
        LayerState::Clear => (Icon::Check, theme::success()),
        LayerState::Attention => (Icon::TriangleAlert, theme::warning()),
        LayerState::Issue => (Icon::Ban, theme::danger()),
        LayerState::NotApplicable => (Icon::Minus, theme::text_muted()),
        LayerState::Unknown => (Icon::CircleHelp, theme::text_muted()),
    }
}

/// Icon, word and colour together, so colour is never the only signal.
pub(crate) fn state_badge(state: LayerState) -> AnyElement {
    let (glyph, color) = state_style(state);
    div()
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap_1()
        .child(icon(glyph, px(12.0), color))
        .child(div().text_xs().text_color(color).child(state.label()))
        .into_any_element()
}

/// Width of the layer name column, so the badges line up.
pub(crate) const NAME_WIDTH: f32 = 84.0;

pub(crate) fn layers_panel(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let state = model.pulsations.read(cx);
    let Some(layers) = state.detail().and_then(|detail| detail.layers.clone()) else {
        return div().into_any_element();
    };
    let expanded = state.layers_expanded.clone();
    div()
        .mb_3()
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(theme::border())
        .min_w_0()
        .debug_selector(|| "layers-panel".into())
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(icon(Icon::Layers, px(14.0), theme::accent()))
                .child(div().text_sm().child("Layers"))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child("derived by the host"),
                ),
        )
        .children(
            layers
                .layers
                .iter()
                .map(|layer| layer_row(layer, expanded.contains(&layer.kind), cx)),
        )
        .child(
            div()
                .mt_2()
                .text_xs()
                .text_color(theme::text_muted())
                .child(CLEAR_MEANING),
        )
        .into_any_element()
}

fn layer_row(layer: &LayerV1, open: bool, cx: &mut Context<AppModel>) -> AnyElement {
    let kind = layer.kind;
    let selector = format!("layers-row-{}", kind.as_str());
    div()
        .mt_1()
        .min_w_0()
        .rounded_md()
        .bg(theme::surface_raised())
        .child(
            div()
                .id(SharedString::from(selector.clone()))
                .debug_selector(move || selector.clone())
                .min_w_0()
                .flex()
                .items_center()
                .gap_2()
                .p_2()
                .cursor_pointer()
                .on_click(cx.listener(move |model, _, _, cx| {
                    model.pulsations.update(cx, |state, _| {
                        if !state.layers_expanded.remove(&kind) {
                            state.layers_expanded.insert(kind);
                        }
                    });
                    cx.notify();
                }))
                .child(icon(
                    if open {
                        Icon::ChevronDown
                    } else {
                        Icon::ChevronRight
                    },
                    px(12.0),
                    theme::text_muted(),
                ))
                .child(
                    div()
                        .w(px(NAME_WIDTH))
                        .flex_shrink_0()
                        .text_sm()
                        .child(kind.title()),
                )
                .child(state_badge(layer.state))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(layer.headline.clone()),
                ),
        )
        .when(open, |row| row.child(layer_details(layer)))
        .into_any_element()
}

fn layer_details(layer: &LayerV1) -> AnyElement {
    let kind = layer.kind;
    let (_, color) = state_style(layer.state);
    div()
        .debug_selector(move || format!("layers-items-{}", kind.as_str()))
        .min_w_0()
        .px_2()
        .pb_2()
        .flex()
        .flex_col()
        .gap_1()
        .children(
            layer
                .reasons
                .iter()
                .map(|reason| div().text_xs().text_color(color).child(reason.clone())),
        )
        .children(
            layer
                .items
                .iter()
                .enumerate()
                .map(|(index, item)| item_row(kind, index, item)),
        )
        .when(layer.truncated_count > 0, |list| {
            list.child(
                div()
                    .debug_selector(move || format!("layers-more-{}", kind.as_str()))
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(format!("{} more", layer.truncated_count)),
            )
        })
        .into_any_element()
}

fn item_row(kind: LayerKind, index: usize, item: &LayerItemV1) -> AnyElement {
    let color = if item.state.is_flagged() {
        state_style(item.state).1
    } else {
        theme::text_muted()
    };
    div()
        .debug_selector(move || format!("layers-item-{}-{index}", kind.as_str()))
        .min_w_0()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_2()
        .child(
            div()
                .min_w_0()
                .font_family(theme::FONT_MONO)
                .text_xs()
                .child(item.label.clone()),
        )
        .child(
            div()
                .min_w_0()
                .text_xs()
                .text_color(color)
                .child(item.value.clone()),
        )
        .into_any_element()
}
