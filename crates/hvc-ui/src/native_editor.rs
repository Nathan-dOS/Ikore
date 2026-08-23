use std::ops::Range;
use std::time::Duration;

use gpui::{
    App, Bounds, ClipboardItem, Context, Element, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, FocusHandle, Focusable, GlobalElementId, InspectorElementId, KeyBinding,
    LayoutId, MouseButton, MouseDownEvent, PaintQuad, Pixels, Point, ShapedLine, Style, TextRun,
    Timer, UTF16Selection, Window, actions, div, fill, point, prelude::*, px, relative, rgba, size,
};
use hvc_editor::EditorBuffer;

use crate::theme;

actions!(
    native_editor,
    [Backspace, Delete, Undo, Redo, SelectAll, Copy, Cut, Paste]
);

pub fn register_editor_keybindings(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, Some("NativeEditor")),
        KeyBinding::new("delete", Delete, Some("NativeEditor")),
        KeyBinding::new("ctrl-z", Undo, Some("NativeEditor")),
        KeyBinding::new("ctrl-y", Redo, Some("NativeEditor")),
        KeyBinding::new("ctrl-a", SelectAll, Some("NativeEditor")),
        KeyBinding::new("ctrl-c", Copy, Some("NativeEditor")),
        KeyBinding::new("ctrl-x", Cut, Some("NativeEditor")),
        KeyBinding::new("ctrl-v", Paste, Some("NativeEditor")),
    ]);
}

pub struct NativeEditor {
    pub buffer: EditorBuffer,
    focus_handle: FocusHandle,
    marked_range: Option<Range<usize>>,
    last_bounds: Option<Bounds<Pixels>>,
    autosave_generation: u64,
    pub autosave_ready: bool,
}

impl NativeEditor {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            buffer: EditorBuffer::new("", "new"),
            focus_handle: cx.focus_handle(),
            marked_range: None,
            last_bounds: None,
            autosave_generation: 0,
            autosave_ready: false,
        }
    }

    fn backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        let _ = self.buffer.backspace();
        self.after_edit(cx);
    }

    fn delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) {
        let _ = self.buffer.delete_forward();
        self.after_edit(cx);
    }

    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        if self.buffer.undo() {
            self.after_edit(cx);
        }
    }

    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        if self.buffer.redo() {
            self.after_edit(cx);
        }
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        let _ = self.buffer.set_selection_utf16(0, self.buffer.utf16_len());
        cx.notify();
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        let range = self.selection_utf16();
        if !range.is_empty()
            && let Ok(text) = self.buffer.text_for_utf16_range(range)
        {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        self.copy(&Copy, window, cx);
        if !self.buffer.selection().is_empty() {
            let _ = self.buffer.replace_selection("");
            self.after_edit(cx);
        }
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let _ = self.buffer.replace_selection(&text);
            self.after_edit(cx);
        }
    }

    fn mouse_down(&mut self, _: &MouseDownEvent, window: &mut Window, _cx: &mut Context<Self>) {
        self.focus_handle.focus(window);
    }

    fn selection_utf16(&self) -> Range<usize> {
        let (start, end) = self.buffer.selection().ordered();
        self.buffer.utf16_offset(start).unwrap_or(0)
            ..self
                .buffer
                .utf16_offset(end)
                .unwrap_or_else(|_| self.buffer.utf16_len())
    }

    fn replace_range(&mut self, range: Range<usize>, text: &str) {
        let _ = self.buffer.set_selection_utf16(range.start, range.end);
        let _ = self.buffer.replace_selection(text);
    }

    fn after_edit(&mut self, cx: &mut Context<Self>) {
        self.marked_range = None;
        self.autosave_ready = false;
        self.autosave_generation = self.autosave_generation.saturating_add(1);
        let generation = self.autosave_generation;
        cx.spawn(async move |this, cx| {
            Timer::after(Duration::from_secs(1)).await;
            let _ = this.update(cx, |editor, cx| {
                if editor.autosave_generation == generation && editor.buffer.is_dirty() {
                    editor.autosave_ready = true;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }
}

impl Focusable for NativeEditor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EntityInputHandler for NativeEditor {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let bounded =
            range.start.min(self.buffer.utf16_len())..range.end.min(self.buffer.utf16_len());
        actual_range.replace(bounded.clone());
        self.buffer.text_for_utf16_range(bounded).ok()
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let selection = self.buffer.selection();
        Some(UTF16Selection {
            range: self.selection_utf16(),
            reversed: selection.anchor > selection.head,
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
        let target = range
            .or_else(|| self.marked_range.clone())
            .unwrap_or_else(|| self.selection_utf16());
        self.replace_range(target, new_text);
        self.after_edit(cx);
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
            .unwrap_or_else(|| self.selection_utf16());
        let start = target.start;
        self.replace_range(target, new_text);
        let inserted_utf16 = new_text.encode_utf16().count();
        self.marked_range = (!new_text.is_empty()).then_some(start..start + inserted_utf16);
        if let Some(selection) = new_selection {
            let _ = self
                .buffer
                .set_selection_utf16(start + selection.start, start + selection.end);
        }
        self.after_edit(cx);
        self.marked_range = (!new_text.is_empty()).then_some(start..start + inserted_utf16);
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
        Some(self.selection_utf16().end)
    }
}

impl Render for NativeEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context("NativeEditor")
            .track_focus(&self.focus_handle(cx))
            .size_full()
            .bg(theme::canvas())
            .text_color(theme::text())
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .child(EditorElement {
                editor: cx.entity(),
            })
    }
}

struct EditorElement {
    editor: Entity<NativeEditor>,
}

struct EditorPaintState {
    lines: Vec<(ShapedLine, Pixels)>,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
}

impl IntoElement for EditorElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for EditorElement {
    type RequestLayoutState = ();
    type PrepaintState = EditorPaintState;

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
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let editor = self.editor.read(cx);
        let snapshot = editor.buffer.snapshot();
        let line_height = window.line_height();
        let visible_start = (snapshot.scroll_top / f32::from(line_height)).floor() as usize;
        let visible_count =
            (f32::from(bounds.size.height) / f32::from(line_height)).ceil() as usize + 1;
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let mut lines = Vec::new();

        for (visible_index, text) in snapshot
            .text
            .lines()
            .skip(visible_start)
            .take(visible_count)
            .enumerate()
        {
            let display = if text.is_empty() { " " } else { text };
            let run = TextRun {
                len: display.len(),
                font: style.font(),
                color: style.color,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            lines.push((
                window.text_system().shape_line(
                    display.to_string().into(),
                    font_size,
                    &[run],
                    None,
                ),
                px(visible_index as f32 * f32::from(line_height)),
            ));
        }

        let caret = snapshot.selection.head;
        let cursor = if caret.line >= visible_start && caret.line < visible_start + visible_count {
            let line_index = caret.line - visible_start;
            let x = lines
                .get(line_index)
                .map(|(line, _)| line.x_for_index(byte_offset_for_column(&line.text, caret.column)))
                .unwrap_or(px(0.0));
            Some(fill(
                Bounds::new(
                    point(bounds.left() + x, bounds.top() + line_height * line_index),
                    size(px(2.0), line_height),
                ),
                theme::accent(),
            ))
        } else {
            None
        };

        let (selection_start, selection_end) = snapshot.selection.ordered();
        let selection = if selection_start.line == selection_end.line
            && selection_start != selection_end
            && selection_start.line >= visible_start
            && selection_start.line < visible_start + visible_count
        {
            let line_index = selection_start.line - visible_start;
            lines.get(line_index).map(|(line, _)| {
                let left =
                    line.x_for_index(byte_offset_for_column(&line.text, selection_start.column));
                let right =
                    line.x_for_index(byte_offset_for_column(&line.text, selection_end.column));
                fill(
                    Bounds::new(
                        point(
                            bounds.left() + left,
                            bounds.top() + line_height * line_index,
                        ),
                        size(right - left, line_height),
                    ),
                    rgba(0x54d6a244),
                )
            })
        } else {
            None
        };

        EditorPaintState {
            lines,
            cursor,
            selection,
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
        let focus_handle = self.editor.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.editor.clone()),
            cx,
        );
        if let Some(selection) = state.selection.take() {
            window.paint_quad(selection);
        }
        for (line, y) in state.lines.drain(..) {
            let _ = line.paint(
                point(bounds.left(), bounds.top() + y),
                window.line_height(),
                window,
                cx,
            );
        }
        if focus_handle.is_focused(window)
            && let Some(cursor) = state.cursor.take()
        {
            window.paint_quad(cursor);
        }
        self.editor.update(cx, |editor, _| {
            editor.last_bounds = Some(bounds);
        });
    }
}

fn byte_offset_for_column(text: &str, column: usize) -> usize {
    text.char_indices()
        .nth(column)
        .map(|(offset, _)| offset)
        .unwrap_or(text.len())
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;

    #[gpui::test]
    fn native_editor_owns_a_rope_buffer_and_autosave_state(cx: &mut TestAppContext) {
        let editor = cx.new(NativeEditor::new);
        cx.read(|cx| {
            assert_eq!(editor.read(cx).buffer.text(), "");
            assert!(!editor.read(cx).autosave_ready);
        });
    }
}
