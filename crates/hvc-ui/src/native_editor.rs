use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, Element, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, FocusHandle, Focusable, GlobalElementId, InspectorElementId, KeyBinding,
    LayoutId, MouseButton, MouseDownEvent, PaintQuad, Pixels, Point, ScrollWheelEvent, ShapedLine,
    Style, TextRun, UTF16Selection, Window, actions, div, fill, point, prelude::*, px, relative,
    rgba, size,
};
use hvc_editor::EditorBuffer;
use hvc_editor::{EditorError, EditorSnapshot, MarkdownSyntax, MarkdownSyntaxSnapshot};

use crate::theme;

actions!(
    native_editor,
    [
        Backspace,
        Delete,
        Undo,
        Redo,
        SelectAll,
        Copy,
        Cut,
        Paste,
        MoveLeft,
        MoveRight,
        MoveUp,
        MoveDown,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        LineStart,
        LineEnd,
        SelectLineStart,
        SelectLineEnd
    ]
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
        KeyBinding::new("left", MoveLeft, Some("NativeEditor")),
        KeyBinding::new("right", MoveRight, Some("NativeEditor")),
        KeyBinding::new("up", MoveUp, Some("NativeEditor")),
        KeyBinding::new("down", MoveDown, Some("NativeEditor")),
        KeyBinding::new("shift-left", SelectLeft, Some("NativeEditor")),
        KeyBinding::new("shift-right", SelectRight, Some("NativeEditor")),
        KeyBinding::new("shift-up", SelectUp, Some("NativeEditor")),
        KeyBinding::new("shift-down", SelectDown, Some("NativeEditor")),
        KeyBinding::new("home", LineStart, Some("NativeEditor")),
        KeyBinding::new("end", LineEnd, Some("NativeEditor")),
        KeyBinding::new("shift-home", SelectLineStart, Some("NativeEditor")),
        KeyBinding::new("shift-end", SelectLineEnd, Some("NativeEditor")),
    ]);
}

pub struct NativeEditor {
    pub buffer: EditorBuffer,
    focus_handle: FocusHandle,
    marked_range: Option<Range<usize>>,
    last_bounds: Option<Bounds<Pixels>>,
    autosave_generation: u64,
    syntax: Option<MarkdownSyntax>,
}

impl NativeEditor {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            buffer: EditorBuffer::new("", "new"),
            focus_handle: cx.focus_handle(),
            marked_range: None,
            last_bounds: None,
            autosave_generation: 0,
            syntax: None,
        }
    }

    pub fn restore_session_snapshot(
        &mut self,
        snapshot: EditorSnapshot,
    ) -> Result<(), EditorError> {
        self.buffer.restore_snapshot(snapshot)?;
        self.marked_range = None;
        self.autosave_generation = 0;
        Ok(())
    }

    pub fn restore_workspace_snapshot(
        &mut self,
        relative_path: &str,
        snapshot: EditorSnapshot,
    ) -> Result<(), EditorError> {
        self.restore_session_snapshot(snapshot)?;
        self.syntax = if relative_path.to_ascii_lowercase().ends_with(".md") {
            MarkdownSyntax::new(self.buffer.text()).ok()
        } else {
            None
        };
        Ok(())
    }

    pub fn syntax_snapshot(&self) -> Option<MarkdownSyntaxSnapshot> {
        self.syntax.as_ref().and_then(MarkdownSyntax::snapshot)
    }

    pub fn edit_generation(&self) -> u64 {
        self.autosave_generation
    }

    #[cfg(test)]
    pub(crate) fn insert_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.buffer.insert(text).unwrap();
        self.after_edit(cx);
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

    fn move_left(&mut self, _: &MoveLeft, window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_left(false);
        self.after_cursor_move(window, cx);
    }

    fn move_right(&mut self, _: &MoveRight, window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_right(false);
        self.after_cursor_move(window, cx);
    }

    fn move_up(&mut self, _: &MoveUp, window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_up(false);
        self.after_cursor_move(window, cx);
    }

    fn move_down(&mut self, _: &MoveDown, window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_down(false);
        self.after_cursor_move(window, cx);
    }

    fn select_left(&mut self, _: &SelectLeft, window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_left(true);
        self.after_cursor_move(window, cx);
    }

    fn select_right(&mut self, _: &SelectRight, window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_right(true);
        self.after_cursor_move(window, cx);
    }

    fn select_up(&mut self, _: &SelectUp, window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_up(true);
        self.after_cursor_move(window, cx);
    }

    fn select_down(&mut self, _: &SelectDown, window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_down(true);
        self.after_cursor_move(window, cx);
    }

    fn line_start(&mut self, _: &LineStart, window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_to_line_start(false);
        self.after_cursor_move(window, cx);
    }

    fn line_end(&mut self, _: &LineEnd, window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_to_line_end(false);
        self.after_cursor_move(window, cx);
    }

    fn select_line_start(
        &mut self,
        _: &SelectLineStart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.buffer.move_to_line_start(true);
        self.after_cursor_move(window, cx);
    }

    fn select_line_end(&mut self, _: &SelectLineEnd, window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_to_line_end(true);
        self.after_cursor_move(window, cx);
    }

    fn after_cursor_move(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let line_height = f32::from(window.line_height()).max(1.0);
        let viewport_height = self
            .last_bounds
            .map(|bounds| f32::from(bounds.size.height))
            .unwrap_or(line_height);
        let cursor_top = self.buffer.selection().head.line as f32 * line_height;
        let scroll_top = self.buffer.scroll_top();
        if cursor_top < scroll_top {
            self.buffer.set_scroll_top(cursor_top);
        } else if cursor_top + line_height > scroll_top + viewport_height {
            self.buffer
                .set_scroll_top(cursor_top + line_height - viewport_height);
        }
        cx.notify();
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_handle.focus(window);
        if let Some(bounds) = self.last_bounds {
            let line_height = f32::from(window.line_height()).max(1.0);
            let snapshot = self.buffer.snapshot();
            let line = ((snapshot.scroll_top + f32::from(event.position.y - bounds.top()))
                / line_height)
                .floor()
                .max(0.0) as usize;
            let line = line.min(snapshot.text.lines().count().saturating_sub(1));
            let text = snapshot.text.lines().nth(line).unwrap_or_default();
            let approximate_character_width = f32::from(window.rem_size()) * 0.52;
            let column = (f32::from(event.position.x - bounds.left())
                / approximate_character_width.max(1.0))
            .round()
            .max(0.0) as usize;
            let column = column.min(text.chars().count());
            let _ = self
                .buffer
                .set_selection(hvc_editor::EditorSelection::caret(
                    hvc_editor::TextPosition { line, column },
                ));
        }
        cx.notify();
    }

    fn scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let delta = event.delta.pixel_delta(window.line_height());
        let viewport_height = self
            .last_bounds
            .map(|bounds| f32::from(bounds.size.height))
            .unwrap_or_default();
        let document_height = self.buffer.line_count() as f32 * f32::from(window.line_height());
        let maximum = (document_height - viewport_height).max(0.0);
        let next = (self.buffer.scroll_top() - f32::from(delta.y)).clamp(0.0, maximum);
        if (next - self.buffer.scroll_top()).abs() > f32::EPSILON {
            self.buffer.set_scroll_top(next);
            cx.notify();
        }
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
        self.autosave_generation = self.autosave_generation.saturating_add(1);
        if let Some(syntax) = self.syntax.as_mut() {
            syntax.update(self.buffer.text());
        }
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
            .on_action(cx.listener(Self::move_left))
            .on_action(cx.listener(Self::move_right))
            .on_action(cx.listener(Self::move_up))
            .on_action(cx.listener(Self::move_down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::line_start))
            .on_action(cx.listener(Self::line_end))
            .on_action(cx.listener(Self::select_line_start))
            .on_action(cx.listener(Self::select_line_end))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_scroll_wheel(cx.listener(Self::scroll_wheel))
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
    selections: Vec<PaintQuad>,
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
        let mut selections = Vec::new();
        if selection_start != selection_end {
            for line_number in selection_start.line..=selection_end.line {
                if line_number < visible_start || line_number >= visible_start + visible_count {
                    continue;
                }
                let line_index = line_number - visible_start;
                let Some((line, _)) = lines.get(line_index) else {
                    continue;
                };
                let start_column = if line_number == selection_start.line {
                    selection_start.column
                } else {
                    0
                };
                let end_column = if line_number == selection_end.line {
                    selection_end.column
                } else {
                    line.text.chars().count()
                };
                let left = line.x_for_index(byte_offset_for_column(&line.text, start_column));
                let right = line.x_for_index(byte_offset_for_column(&line.text, end_column));
                selections.push(fill(
                    Bounds::new(
                        point(
                            bounds.left() + left,
                            bounds.top() + line_height * line_index,
                        ),
                        size((right - left).max(px(2.0)), line_height),
                    ),
                    rgba(0x54d6a244),
                ));
            }
        }

        EditorPaintState {
            lines,
            cursor,
            selections,
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
        for selection in state.selections.drain(..) {
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
    fn native_editor_owns_a_rope_buffer_and_edit_generation(cx: &mut TestAppContext) {
        let editor = cx.new(NativeEditor::new);
        cx.read(|cx| {
            assert_eq!(editor.read(cx).buffer.text(), "");
            assert_eq!(editor.read(cx).edit_generation(), 0);
        });
    }

    #[gpui::test]
    fn incrementally_tracks_markdown_but_keeps_text_files_plain(cx: &mut TestAppContext) {
        let window = cx.add_window(|_, cx| NativeEditor::new(cx));
        window
            .update(cx, |editor, window, cx| {
                editor
                    .restore_workspace_snapshot(
                        "notes/one.md",
                        EditorSnapshot {
                            text: "# Title\n\nText".into(),
                            revision: "r1".into(),
                            selection: Default::default(),
                            scroll_top: 0.0,
                            dirty: false,
                        },
                    )
                    .unwrap();
                assert_eq!(editor.syntax_snapshot().unwrap().root_kind, "document");
                editor.insert_text(" novo", cx);
                assert!(!editor.syntax_snapshot().unwrap().has_error);

                editor
                    .restore_workspace_snapshot(
                        "notes/one.txt",
                        EditorSnapshot {
                            text: String::new(),
                            revision: "r2".into(),
                            selection: Default::default(),
                            scroll_top: 0.0,
                            dirty: false,
                        },
                    )
                    .unwrap();
                assert!(editor.syntax_snapshot().is_none());

                editor.replace_and_mark_text_in_range(None, "cafe", Some(4..4), window, cx);
                assert_eq!(editor.marked_text_range(window, cx), Some(0..4));
                editor.replace_text_in_range(None, "café", window, cx);
                assert_eq!(editor.buffer.text(), "café");
                editor.buffer.set_selection_utf16(0, 4).unwrap();
                editor.copy(&Copy, window, cx);
            })
            .unwrap();
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("café".into())
        );

        cx.write_to_clipboard(ClipboardItem::new_string(" 👋".into()));
        window
            .update(cx, |editor, window, cx| {
                editor.buffer.set_selection_utf16(4, 4).unwrap();
                editor.paste(&Paste, window, cx);
                assert_eq!(editor.buffer.text(), "café 👋");
            })
            .unwrap();
    }
}
