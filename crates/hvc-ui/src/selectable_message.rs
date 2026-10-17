use std::ops::Range;
use std::rc::Rc;

use gpui::{
    App, ClipboardItem, FocusHandle, HighlightStyle, IntoElement, MouseButton, RenderOnce,
    SharedString, StyledText, TextLayout, Window, div, prelude::*, px,
};
use hvc_editor::{ChatContentPart, parse_chat_content, parse_markdown};

use crate::{
    text_input::{Copy, SelectAll},
    theme,
};

type DiffCopyHandler = Rc<dyn Fn(String, &mut App)>;

/// Read-only, message-scoped selection. Text keeps its Markdown layout and all
/// blocks share one selection, including tables and diffs. No input handler is
/// installed, so typing, cut, paste and IME cannot mutate a saved message.
#[derive(IntoElement)]
pub(crate) struct SelectableMessage {
    id: SharedString,
    content: SharedString,
    on_copy_diff: Option<DiffCopyHandler>,
}

impl SelectableMessage {
    pub(crate) fn new(id: impl Into<SharedString>, content: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            content: SharedString::from(content.into()),
            on_copy_diff: None,
        }
    }

    pub(crate) fn on_copy_diff(mut self, handler: impl Fn(String, &mut App) + 'static) -> Self {
        self.on_copy_diff = Some(Rc::new(handler));
        self
    }
}

struct Selection {
    source: SharedString,
    text: String,
    anchor: usize,
    head: usize,
    dragging: bool,
    focus: FocusHandle,
}

impl Selection {
    fn range(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }

    fn selected_text(&self) -> Option<String> {
        let range = self.range();
        (!range.is_empty()).then(|| self.text[range].to_owned())
    }
}

#[derive(Clone)]
struct TextPiece {
    range: Range<usize>,
    layout: TextLayout,
}

fn boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// Pick the closest block even over paragraph gaps or beyond the last glyph.
fn index_at(pieces: &[TextPiece], position: gpui::Point<gpui::Pixels>) -> Option<usize> {
    pieces
        .iter()
        .min_by(|a, b| {
            let distance = |piece: &TextPiece| {
                let bounds = piece.layout.bounds();
                let dx = (bounds.left() - position.x)
                    .max(position.x - bounds.right())
                    .max(px(0.0));
                let dy = (bounds.top() - position.y)
                    .max(position.y - bounds.bottom())
                    .max(px(0.0));
                f32::from(dx).powi(2) + f32::from(dy).powi(2)
            };
            distance(a).total_cmp(&distance(b))
        })
        .map(|piece| {
            let local = piece
                .layout
                .index_for_position(position)
                .unwrap_or_else(|index| index);
            piece.range.start + local.min(piece.range.len())
        })
}

impl RenderOnce for SelectableMessage {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let selection = window.use_keyed_state(self.id.clone(), cx, |_, cx| Selection {
            source: self.content.clone(),
            text: String::new(),
            anchor: 0,
            head: 0,
            dragging: false,
            focus: cx.focus_handle(),
        });
        selection.update(cx, |state, _| {
            if state.source != self.content {
                state.source = self.content.clone();
                state.anchor = 0;
                state.head = 0;
                state.dragging = false;
            }
        });
        let focus = selection.read(cx).focus.clone();
        let selected = if focus.is_focused(window) {
            selection.read(cx).range()
        } else {
            0..0
        };
        let mut projected = String::new();
        let mut pieces = Vec::new();
        let mut text_element = |text: String| {
            if !projected.is_empty() {
                projected.push_str("\n\n");
            }
            let start = projected.len();
            projected.push_str(&text);
            let end = projected.len();
            let from = boundary(&text, selected.start.saturating_sub(start));
            let to = boundary(&text, selected.end.saturating_sub(start));
            let mut styled = StyledText::new(text);
            if from < to {
                styled = styled.with_highlights([(
                    from..to,
                    HighlightStyle {
                        background_color: Some(theme::accent().into()),
                        color: Some(theme::canvas().into()),
                        ..Default::default()
                    },
                )]);
            }
            let index = pieces.len();
            pieces.push(TextPiece {
                range: start..end,
                layout: styled.layout().clone(),
            });
            let debug_id = format!("{}:text:{index}", self.id);
            div()
                .debug_selector(move || debug_id.clone())
                .min_w_0()
                .child(styled)
                .into_any_element()
        };
        let mut blocks = Vec::new();
        for part in parse_chat_content(&self.content) {
            match part {
                ChatContentPart::Markdown(markdown) => {
                    blocks.extend(parse_markdown(&markdown).into_iter().map(|block| {
                        crate::workspace_view::render_markdown_block_with(block, &mut text_element)
                    }));
                }
                ChatContentPart::UnifiedDiff { content, file_path } => {
                    let copy = content.clone();
                    let on_copy = self.on_copy_diff.clone();
                    let diff_id =
                        SharedString::from(format!("{}:copy-diff:{}", self.id, blocks.len()));
                    blocks.push(
                        div()
                            .my_2()
                            .p_3()
                            .rounded_md()
                            .border_1()
                            .border_color(theme::warning())
                            .bg(theme::canvas())
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child(text_element(file_path)),
                                    )
                                    .child(
                                        div()
                                            .id(diff_id)
                                            .mt_2()
                                            .p_2()
                                            .rounded_md()
                                            .border_1()
                                            .border_color(theme::border())
                                            .bg(theme::surface_raised())
                                            .cursor_pointer()
                                            .hover(|style| style.border_color(theme::accent()))
                                            .on_click(move |_, _, cx| {
                                                if let Some(handler) = &on_copy {
                                                    handler(copy.clone(), cx);
                                                } else {
                                                    cx.write_to_clipboard(
                                                        ClipboardItem::new_string(copy.clone()),
                                                    );
                                                }
                                            })
                                            .child("Copy diff"),
                                    ),
                            )
                            .child(
                                div()
                                    .mt_2()
                                    .font_family(theme::FONT_MONO)
                                    .text_xs()
                                    .child(text_element(content)),
                            )
                            .into_any_element(),
                    );
                }
            }
        }
        selection.update(cx, |state, _| {
            if state.text != projected {
                state.text = projected;
                state.anchor = 0;
                state.head = 0;
                state.dragging = false;
            }
        });
        let pieces = Rc::new(pieces);
        let down_pieces = pieces.clone();
        let down_selection = selection.clone();
        let move_selection = selection.clone();
        let up_selection = selection.clone();
        let out_selection = selection.clone();
        let copy_selection = selection.clone();
        div()
            .id(self.id)
            .key_context("SelectableMessage")
            .track_focus(&focus)
            .min_w_0()
            .flex()
            .flex_col()
            .cursor_text()
            .on_action(move |_: &Copy, _, cx| {
                if let Some(text) = copy_selection.read(cx).selected_text() {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
            })
            .on_action(move |_: &SelectAll, _, cx| {
                selection.update(cx, |state, cx| {
                    state.anchor = 0;
                    state.head = state.text.len();
                    cx.notify();
                })
            })
            .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                if let Some(index) = index_at(&down_pieces, event.position) {
                    down_selection.update(cx, |state, cx| {
                        state.focus.focus(window);
                        let index = boundary(&state.text, index);
                        if !event.modifiers.shift {
                            state.anchor = index;
                        }
                        state.head = index;
                        state.dragging = true;
                        if event.click_count >= 3 {
                            state.anchor = 0;
                            state.head = state.text.len();
                        }
                        cx.notify();
                    });
                }
            })
            .on_mouse_move(move |event, _, cx| {
                if event.pressed_button == Some(MouseButton::Left)
                    && move_selection.read(cx).dragging
                    && let Some(index) = index_at(&pieces, event.position)
                {
                    move_selection.update(cx, |state, cx| {
                        state.head = boundary(&state.text, index);
                        cx.notify();
                    });
                }
            })
            .on_mouse_up(MouseButton::Left, move |_, _, cx| {
                up_selection.update(cx, |state, _| state.dragging = false)
            })
            .on_mouse_up_out(MouseButton::Left, move |_, _, cx| {
                out_selection.update(cx, |state, _| state.dragging = false)
            })
            .children(blocks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Context, Modifiers, Render, TestAppContext, point, px, size};

    struct Harness(String);
    impl Render for Harness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .w_full()
                .max_w(px(280.0))
                .child(SelectableMessage::new("test-message", self.0.clone()))
        }
    }

    #[gpui::test]
    fn selection_copies_across_formatted_blocks_in_both_directions(cx: &mut TestAppContext) {
        cx.update(crate::text_input::register_text_input_keybindings);
        let (_, visual) =
            cx.add_window_view(|_, _| Harness("a\u{e7}\u{e3}o 🦀\n\nsegunda linha".into()));
        visual.simulate_resize(size(px(400.0), px(300.0)));
        visual.run_until_parked();
        let first = visual.debug_bounds("test-message:text:0").unwrap();
        let last = visual.debug_bounds("test-message:text:1").unwrap();
        let start = point(first.left(), first.top() + px(5.0));
        let end = point(last.right() - px(1.0), last.bottom() - px(3.0));
        for (from, to) in [(start, end), (end, start)] {
            visual.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
            visual.simulate_mouse_move(to, MouseButton::Left, Modifiers::default());
            visual.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
            visual.simulate_keystrokes("ctrl-c");
            visual.update(|_, cx| {
                assert_eq!(
                    cx.read_from_clipboard().unwrap().text().as_deref(),
                    Some("a\u{e7}\u{e3}o 🦀\n\nsegunda linha")
                );
            });
        }
    }

    #[gpui::test]
    fn selection_is_read_only_and_tracks_updated_messages(cx: &mut TestAppContext) {
        cx.update(crate::text_input::register_text_input_keybindings);
        let (harness, visual) = cx.add_window_view(|_, _| Harness("# Title\n\nOriginal 🦀".into()));
        visual.run_until_parked();
        let bounds = visual.debug_bounds("test-message:text:1").unwrap();
        visual.simulate_mouse_down(bounds.center(), MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_up(bounds.center(), MouseButton::Left, Modifiers::default());
        visual.simulate_keystrokes("ctrl-a backspace delete ctrl-x");
        visual.simulate_keystrokes("ctrl-c");
        visual.update(|_, cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().as_deref(),
                Some("Title\n\nOriginal 🦀")
            )
        });
        harness.update(visual, |h, cx| {
            h.0 = "Updated \u{e9}".into();
            cx.notify();
        });
        visual.run_until_parked();
        visual.simulate_keystrokes("ctrl-a ctrl-c");
        visual.update(|_, cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().as_deref(),
                Some("Updated \u{e9}")
            )
        });
    }

    #[gpui::test]
    fn wrapped_messages_copy_completely_after_resize(cx: &mut TestAppContext) {
        cx.update(crate::text_input::register_text_input_keybindings);
        let text = format!("{}end", "caf\u{e9} 🦀 Unicode ".repeat(80));
        let (_, visual) = cx.add_window_view(|_, _| Harness(text.clone()));
        for width in [400.0, 160.0, 600.0] {
            visual.simulate_resize(size(px(width), px(300.0)));
            visual.run_until_parked();
            let bounds = visual.debug_bounds("test-message:text:0").unwrap();
            assert!(bounds.size.width <= px(280.0));
            assert!(bounds.size.height > px(100.0));
            let start = bounds.origin + point(px(3.0), px(3.0));
            visual.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
            visual.simulate_mouse_up(start, MouseButton::Left, Modifiers::default());
            visual.simulate_keystrokes("ctrl-a ctrl-c");
            visual.update(|_, cx| {
                assert_eq!(
                    cx.read_from_clipboard().unwrap().text().as_deref(),
                    Some(text.as_str())
                )
            });
        }
    }

    struct MultipleMessages;
    impl Render for MultipleMessages {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .flex()
                .flex_col()
                .w(px(280.0))
                .child(SelectableMessage::new("first", "first 🦀"))
                .child(SelectableMessage::new("second", "second \u{e9}"))
        }
    }

    #[gpui::test]
    fn selection_and_focus_are_scoped_to_each_message(cx: &mut TestAppContext) {
        cx.update(crate::text_input::register_text_input_keybindings);
        let (_, visual) = cx.add_window_view(|_, _| MultipleMessages);
        visual.run_until_parked();
        for (selector, expected) in [
            ("first:text:0", "first 🦀"),
            ("second:text:0", "second \u{e9}"),
            ("first:text:0", "first 🦀"),
        ] {
            let bounds = visual.debug_bounds(selector).unwrap();
            visual.simulate_mouse_down(bounds.center(), MouseButton::Left, Modifiers::default());
            visual.simulate_mouse_up(bounds.center(), MouseButton::Left, Modifiers::default());
            visual.simulate_keystrokes("ctrl-a ctrl-c");
            visual.update(|_, cx| {
                assert_eq!(
                    cx.read_from_clipboard().unwrap().text().as_deref(),
                    Some(expected)
                )
            });
        }
    }
}
