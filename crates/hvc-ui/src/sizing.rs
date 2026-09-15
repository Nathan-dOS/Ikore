//! Width discipline for controls.
//!
//! A button or a single-line input is only as wide as what the user has to
//! read in it. Left alone, a child of a flex column stretches to the panel
//! (`align-items: stretch`) and `NativeTextInput` fills its parent, which is
//! how a "Filter" menu or an API-key field ended up spanning the whole window.
//! Every shared control helper calls [`StyledExt::self_start`] so it sizes to
//! its content, and every single-line input goes through [`sized_input`] with
//! one of the three widths below; multiline fields use [`text_area`].
use gpui::{AlignSelf, Div, Entity, Pixels, Styled, div, prelude::*, px};

use crate::text_input::NativeTextInput;

/// Numbers and short codes: a threshold, a budget, a wave capacity.
pub(crate) const INPUT_SHORT: Pixels = px(160.0);
/// Names and identifiers: a feature or route name, a model id, a note name.
pub(crate) const INPUT_MEDIUM: Pixels = px(320.0);
/// Paths, URLs, secrets and JSON arguments.
pub(crate) const INPUT_LONG: Pixels = px(560.0);
/// A multiline field stays readable below this width; the Chat composer is
/// the one exception and keeps its column.
pub(crate) const TEXTAREA_MAX: Pixels = px(720.0);

pub(crate) trait StyledExt: Styled + Sized {
    /// Sizes the element to its content inside a flex column instead of
    /// stretching it across the container (`align-self: flex-start`).
    fn self_start(mut self) -> Self {
        self.style().align_self = Some(AlignSelf::FlexStart);
        self
    }
}

impl<T: Styled> StyledExt for T {}

/// A single-line input at one of the scale widths, never wider than its panel.
/// Layout tests find these wrappers as `sized-input:<width>`.
pub(crate) fn sized_input(input: Entity<NativeTextInput>, width: Pixels) -> Div {
    div()
        .debug_selector(move || format!("sized-input:{}", f32::from(width)))
        .w(width)
        .max_w_full()
        .flex_shrink_0()
        .child(input)
}

/// A multiline input that fills its column up to [`TEXTAREA_MAX`].
pub(crate) fn text_area(input: Entity<NativeTextInput>) -> Div {
    div()
        .debug_selector(|| "text-area".into())
        .w_full()
        .max_w(TEXTAREA_MAX)
        .child(input)
}
