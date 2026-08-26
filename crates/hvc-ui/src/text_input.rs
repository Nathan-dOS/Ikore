use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, Element, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, FocusHandle, Focusable, GlobalElementId, InspectorElementId, KeyBinding,
    LayoutId, MouseButton, Pixels, Point, ShapedLine, Style, TextRun, UTF16Selection, Window,
    actions, div, point, prelude::*, px, relative,
};

use crate::theme;

actions!(
    native_text_input,
    [
        Backspace, Delete, SelectAll, Copy, Cut, Paste, MoveLeft, MoveRight
    ]
);

pub(crate) fn register_text_input_keybindings(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, Some("NativeTextInput")),
        KeyBinding::new("delete", Delete, Some("NativeTextInput")),
        KeyBinding::new("ctrl-a", SelectAll, Some("NativeTextInput")),
        KeyBinding::new("ctrl-c", Copy, Some("NativeTextInput")),
        KeyBinding::new("ctrl-x", Cut, Some("NativeTextInput")),
        KeyBinding::new("ctrl-v", Paste, Some("NativeTextInput")),
        KeyBinding::new("left", MoveLeft, Some("NativeTextInput")),
        KeyBinding::new("right", MoveRight, Some("NativeTextInput")),
    ]);
}

pub(crate) struct NativeTextInput {
    value: String,
    selection: Range<usize>,
    marked_range: Option<Range<usize>>,
    focus_handle: FocusHandle,
    placeholder: String,
    obscured: bool,
    last_bounds: Option<Bounds<Pixels>>,
}

impl NativeTextInput {
    pub(crate) fn new(placeholder: impl Into<String>, cx: &mut Context<Self>) -> Self {
        Self {
            value: String::new(),
            selection: 0..0,
            marked_range: None,
            focus_handle: cx.focus_handle(),
            placeholder: placeholder.into(),
            obscured: false,
            last_bounds: None,
        }
    }

    pub(crate) fn obscured(mut self) -> Self {
        self.obscured = true;
        self
    }

    pub(crate) fn value(&self) -> &str {
        &self.value
    }

    pub(crate) fn set_value(&mut self, value: impl Into<String>, cx: &mut Context<Self>) {
        self.value = value.into();
        let end = self.utf16_len();
        self.selection = end..end;
        self.marked_range = None;
        cx.notify();
    }

    pub(crate) fn clear(&mut self, cx: &mut Context<Self>) {
        self.set_value(String::new(), cx);
    }

    fn utf16_len(&self) -> usize {
        self.value.encode_utf16().count()
    }

    fn bounded_range(&self, range: Range<usize>) -> Range<usize> {
        range.start.min(self.utf16_len())..range.end.min(self.utf16_len())
    }

    fn replace_range(&mut self, range: Range<usize>, text: &str) {
        let range = self.bounded_range(range);
        let start = byte_index_for_utf16(&self.value, range.start);
        let end = byte_index_for_utf16(&self.value, range.end);
        self.value.replace_range(start..end, text);
        let caret = range.start + text.encode_utf16().count();
        self.selection = caret..caret;
    }

    fn selected_text(&self) -> String {
        let range = self.bounded_range(self.selection.clone());
        let start = byte_index_for_utf16(&self.value, range.start);
        let end = byte_index_for_utf16(&self.value, range.end);
        self.value[start..end].to_owned()
    }

    fn backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        let range = if self.selection.is_empty() && self.selection.start > 0 {
            previous_utf16_boundary(&self.value, self.selection.start)..self.selection.start
        } else {
            self.selection.clone()
        };
        self.replace_range(range, "");
        cx.notify();
    }

    fn delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) {
        let range = if self.selection.is_empty() && self.selection.end < self.utf16_len() {
            self.selection.end..next_utf16_boundary(&self.value, self.selection.end)
        } else {
            self.selection.clone()
        };
        self.replace_range(range, "");
        cx.notify();
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.selection = 0..self.utf16_len();
        cx.notify();
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.obscured && !self.selection.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(self.selected_text()));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.obscured {
            self.copy(&Copy, window, cx);
        }
        if !self.selection.is_empty() {
            self.replace_range(self.selection.clone(), "");
            cx.notify();
        }
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_range(self.selection.clone(), &text.replace(['\r', '\n'], " "));
            cx.notify();
        }
    }

    fn move_left(&mut self, _: &MoveLeft, _: &mut Window, cx: &mut Context<Self>) {
        let caret = if self.selection.is_empty() {
            previous_utf16_boundary(&self.value, self.selection.start)
        } else {
            self.selection.start
        };
        self.selection = caret..caret;
        cx.notify();
    }

    fn move_right(&mut self, _: &MoveRight, _: &mut Window, cx: &mut Context<Self>) {
        let caret = if self.selection.is_empty() {
            next_utf16_boundary(&self.value, self.selection.end)
        } else {
            self.selection.end
        };
        self.selection = caret..caret;
        cx.notify();
    }
}

impl Focusable for NativeTextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EntityInputHandler for NativeTextInput {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.bounded_range(range);
        actual_range.replace(range.clone());
        let start = byte_index_for_utf16(&self.value, range.start);
        let end = byte_index_for_utf16(&self.value, range.end);
        Some(self.value[start..end].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.selection.clone(),
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range.clone()
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range
            .or_else(|| self.marked_range.clone())
            .unwrap_or_else(|| self.selection.clone());
        self.replace_range(range, &new_text.replace(['\r', '\n'], " "));
        self.marked_range = None;
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        new_selection: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = range
            .or_else(|| self.marked_range.clone())
            .unwrap_or_else(|| self.selection.clone());
        let start = target.start;
        let new_text = new_text.replace(['\r', '\n'], " ");
        self.replace_range(target, &new_text);
        let inserted = new_text.encode_utf16().count();
        self.marked_range = (!new_text.is_empty()).then_some(start..start + inserted);
        if let Some(selection) = new_selection {
            self.selection = start + selection.start..start + selection.end;
        }
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(self.last_bounds.unwrap_or(bounds))
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.selection.end)
    }
}

impl Render for NativeTextInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context("NativeTextInput")
            .track_focus(&self.focus_handle(cx))
            .h(px(36.0))
            .w_full()
            .px_3()
            .rounded_md()
            .border_1()
            .border_color(theme::border())
            .bg(theme::canvas())
            .text_color(theme::text())
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::move_left))
            .on_action(cx.listener(Self::move_right))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|input, _, window, cx| {
                    input.focus_handle.focus(window);
                    let end = input.utf16_len();
                    input.selection = end..end;
                    cx.notify();
                }),
            )
            .child(TextInputElement { input: cx.entity() })
    }
}

struct TextInputElement {
    input: Entity<NativeTextInput>,
}

struct TextInputPaintState {
    line: ShapedLine,
}

impl IntoElement for TextInputElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextInputElement {
    type RequestLayoutState = ();
    type PrepaintState = TextInputPaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = relative(1.0).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let (text, color) = if input.value.is_empty() {
            (input.placeholder.clone(), theme::text_muted())
        } else if input.obscured {
            ("•".repeat(input.value.chars().count()), theme::text())
        } else {
            (input.value.clone(), theme::text())
        };
        let style = window.text_style();
        let run = TextRun {
            len: text.len(),
            font: style.font(),
            color: color.into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        TextInputPaintState {
            line: window.text_system().shape_line(
                text.into(),
                style.font_size.to_pixels(window.rem_size()),
                &[run],
                None,
            ),
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        state: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        let _ = state.line.paint(
            point(bounds.left(), bounds.top() + px(7.0)),
            window.line_height(),
            window,
            cx,
        );
        self.input
            .update(cx, |input, _| input.last_bounds = Some(bounds));
    }
}

fn byte_index_for_utf16(text: &str, offset: usize) -> usize {
    let mut utf16 = 0;
    for (byte, character) in text.char_indices() {
        if utf16 >= offset {
            return byte;
        }
        utf16 += character.len_utf16();
    }
    text.len()
}

fn previous_utf16_boundary(text: &str, offset: usize) -> usize {
    if offset == 0 {
        return 0;
    }
    let byte = byte_index_for_utf16(text, offset);
    text[..byte]
        .chars()
        .next_back()
        .map_or(0, |character| offset.saturating_sub(character.len_utf16()))
}

fn next_utf16_boundary(text: &str, offset: usize) -> usize {
    let byte = byte_index_for_utf16(text, offset);
    text[byte..]
        .chars()
        .next()
        .map_or(offset, |character| offset + character.len_utf16())
        .min(text.encode_utf16().count())
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;

    #[gpui::test]
    fn preserves_unicode_and_ime_composition_in_single_line_fields(cx: &mut TestAppContext) {
        let input = cx.new(|cx| NativeTextInput::new("Busca", cx));
        input.update(cx, |input, cx| input.set_value("café 👋", cx));
        cx.read(|cx| {
            let input = input.read(cx);
            assert_eq!(input.value(), "café 👋");
            assert_eq!(input.selection.end, 7);
        });
    }
}
