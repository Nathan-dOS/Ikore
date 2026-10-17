use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, ContentMask, Context, Element, ElementId, ElementInputHandler,
    Entity, EntityInputHandler, FocusHandle, Focusable, GlobalElementId, InspectorElementId,
    KeyBinding, LayoutId, MouseButton, Pixels, Point, ShapedLine, Style, TextRun, UTF16Selection,
    Window, actions, div, fill, point, prelude::*, px, relative, size,
};

use crate::theme;

actions!(
    native_text_input,
    [
        Backspace,
        Delete,
        SelectAll,
        Copy,
        Cut,
        Paste,
        MoveLeft,
        MoveRight,
        MoveToBeginning,
        MoveToEnd,
        BackspaceWord,
        Submit,
        InsertNewline,
        MoveUp,
        MoveDown
    ]
);

pub(crate) fn register_text_input_keybindings(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, Some("NativeTextInput")),
        KeyBinding::new("ctrl-backspace", BackspaceWord, Some("NativeTextInput")),
        KeyBinding::new("delete", Delete, Some("NativeTextInput")),
        KeyBinding::new("ctrl-a", SelectAll, Some("NativeTextInput")),
        KeyBinding::new("ctrl-c", Copy, Some("NativeTextInput")),
        KeyBinding::new("ctrl-a", SelectAll, Some("SelectableMessage")),
        KeyBinding::new("ctrl-c", Copy, Some("SelectableMessage")),
        KeyBinding::new("ctrl-x", Cut, Some("NativeTextInput")),
        KeyBinding::new("ctrl-v", Paste, Some("NativeTextInput")),
        KeyBinding::new("left", MoveLeft, Some("NativeTextInput")),
        KeyBinding::new("right", MoveRight, Some("NativeTextInput")),
        KeyBinding::new("home", MoveToBeginning, Some("NativeTextInput")),
        KeyBinding::new("end", MoveToEnd, Some("NativeTextInput")),
        KeyBinding::new("enter", Submit, Some("ChatComposer")),
        KeyBinding::new("shift-enter", InsertNewline, Some("ChatComposer")),
        KeyBinding::new("up", MoveUp, Some("ChatComposer")),
        KeyBinding::new("down", MoveDown, Some("ChatComposer")),
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
    last_line: Option<ShapedLine>,
    scroll_x: Pixels,
    multiline: bool,
    /// Rows a multiline input grows to before it scrolls internally. Six suits
    /// a chat composer; a field holding a written answer needs more.
    max_rows: usize,
    rows: Vec<InputRow>,
    scroll_y: Pixels,
    follow_caret: bool,
    drag_anchor: usize,
    caret_row: Option<usize>,
    preferred_column: Option<Pixels>,
    /// Extra left padding for a glyph drawn over the field (the Settings search icon).
    leading_inset: Pixels,
}

impl gpui::EventEmitter<Submit> for NativeTextInput {}

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
            last_line: None,
            scroll_x: px(0.0),
            multiline: false,
            max_rows: 6,
            rows: Vec::new(),
            scroll_y: px(0.0),
            follow_caret: true,
            drag_anchor: 0,
            caret_row: None,
            preferred_column: None,
            leading_inset: px(0.0),
        }
    }

    /// Leaves `inset` free at the left edge for an icon the parent draws over the field.
    pub(crate) fn leading_inset(mut self, inset: Pixels) -> Self {
        self.leading_inset = inset;
        self
    }

    pub(crate) fn obscured(mut self) -> Self {
        self.obscured = true;
        self
    }

    pub(crate) fn multiline(mut self) -> Self {
        self.multiline = true;
        self
    }

    /// Raises the height cap of a multiline input. No effect single-line.
    pub(crate) fn max_rows(mut self, rows: usize) -> Self {
        self.max_rows = rows.max(1);
        self
    }

    fn normalize(&self, text: &str) -> String {
        if self.multiline {
            text.replace("\r\n", "\n").replace('\r', "\n")
        } else {
            text.replace(['\r', '\n'], " ")
        }
    }

    fn submit(&mut self, _: &Submit, _: &mut Window, cx: &mut Context<Self>) {
        if self.multiline && self.marked_range.is_none() {
            cx.emit(Submit);
        }
    }

    fn insert_newline(&mut self, _: &InsertNewline, _: &mut Window, cx: &mut Context<Self>) {
        if self.multiline && self.marked_range.is_none() {
            self.replace_range(self.selection.clone(), "\n");
            self.follow_caret = true;
            cx.notify();
        }
    }

    fn move_vertical(&mut self, direction: f32, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        let caret = self.caret_position();
        let x = self.preferred_column.unwrap_or(caret.x);
        let target = ((caret.y / px(24.0) + direction).max(0.0) as usize).min(self.rows.len() - 1);
        let row = &self.rows[target];
        let byte = (row.start + row.line.closest_index_for_x(x)).min(self.value.len());
        let index = self.value[..byte].encode_utf16().count();
        self.selection = index..index;
        // The end of one soft row shares a byte offset with the next row.
        // Keep visual affinity and the desired column across Up/Down.
        self.caret_row = Some(target);
        self.preferred_column = Some(x);
        self.follow_caret = true;
        cx.notify();
    }

    fn move_up(&mut self, _: &MoveUp, _: &mut Window, cx: &mut Context<Self>) {
        self.move_vertical(-1.0, cx);
    }

    fn move_down(&mut self, _: &MoveDown, _: &mut Window, cx: &mut Context<Self>) {
        self.move_vertical(1.0, cx);
    }

    fn caret_position(&self) -> Point<Pixels> {
        let byte = byte_index_for_utf16(&self.value, self.selection.end);
        if let Some(index) = self.caret_row
            && let Some(row) = self.rows.get(index)
            && (row.start..=row.end).contains(&byte)
        {
            return point(
                row.line.x_for_index(byte - row.start),
                px(index as f32 * 24.0),
            );
        }
        row_position(&self.rows, byte)
    }

    fn reset_caret_affinity(&mut self) {
        self.caret_row = None;
        self.preferred_column = None;
        self.follow_caret = true;
    }

    pub(crate) fn value(&self) -> &str {
        &self.value
    }

    pub(crate) fn set_value(&mut self, value: impl Into<String>, cx: &mut Context<Self>) {
        self.value = value.into();
        self.reset_caret_affinity();
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
        self.reset_caret_affinity();
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

    fn display_index(&self, offset: usize) -> usize {
        let byte = byte_index_for_utf16(&self.value, offset);
        if self.obscured {
            self.value[..byte].chars().count() * '•'.len_utf8()
        } else {
            byte
        }
    }

    fn index_for_point(&self, position: Point<Pixels>) -> Option<usize> {
        let bounds = self.last_bounds?;
        if self.multiline {
            if self.rows.is_empty() {
                return Some(0);
            }
            let index = (((position.y - bounds.top() + self.scroll_y) / px(24.0)).max(0.0)
                as usize)
                .min(self.rows.len() - 1);
            let row = &self.rows[index];
            let byte = (row.start + row.line.closest_index_for_x(position.x - bounds.left()))
                .min(self.value.len());
            return Some(self.value[..byte].encode_utf16().count());
        }
        let line = self.last_line.as_ref()?;
        let byte = line.closest_index_for_x(position.x - bounds.left() + self.scroll_x);
        let byte = if self.obscured {
            self.value
                .char_indices()
                .nth(byte / '•'.len_utf8())
                .map_or(self.value.len(), |(byte, _)| byte)
        } else {
            byte.min(self.value.len())
        };
        Some(self.value[..byte].encode_utf16().count())
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
        self.reset_caret_affinity();
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
            self.replace_range(self.selection.clone(), &self.normalize(&text));
            cx.notify();
        }
    }

    fn move_left(&mut self, _: &MoveLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.reset_caret_affinity();
        let caret = if self.selection.is_empty() {
            previous_utf16_boundary(&self.value, self.selection.start)
        } else {
            self.selection.start
        };
        self.selection = caret..caret;
        cx.notify();
    }

    fn move_right(&mut self, _: &MoveRight, _: &mut Window, cx: &mut Context<Self>) {
        self.reset_caret_affinity();
        let caret = if self.selection.is_empty() {
            next_utf16_boundary(&self.value, self.selection.end)
        } else {
            self.selection.end
        };
        self.selection = caret..caret;
        cx.notify();
    }

    fn move_to_beginning(&mut self, _: &MoveToBeginning, _: &mut Window, cx: &mut Context<Self>) {
        self.reset_caret_affinity();
        self.selection = 0..0;
        cx.notify();
    }

    fn move_to_end(&mut self, _: &MoveToEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.reset_caret_affinity();
        let end = self.utf16_len();
        self.selection = end..end;
        cx.notify();
    }

    fn backspace_word(&mut self, _: &BackspaceWord, _: &mut Window, cx: &mut Context<Self>) {
        let range = if self.selection.is_empty() && self.selection.start > 0 {
            let start = previous_word_boundary_utf16(&self.value, self.selection.start);
            start..self.selection.start
        } else {
            self.selection.clone()
        };
        self.replace_range(range, "");
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
        self.replace_range(range, &self.normalize(new_text));
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
        let new_text = self.normalize(new_text);
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
        range: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let bounds = self.last_bounds.unwrap_or(bounds);
        if self.multiline {
            let start = row_position(&self.rows, byte_index_for_utf16(&self.value, range.start));
            let end = row_position(&self.rows, byte_index_for_utf16(&self.value, range.end));
            let y = (bounds.top() + start.y - self.scroll_y)
                .clamp(bounds.top(), (bounds.bottom() - px(24.0)).max(bounds.top()));
            return Some(Bounds::new(
                point(bounds.left() + start.x, y),
                size(
                    if start.y == end.y {
                        (end.x - start.x).max(px(2.0))
                    } else {
                        px(2.0)
                    },
                    px(24.0),
                ),
            ));
        }
        let line = self.last_line.as_ref()?;
        let start =
            bounds.left() + line.x_for_index(self.display_index(range.start)) - self.scroll_x;
        let end = bounds.left() + line.x_for_index(self.display_index(range.end)) - self.scroll_x;
        Some(Bounds::from_corners(
            point(start.clamp(bounds.left(), bounds.right()), bounds.top()),
            point(end.clamp(bounds.left(), bounds.right()), bounds.bottom()),
        ))
    }

    fn character_index_for_point(
        &mut self,
        position: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        self.index_for_point(position)
    }
}

impl Render for NativeTextInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context("NativeTextInput")
            .track_focus(&self.focus_handle(cx))
            .when(!self.multiline, |d| d.h(px(40.0)))
            .when(self.multiline, |d| {
                d.key_context("NativeTextInput ChatComposer").py(px(8.0))
            })
            .flex_shrink_0()
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .text_size(px(16.0))
            .line_height(px(24.0))
            .cursor_text()
            .overflow_hidden()
            .pl(px(12.0) + self.leading_inset)
            .pr(px(12.0))
            .rounded_md()
            .border_1()
            .border_color(if self.focus_handle.is_focused(window) {
                theme::accent()
            } else {
                theme::border_strong()
            })
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
            .on_action(cx.listener(Self::move_to_beginning))
            .on_action(cx.listener(Self::move_to_end))
            .on_action(cx.listener(Self::backspace_word))
            .when(self.multiline, |d| {
                d.on_action(cx.listener(Self::submit))
                    .on_action(cx.listener(Self::insert_newline))
                    .on_action(cx.listener(Self::move_up))
                    .on_action(cx.listener(Self::move_down))
                    .on_scroll_wheel(cx.listener(|input, event: &gpui::ScrollWheelEvent, _, cx| {
                        let height = input.last_bounds.map_or(px(144.0), |b| b.size.height);
                        let max_scroll = (px(input.rows.len() as f32 * 24.0) - height).max(px(0.0));
                        if max_scroll > px(0.0) {
                            input.scroll_y = (input.scroll_y - event.delta.pixel_delta(px(24.0)).y)
                                .clamp(px(0.0), max_scroll);
                            input.follow_caret = false;
                            cx.notify();
                            cx.stop_propagation();
                        }
                    }))
                    .on_mouse_move(cx.listener(|input, event: &gpui::MouseMoveEvent, _, cx| {
                        if event.pressed_button == Some(MouseButton::Left)
                            && let Some(caret) = input.index_for_point(event.position)
                        {
                            input.selection =
                                input.drag_anchor.min(caret)..input.drag_anchor.max(caret);
                            cx.notify();
                        }
                    }))
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|input, event: &gpui::MouseDownEvent, window, cx| {
                    input.focus_handle.focus(window);
                    let caret = input
                        .index_for_point(event.position)
                        .unwrap_or(input.utf16_len());
                    input.selection = caret..caret;
                    input.reset_caret_affinity();
                    input.drag_anchor = caret;
                    cx.notify();
                }),
            )
            .child(if self.multiline {
                MultilineInputElement { input: cx.entity() }.into_any_element()
            } else {
                TextInputElement { input: cx.entity() }.into_any_element()
            })
    }
}

struct TextInputElement {
    input: Entity<NativeTextInput>,
}

#[derive(Clone)]
struct InputRow {
    start: usize,
    end: usize,
    line: ShapedLine,
}

fn row_position(rows: &[InputRow], byte: usize) -> Point<Pixels> {
    let Some((index, row)) = rows
        .iter()
        .enumerate()
        .rev()
        .find(|(_, row)| row.start <= byte)
    else {
        return point(px(0.0), px(0.0));
    };
    point(
        row.line.x_for_index(byte.min(row.end) - row.start),
        px(index as f32 * 24.0),
    )
}

fn shape_input_rows(input: &NativeTextInput, width: Pixels, window: &Window) -> Vec<InputRow> {
    let text = if input.value.is_empty() {
        &input.placeholder
    } else {
        &input.value
    };
    let style = window.text_style();
    let font_size = style.font_size.to_pixels(window.rem_size());
    let color = if input.value.is_empty() {
        theme::text_muted()
    } else {
        theme::text()
    };
    let run = |len| TextRun {
        len,
        font: style.font(),
        color: color.into(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let mut rows = Vec::new();
    let mut offset = 0;
    // Preserve explicit empty lines, including a trailing Shift+Enter. GPUI's
    // shaper supplies Unicode glyph boundaries and wraps long unbroken words.
    for paragraph in text.split('\n') {
        let wrapped = window
            .text_system()
            .shape_text(
                paragraph.to_owned().into(),
                font_size,
                &[run(paragraph.len())],
                Some((width - px(2.0)).max(px(1.0))),
                None,
            )
            .unwrap_or_default();
        let mut boundaries = vec![0];
        if let Some(line) = wrapped.first() {
            boundaries.extend(line.wrap_boundaries.iter().map(|boundary| {
                line.unwrapped_layout.runs[boundary.run_ix].glyphs[boundary.glyph_ix].index
            }));
        }
        boundaries.push(paragraph.len());
        for pair in boundaries.windows(2) {
            let slice = &paragraph[pair[0]..pair[1]];
            rows.push(InputRow {
                start: offset + pair[0],
                end: offset + pair[1],
                line: window.text_system().shape_line(
                    slice.to_owned().into(),
                    font_size,
                    &[run(slice.len())],
                    None,
                ),
            });
        }
        offset += paragraph.len() + 1;
    }
    rows
}

struct MultilineInputElement {
    input: Entity<NativeTextInput>,
}

impl IntoElement for MultilineInputElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for MultilineInputElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

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
        _: &mut App,
    ) -> (LayoutId, ()) {
        let input = self.input.clone();
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.min_size.width = px(0.0).into();
        let id = window.request_measured_layout(style, move |known, available, window, cx| {
            let width = known.width.unwrap_or(match available.width {
                gpui::AvailableSpace::Definite(width) => width,
                _ => px(320.0),
            });
            let text_input = input.read(cx);
            let max_rows = text_input.max_rows;
            let rows = shape_input_rows(text_input, width, window);
            size(width, px(rows.len().clamp(1, max_rows) as f32 * 24.0))
        });
        (id, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let rows = shape_input_rows(self.input.read(cx), bounds.size.width, window);
        self.input.update(cx, |input, _| {
            let resized = input
                .last_bounds
                .is_none_or(|previous| previous.size != bounds.size);
            if resized {
                input.reset_caret_affinity();
            }
            input.rows = rows;
            let max_scroll = (px(input.rows.len() as f32 * 24.0) - bounds.size.height).max(px(0.0));
            let mut scroll = input.scroll_y.clamp(px(0.0), max_scroll);
            if input.follow_caret || resized {
                let caret = input.caret_position();
                scroll = scroll
                    .min(caret.y)
                    .max(caret.y + px(24.0) - bounds.size.height)
                    .clamp(px(0.0), max_scroll);
            }
            input.scroll_y = scroll;
            input.follow_caret = false;
            input.last_bounds = Some(bounds);
        });
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let input = self.input.read(cx);
        let focus = input.focus_handle.clone();
        let selection = byte_index_for_utf16(&input.value, input.selection.start)
            ..byte_index_for_utf16(&input.value, input.selection.end);
        let caret = input.caret_position();
        let scroll = input.scroll_y;
        let content_height = px(input.rows.len() as f32 * 24.0);
        let rows = input.rows.clone();
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for (index, row) in rows.iter().enumerate() {
                let y = bounds.top() + px(index as f32 * 24.0) - scroll;
                if y + px(24.0) <= bounds.top() || y >= bounds.bottom() {
                    continue;
                }
                if !selection.is_empty() && selection.start <= row.end && selection.end > row.start
                {
                    let start = row.line.x_for_index(
                        selection
                            .start
                            .saturating_sub(row.start)
                            .min(row.end - row.start),
                    );
                    let end = if selection.end > row.end {
                        bounds.size.width
                    } else {
                        row.line.x_for_index(selection.end - row.start)
                    };
                    window.paint_quad(fill(
                        Bounds::new(
                            point(bounds.left() + start, y),
                            size((end - start).max(px(2.0)), px(24.0)),
                        ),
                        theme::accent_soft(),
                    ));
                }
                let _ = row
                    .line
                    .paint(point(bounds.left(), y), px(24.0), window, cx);
            }
            if focus.is_focused(window) && selection.is_empty() {
                window.paint_quad(fill(
                    Bounds::new(
                        point(bounds.left() + caret.x, bounds.top() + caret.y - scroll),
                        size(px(2.0), px(24.0)),
                    ),
                    theme::accent(),
                ));
            }
            if content_height > bounds.size.height {
                let thumb =
                    (bounds.size.height * (bounds.size.height / content_height)).max(px(16.0));
                let y = bounds.top()
                    + (bounds.size.height - thumb)
                        * (scroll / (content_height - bounds.size.height));
                window.paint_quad(fill(
                    Bounds::new(point(bounds.right() - px(3.0), y), size(px(3.0), thumb)),
                    theme::text_muted(),
                ));
            }
        });
    }
}

struct TextInputPaintState {
    line: ShapedLine,
    origin: Point<Pixels>,
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
        style.size.height = window.line_height().into();
        style.min_size.width = px(0.0).into();
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
        let line = window.text_system().shape_line(
            text.into(),
            style.font_size.to_pixels(window.rem_size()),
            &[run],
            None,
        );
        let caret = line.x_for_index(input.display_index(input.selection.end));
        let visible_width = (bounds.size.width - px(2.0)).max(px(0.0));
        let max_scroll = (line.width - visible_width).max(px(0.0));
        let scroll_x = input
            .scroll_x
            .min(caret)
            .max(caret - visible_width)
            .clamp(px(0.0), max_scroll);
        self.input.update(cx, |input, _| input.scroll_x = scroll_x);
        TextInputPaintState {
            line,
            origin: point(bounds.left() - scroll_x, bounds.top()),
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
        let input = self.input.read(cx);
        let start = state.origin.x
            + state
                .line
                .x_for_index(input.display_index(input.selection.start));
        let end = state.origin.x
            + state
                .line
                .x_for_index(input.display_index(input.selection.end));
        let selected = !input.selection.is_empty();
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            if selected {
                window.paint_quad(fill(
                    Bounds::from_corners(point(start, bounds.top()), point(end, bounds.bottom())),
                    theme::accent_soft(),
                ));
            }
            let _ = state
                .line
                .paint(state.origin, window.line_height(), window, cx);
            if focus_handle.is_focused(window) && !selected {
                window.paint_quad(fill(
                    Bounds::new(point(end, bounds.top()), size(px(2.0), bounds.size.height)),
                    theme::accent(),
                ));
            }
        });
        self.input.update(cx, |input, _| {
            input.last_bounds = Some(bounds);
            input.last_line = Some(state.line.clone());
        });
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

fn previous_word_boundary_utf16(text: &str, offset: usize) -> usize {
    if offset == 0 {
        return 0;
    }
    let byte_offset = byte_index_for_utf16(text, offset);
    let slice = &text[..byte_offset];
    let trimmed = slice.trim_end_matches(char::is_whitespace);
    if trimmed.is_empty() {
        return 0;
    }
    let word_start_byte = match trimmed.rfind(char::is_whitespace) {
        Some(idx) => {
            let ws_char = trimmed[idx..].chars().next().unwrap();
            idx + ws_char.len_utf8()
        }
        None => 0,
    };
    text[..word_start_byte].encode_utf16().count()
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;

    #[gpui::test]
    fn composer_wraps_grows_scrolls_and_preserves_unicode(cx: &mut TestAppContext) {
        cx.update(register_text_input_keybindings);
        let (harness, visual) = cx.add_window_view(|window, cx| {
            let input = cx.new(|cx| NativeTextInput::new("Message", cx).multiline());
            input.read(cx).focus_handle.focus(window);
            InputHarness(input)
        });
        visual.simulate_resize(size(px(300.0), px(400.0)));
        let input = harness.read_with(visual, |h, _| h.0.clone());
        visual.simulate_input("café 👋");
        visual.run_until_parked();
        input.read_with(visual, |i, _| {
            assert_eq!(i.last_bounds.unwrap().size.height, px(24.0))
        });
        visual.simulate_keystrokes("shift-enter");
        visual.run_until_parked();
        input.read_with(visual, |i, _| {
            assert_eq!(i.value(), "café 👋\n");
            assert_eq!(i.last_bounds.unwrap().size.height, px(48.0));
        });
        let long = "long_unbroken_Unicode_👋é".repeat(45);
        visual.simulate_input(&long);
        visual.run_until_parked();
        let bounds = input.read_with(visual, |i, _| {
            assert_eq!(i.value(), format!("café 👋\n{long}"));
            let bounds = i.last_bounds.unwrap();
            assert_eq!(bounds.size.height, px(144.0));
            assert!(i.rows.len() > 6);
            assert!(i.scroll_y > px(0.0));
            assert!(i.rows.iter().all(|r| r.line.width <= bounds.size.width));
            let caret = i.caret_position();
            assert!(caret.y >= i.scroll_y && caret.y + px(24.0) <= i.scroll_y + bounds.size.height);
            assert_eq!(
                i.index_for_point(point(
                    bounds.left() + caret.x,
                    bounds.top() + caret.y - i.scroll_y + px(12.0)
                )),
                Some(i.utf16_len())
            );
            bounds
        });
        visual.simulate_event(gpui::ScrollWheelEvent {
            position: bounds.center(),
            delta: gpui::ScrollDelta::Pixels(point(px(0.0), px(10_000.0))),
            ..Default::default()
        });
        visual.run_until_parked();
        input.read_with(visual, |i, _| assert_eq!(i.scroll_y, px(0.0)));
        visual.simulate_keystrokes("end up down");
        visual.simulate_resize(size(px(220.0), px(400.0)));
        visual.run_until_parked();
        input.read_with(visual, |i, _| {
            assert!(i.scroll_y > px(0.0));
            assert_eq!(
                i.selection.end,
                i.utf16_len(),
                "caret={:?}, tail={:?}",
                i.caret_position(),
                i.rows
                    .iter()
                    .rev()
                    .take(4)
                    .map(|r| (r.start, r.end))
                    .collect::<Vec<_>>()
            );
        });
        visual.simulate_keystrokes("ctrl-a");
        visual.simulate_input("short");
        visual.run_until_parked();
        input.read_with(visual, |i, _| {
            assert_eq!(i.value(), "short");
            assert_eq!(i.last_bounds.unwrap().size.height, px(24.0));
            assert_eq!(i.scroll_y, px(0.0));
        });
    }

    #[gpui::test]
    fn composer_enter_emits_submit_except_during_ime_composition(cx: &mut TestAppContext) {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let submitted = Arc::new(AtomicUsize::new(0));
        cx.update(register_text_input_keybindings);
        let count = submitted.clone();
        let (harness, visual) = cx.add_window_view(move |window, cx| {
            let input = cx.new(|cx| NativeTextInput::new("Message", cx).multiline());
            input.read(cx).focus_handle.focus(window);
            cx.subscribe(&input, move |_, _, _: &Submit, _| {
                count.fetch_add(1, Ordering::SeqCst);
            })
            .detach();
            InputHarness(input)
        });
        let input = harness.read_with(visual, |h, _| h.0.clone());
        visual.simulate_input("message");
        visual.simulate_keystrokes("shift-enter");
        visual.run_until_parked();
        assert_eq!(submitted.load(Ordering::SeqCst), 0);
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        assert_eq!(submitted.load(Ordering::SeqCst), 1);
        input.update(visual, |i, _| i.marked_range = Some(0..1));
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        assert_eq!(submitted.load(Ordering::SeqCst), 1);
        input.read_with(visual, |i, _| assert_eq!(i.value(), "message\n"));
    }

    struct InputHarness(Entity<NativeTextInput>);

    impl Render for InputHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .flex()
                .flex_col()
                .child(div().h(px(100.0)).flex_shrink_0())
                .child(self.0.clone())
        }
    }

    #[gpui::test]
    fn long_input_tracks_caret_after_typing_navigation_and_resize(cx: &mut TestAppContext) {
        cx.update(register_text_input_keybindings);
        let (harness, visual) = cx.add_window_view(|window, cx| {
            let input = cx.new(|cx| NativeTextInput::new("Search", cx));
            input.read(cx).focus_handle.focus(window);
            InputHarness(input)
        });
        visual.simulate_resize(size(px(220.0), px(130.0)));
        let input = harness.read_with(visual, |harness, _| harness.0.clone());
        let text = "A long message with café and 👋 at the end ".repeat(4);
        visual.simulate_input(&text);
        visual.run_until_parked();
        let assert_caret_visible = |input: &NativeTextInput| {
            let bounds = input.last_bounds.unwrap();
            let line = input.last_line.as_ref().unwrap();
            let x = line.x_for_index(input.display_index(input.selection.end)) - input.scroll_x;
            assert!(
                x >= px(0.0) && x + px(2.0) <= bounds.size.width,
                "caret x={x:?}, bounds={bounds:?}"
            );
            assert_eq!(
                bounds.size.height,
                px(24.0),
                "the line must not shrink in a crowded form"
            );
            assert!(
                line.ascent + line.descent <= bounds.size.height,
                "glyphs must fit vertically"
            );
        };
        input.read_with(visual, |input, _| {
            assert_eq!(input.value(), text);
            assert!(input.scroll_x > px(0.0));
            assert_caret_visible(input);
        });
        visual.simulate_keystrokes("home");
        visual.run_until_parked();
        input.read_with(visual, |input, _| {
            assert_eq!(input.scroll_x, px(0.0));
            assert_caret_visible(input);
        });
        visual.simulate_keystrokes("end left backspace");
        visual.simulate_resize(size(px(160.0), px(150.0)));
        visual.run_until_parked();
        input.read_with(visual, |input, _| assert_caret_visible(input));
        visual.simulate_keystrokes("ctrl-a");
        visual.simulate_input("short");
        visual.run_until_parked();
        input.read_with(visual, |input, _| {
            assert_eq!(input.value(), "short");
            assert_eq!(input.scroll_x, px(0.0));
            assert_caret_visible(input);
        });
    }

    #[gpui::test]
    fn obscured_unicode_hit_testing_uses_displayed_bullets(cx: &mut TestAppContext) {
        let (harness, visual) = cx.add_window_view(|_, cx| {
            let input = cx.new(|cx| NativeTextInput::new("Secret", cx).obscured());
            input.update(cx, |input, cx| {
                input.set_value("a👋é-secret-value".repeat(8), cx)
            });
            InputHarness(input)
        });
        visual.simulate_resize(size(px(200.0), px(160.0)));
        visual.run_until_parked();
        let input = harness.read_with(visual, |harness, _| harness.0.clone());
        input.read_with(visual, |input, _| {
            assert!(input.scroll_x > px(0.0));
            let bounds = input.last_bounds.unwrap();
            let line = input.last_line.as_ref().unwrap();
            let end = point(
                bounds.left() + line.width - input.scroll_x,
                bounds.center().y,
            );
            assert_eq!(input.index_for_point(end), Some(input.utf16_len()));
            assert_eq!(input.display_index(3), 2 * '•'.len_utf8());
        });
    }

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

    #[test]
    fn previous_word_boundary_identifies_words_accurately() {
        let text = "hello world foo";
        assert_eq!(previous_word_boundary_utf16(text, 15), 12); // "foo" -> start at 12
        assert_eq!(previous_word_boundary_utf16(text, 12), 6); // "world " -> start at 6
        assert_eq!(previous_word_boundary_utf16(text, 6), 0); // "hello " -> start at 0
        assert_eq!(previous_word_boundary_utf16(text, 0), 0);
    }
}
