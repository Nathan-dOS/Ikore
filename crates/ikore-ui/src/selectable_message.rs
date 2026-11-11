use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    App, Bounds, ClipboardItem, Element, ElementId, FocusHandle, GlobalElementId, HighlightStyle,
    InspectorElementId, IntoElement, LayoutId, MouseButton, Pixels, RenderOnce, SharedString,
    StyledText, TextLayout, TextStyle, Window, div, prelude::*, px,
};
use ikore_editor::{ChatContentPart, parse_chat_content, parse_markdown};

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
    projection: MessageProjection,
    text_cache: Vec<CachedTextPiece>,
    text: String,
    anchor: usize,
    head: usize,
    dragging: bool,
    focus: FocusHandle,
}

/// Keep unchanged text measurements across streamed appends. The parser still
/// processes the complete source, including Markdown that revises earlier blocks.
#[derive(Clone)]
struct CachedTextPiece {
    text: SharedString,
    state: Rc<RefCell<CachedTextState>>,
}

impl CachedTextPiece {
    fn for_text(previous: Option<&Self>, text: &str) -> Self {
        if let Some(previous) = previous.filter(|piece| piece.text.as_ref() == text) {
            return previous.clone();
        }
        let text = SharedString::from(text.to_owned());
        Self {
            state: Rc::new(RefCell::new(CachedTextState::new(text.clone(), 0..0))),
            text,
        }
    }

    fn element(&self, selected: Range<usize>) -> CachedTextElement {
        CachedTextElement {
            // StyledText consumes highlights during request_layout. A selected
            // piece gets a fresh recipe; it never mutates the unselected cache.
            state: if selected.is_empty() {
                self.state.clone()
            } else {
                Rc::new(RefCell::new(CachedTextState::new(
                    self.text.clone(),
                    selected,
                )))
            },
        }
    }
}

struct CachedTextState {
    text: SharedString,
    selected: Range<usize>,
    styled: StyledText,
    style: Option<(TextStyle, Pixels)>,
}

impl CachedTextState {
    fn new(text: SharedString, selected: Range<usize>) -> Self {
        Self {
            styled: Self::styled(&text, selected.clone()),
            text,
            selected,
            style: None,
        }
    }

    fn styled(text: &SharedString, selected: Range<usize>) -> StyledText {
        let styled = StyledText::new(text.clone());
        if selected.is_empty() {
            styled
        } else {
            styled.with_highlights([(
                selected,
                HighlightStyle {
                    background_color: Some(theme::accent().into()),
                    color: Some(theme::canvas().into()),
                    ..Default::default()
                },
            )])
        }
    }

    fn prepare(&mut self, style: &TextStyle, rem_size: Pixels) -> bool {
        // TextLayout caches geometry and decoration, so the entire inherited
        // style matters, including color and the resolved size of a rem.
        let reset = !self.selected.is_empty()
            || self
                .style
                .as_ref()
                .is_some_and(|(old_style, old_rem)| old_style != style || *old_rem != rem_size);
        if reset {
            self.styled = Self::styled(&self.text, self.selected.clone());
        }
        if self
            .style
            .as_ref()
            .is_none_or(|(old, rem)| old != style || *rem != rem_size)
        {
            self.style = Some((style.clone(), rem_size));
        }
        reset
    }
}

struct CachedTextElement {
    state: Rc<RefCell<CachedTextState>>,
}

impl IntoElement for CachedTextElement {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for CachedTextElement {
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
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut state = self.state.borrow_mut();
        state.prepare(&window.text_style(), window.rem_size());
        state.styled.request_layout(id, inspector_id, window, cx)
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.state.borrow_mut().styled.prepaint(
            id,
            inspector_id,
            bounds,
            request_layout,
            window,
            cx,
        );
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut (),
        prepaint: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.state.borrow_mut().styled.paint(
            id,
            inspector_id,
            bounds,
            request_layout,
            prepaint,
            window,
            cx,
        );
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum MessagePart {
    Markdown(ikore_editor::MarkdownBlock),
    UnifiedDiff { content: String, file_path: String },
}

struct MessageProjection {
    source: SharedString,
    parts: Rc<Vec<MessagePart>>,
}

impl MessageProjection {
    fn new(source: SharedString) -> Self {
        let mut parts = Vec::new();
        for part in parse_chat_content(&source) {
            match part {
                ChatContentPart::Markdown(markdown) => {
                    parts.extend(
                        parse_markdown(&markdown)
                            .into_iter()
                            .map(MessagePart::Markdown),
                    );
                }
                ChatContentPart::UnifiedDiff { content, file_path } => {
                    parts.push(MessagePart::UnifiedDiff { content, file_path });
                }
            }
        }
        Self {
            source,
            parts: Rc::new(parts),
        }
    }

    fn refresh(&mut self, source: SharedString) -> bool {
        if self.source == source {
            return false;
        }
        *self = Self::new(source);
        true
    }
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
    state: Rc<RefCell<CachedTextState>>,
}

impl TextPiece {
    fn layout(&self) -> TextLayout {
        // A style/rem change may replace StyledText during request_layout,
        // after the selection handlers were built. Read the current layout.
        self.state.borrow().styled.layout().clone()
    }
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
                let bounds = piece.layout().bounds();
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
                .layout()
                .index_for_position(position)
                .unwrap_or_else(|index| index);
            piece.range.start + local.min(piece.range.len())
        })
}

impl RenderOnce for SelectableMessage {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let selection = window.use_keyed_state(self.id.clone(), cx, |_, cx| Selection {
            projection: MessageProjection::new(self.content.clone()),
            text_cache: Vec::new(),
            text: String::new(),
            anchor: 0,
            head: 0,
            dragging: false,
            focus: cx.focus_handle(),
        });
        selection.update(cx, |state, _| {
            if state.projection.refresh(self.content.clone()) {
                state.anchor = 0;
                state.head = 0;
                state.dragging = false;
            }
        });
        let focus = selection.read(cx).focus.clone();
        let parsed = selection.read(cx).projection.parts.clone();
        let selected = if focus.is_focused(window) {
            selection.read(cx).range()
        } else {
            0..0
        };
        let mut projected = String::with_capacity(self.content.len());
        let previous_text = selection.update(cx, |state, _| std::mem::take(&mut state.text_cache));
        let text_capacity = previous_text.len().max(parsed.len());
        let mut pieces = Vec::with_capacity(text_capacity);
        let mut text_cache = Vec::with_capacity(text_capacity);
        let mut text_element = |text: &str| {
            if !projected.is_empty() {
                projected.push_str("\n\n");
            }
            let start = projected.len();
            projected.push_str(text);
            let end = projected.len();
            let from = boundary(text, selected.start.saturating_sub(start));
            let to = boundary(text, selected.end.saturating_sub(start));
            let index = pieces.len();
            let cached = CachedTextPiece::for_text(previous_text.get(index), text);
            let element = cached.element(from..to);
            pieces.push(TextPiece {
                range: start..end,
                state: element.state.clone(),
            });
            text_cache.push(cached);
            let debug_id = format!("{}:text:{index}", self.id);
            div()
                .debug_selector(move || debug_id.clone())
                .min_w_0()
                .child(element)
        };
        let mut blocks = Vec::with_capacity(parsed.len());
        for part in parsed.iter() {
            match part {
                MessagePart::Markdown(block) => {
                    blocks.push(crate::workspace_view::render_markdown_block_with(
                        block,
                        &mut text_element,
                    ));
                }
                MessagePart::UnifiedDiff { content, file_path } => {
                    let copy = content.clone();
                    let on_copy = self.on_copy_diff.clone();
                    let diff_id =
                        SharedString::from(format!("{}:copy-diff:{}", self.id, blocks.len()));
                    let diff_selector = diff_id.clone();
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
                                            .debug_selector(move || diff_selector.to_string())
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
            state.text_cache = text_cache;
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

    #[gpui::test]
    fn owned_markdown_renderer_moves_original_paragraph_storage(cx: &mut TestAppContext) {
        let text = String::from("Owned paragraph \u{e9} \u{1f980}");
        let original = text.as_ptr() as usize;
        let (_, visual) = cx.add_window_view(|_, _| Harness(String::new()));
        visual.draw(
            point(px(0.0), px(0.0)),
            size(px(280.0), px(300.0)),
            |_, _| {
                crate::workspace_view::render_markdown_block_owned_with(
                    ikore_editor::MarkdownBlock::Paragraph(text),
                    &mut |text| {
                        assert_eq!(text.as_ptr() as usize, original);
                        assert_eq!(text, "Owned paragraph \u{e9} \u{1f980}");
                        text.into_any_element()
                    },
                )
            },
        );
    }

    #[gpui::test]
    fn owned_and_borrowed_markdown_renderers_keep_matching_block_geometry(cx: &mut TestAppContext) {
        use ikore_editor::MarkdownBlock;
        struct ParityHarness(Vec<MarkdownBlock>);
        impl Render for ParityHarness {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div()
                    .flex()
                    .flex_col()
                    .children(self.0.iter().enumerate().map(|(index, block)| {
                        div().flex().children([
                            div()
                                .w(px(280.0))
                                .debug_selector(move || format!("owned-{index}"))
                                .child(crate::workspace_view::render_markdown_block(block.clone())),
                            div()
                                .w(px(280.0))
                                .debug_selector(move || format!("borrowed-{index}"))
                                .child(crate::workspace_view::render_markdown_block_with(
                                    block,
                                    &mut |text| div().child(text.to_owned()),
                                )),
                        ])
                    }))
            }
        }
        let (_, visual) = cx.add_window_view(|_, _| {
            ParityHarness(vec![
                MarkdownBlock::Paragraph("Complete paragraph \u{e9} \u{1f980} ".repeat(20)),
                MarkdownBlock::Heading {
                    level: 1,
                    text: "Heading".into(),
                },
                MarkdownBlock::Heading {
                    level: 6,
                    text: "Small heading".into(),
                },
                MarkdownBlock::CodeBlock {
                    language: Some("rust".into()),
                    code: "let value = 1;".into(),
                },
                MarkdownBlock::CodeBlock {
                    language: None,
                    code: "Unlabelled code".into(),
                },
                MarkdownBlock::Quote("Quotation \u{e9}".into()),
                MarkdownBlock::ListItem("List \u{1f980}".into()),
                MarkdownBlock::Table {
                    headers: vec!["Name".into(), "Value".into()],
                    rows: vec![vec!["Cell".into(), "Unicode \u{e9}".into()]],
                },
                MarkdownBlock::Rule,
            ])
        });
        visual.simulate_resize(size(px(800.0), px(1800.0)));
        visual.run_until_parked();
        for (owned, borrowed) in [
            ("owned-0", "borrowed-0"),
            ("owned-1", "borrowed-1"),
            ("owned-2", "borrowed-2"),
            ("owned-3", "borrowed-3"),
            ("owned-4", "borrowed-4"),
            ("owned-5", "borrowed-5"),
            ("owned-6", "borrowed-6"),
            ("owned-7", "borrowed-7"),
            ("owned-8", "borrowed-8"),
        ] {
            assert_eq!(
                visual.debug_bounds(owned).unwrap().size,
                visual.debug_bounds(borrowed).unwrap().size,
                "{owned}"
            );
        }
    }

    #[gpui::test]
    fn borrowed_markdown_renderer_keeps_original_text_and_complete_transformed_blocks(
        cx: &mut TestAppContext,
    ) {
        use ikore_editor::MarkdownBlock;
        let paragraph = String::from("Paragraph \u{e9} \u{1f980}");
        let header = String::from("Column");
        let cell = String::from("Cell \u{e9}");
        let expected_pointers = [
            paragraph.as_ptr() as usize,
            header.as_ptr() as usize,
            cell.as_ptr() as usize,
        ];
        let observed = Rc::new(RefCell::new(Vec::new()));
        let output = observed.clone();
        struct BorrowedHarness {
            blocks: Vec<MarkdownBlock>,
            observed: Rc<RefCell<Vec<(usize, String)>>>,
        }
        impl Render for BorrowedHarness {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                self.observed.borrow_mut().clear();
                div().children(self.blocks.iter().map(|block| {
                    crate::workspace_view::render_markdown_block_with(block, &mut |text| {
                        self.observed
                            .borrow_mut()
                            .push((text.as_ptr() as usize, text.to_owned()));
                        div().child(text.to_owned())
                    })
                }))
            }
        }
        let (_, visual) = cx.add_window_view(|_, _| BorrowedHarness {
            blocks: vec![
                MarkdownBlock::Paragraph(paragraph),
                MarkdownBlock::Table {
                    headers: vec![header],
                    rows: vec![vec![cell]],
                },
                MarkdownBlock::CodeBlock {
                    language: Some("rust".into()),
                    code: "let value = 1;".into(),
                },
                MarkdownBlock::ListItem("List \u{1f980}".into()),
            ],
            observed: output,
        });
        visual.run_until_parked();
        let observed = observed.borrow();
        assert_eq!(
            observed
                .iter()
                .take(3)
                .map(|(pointer, _)| *pointer)
                .collect::<Vec<_>>(),
            expected_pointers
        );
        assert_eq!(
            observed
                .iter()
                .map(|(_, text)| text.as_str())
                .collect::<Vec<_>>(),
            [
                "Paragraph \u{e9} \u{1f980}",
                "Column",
                "Cell \u{e9}",
                "rust\nlet value = 1;",
                "\u{2022} List \u{1f980}"
            ]
        );
    }

    #[test]
    fn text_piece_cache_reuses_equal_text_and_replaces_only_changed_pieces() {
        let first = CachedTextPiece::for_text(None, "First \u{e9} \u{1f980}");
        let second = CachedTextPiece::for_text(None, "Second");
        let first_again = CachedTextPiece::for_text(Some(&first), "First \u{e9} \u{1f980}");
        let second_changed = CachedTextPiece::for_text(Some(&second), "Second updated");
        assert!(Rc::ptr_eq(&first.state, &first_again.state));
        assert!(!Rc::ptr_eq(&second.state, &second_changed.state));
        assert_eq!(second_changed.text.as_ref(), "Second updated");
    }

    #[test]
    fn text_piece_cache_invalidates_inherited_style_and_rem_and_refreshes_highlights() {
        let piece = CachedTextPiece::for_text(None, "Unicode \u{e9} \u{1f980}");
        let style = gpui::TextStyle::default();
        let mut state = piece.state.borrow_mut();
        assert!(!state.prepare(&style, px(16.0)));
        assert!(!state.prepare(&style, px(16.0)));
        let larger = gpui::TextStyle {
            font_size: px(24.0).into(),
            ..style.clone()
        };
        assert!(state.prepare(&larger, px(16.0)));
        assert!(!state.prepare(&larger, px(16.0)));
        assert!(state.prepare(&larger, px(20.0)));
        let recolored = gpui::TextStyle {
            color: theme::accent().into(),
            ..larger
        };
        assert!(state.prepare(&recolored, px(20.0)));
        drop(state);

        let ordinary = piece.element(0..0);
        let selected = piece.element(0..piece.text.len());
        let selected_again = piece.element(0..piece.text.len());
        assert!(Rc::ptr_eq(&piece.state, &ordinary.state));
        assert!(!Rc::ptr_eq(&piece.state, &selected.state));
        assert!(!Rc::ptr_eq(&selected.state, &selected_again.state));
        let mut selected = selected.state.borrow_mut();
        assert!(selected.prepare(&style, px(16.0)));
        assert!(selected.prepare(&style, px(16.0)));
    }

    struct CachedHarness {
        piece: CachedTextPiece,
        font_rems: f32,
    }

    fn measure_piece_at(
        piece: &CachedTextPiece,
        width: Option<f32>,
        visual: &mut gpui::VisualTestContext,
    ) -> gpui::Size<Pixels> {
        use gpui::AvailableSpace;
        visual.draw(
            point(px(11.0), px(17.0)),
            size(
                width.map_or(AvailableSpace::MaxContent, |width| {
                    AvailableSpace::Definite(px(width))
                }),
                AvailableSpace::MaxContent,
            ),
            |_, _| piece.element(0..0),
        );
        piece.state.borrow().styled.layout().bounds().size
    }

    #[gpui::test]
    fn four_constraints_reuse_measurements_and_keep_unicode_hit_testing(cx: &mut TestAppContext) {
        let source = "Several words with Unicode \u{e9} \u{1f980} and a final word";
        let piece = CachedTextPiece::for_text(None, source);
        let (_, visual) = cx.add_window_view(|_, _| Harness(String::new()));
        let widths = [Some(60.0), Some(120.0), None, Some(280.0)];
        let sizes = widths.map(|width| measure_piece_at(&piece, width, visual));
        let initial = piece.state.borrow().styled.layout().measurement_count();
        assert!(initial > 0);
        for _ in 0..3 {
            for (width, expected) in widths.into_iter().zip(sizes) {
                assert_eq!(measure_piece_at(&piece, width, visual), expected);
                let state = piece.state.borrow();
                let layout = state.styled.layout();
                assert_eq!(layout.text(), source);
                assert_eq!(layout.len(), source.len());
                let start = layout.position_for_index(0).unwrap();
                assert_eq!(
                    layout
                        .index_for_position(start)
                        .unwrap_or_else(|index| index),
                    0
                );
                let after = layout.bounds().bottom_right() + point(px(1.0), px(1.0));
                assert_eq!(
                    layout
                        .index_for_position(after)
                        .unwrap_or_else(|index| index),
                    source.len()
                );
                assert!(layout.retained_measurement_count() <= 4);
            }
        }
        assert_eq!(
            piece.state.borrow().styled.layout().measurement_count(),
            initial,
            "revisiting retained constraints must not measure the unchanged text again"
        );
        assert!(sizes[2].width > sizes[0].width);
        assert!(sizes[2].height < sizes[0].height);
        let mut state = piece.state.borrow_mut();
        let changed_style = gpui::TextStyle {
            font_weight: gpui::FontWeight::BOLD,
            ..gpui::TextStyle::default()
        };
        assert!(state.prepare(&changed_style, px(24.0)));
        assert_eq!(state.styled.layout().measurement_count(), 0);
        assert_eq!(state.styled.layout().retained_measurement_count(), 0);
    }

    #[gpui::test]
    fn fifth_constraint_evicts_the_least_recently_used_measurement(cx: &mut TestAppContext) {
        let piece = CachedTextPiece::for_text(None, "A full Unicode \u{1f980} sentence that wraps");
        let (_, visual) = cx.add_window_view(|_, _| Harness(String::new()));
        for width in [60.0, 120.0, 180.0, 240.0] {
            measure_piece_at(&piece, Some(width), visual);
        }
        let initial = piece.state.borrow().styled.layout().measurement_count();
        measure_piece_at(&piece, Some(60.0), visual);
        assert_eq!(
            piece.state.borrow().styled.layout().measurement_count(),
            initial
        );
        measure_piece_at(&piece, Some(300.0), visual);
        let fifth = piece.state.borrow().styled.layout().measurement_count();
        assert!(fifth > initial);
        assert_eq!(
            piece
                .state
                .borrow()
                .styled
                .layout()
                .retained_measurement_count(),
            4
        );
        measure_piece_at(&piece, Some(60.0), visual);
        assert_eq!(
            piece.state.borrow().styled.layout().measurement_count(),
            fifth
        );
        measure_piece_at(&piece, Some(120.0), visual);
        assert!(piece.state.borrow().styled.layout().measurement_count() > fifth);
        assert_eq!(
            piece
                .state
                .borrow()
                .styled
                .layout()
                .retained_measurement_count(),
            4
        );
    }

    #[gpui::test]
    fn retained_truncation_constraints_restore_complete_unicode_runs(cx: &mut TestAppContext) {
        use gpui::AvailableSpace;
        let source = "Unicode \u{e9} \u{1f980} with a complete long label";
        let piece = CachedTextPiece::for_text(None, source);
        let (_, visual) = cx.add_window_view(|_, _| Harness(String::new()));
        let mut measure = |width: Option<f32>| {
            visual.draw(
                point(px(11.0), px(17.0)),
                size(
                    width.map_or(AvailableSpace::MaxContent, |width| {
                        AvailableSpace::Definite(px(width))
                    }),
                    AvailableSpace::MaxContent,
                ),
                |_, _| {
                    div()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(piece.element(0..0))
                },
            );
            let state = piece.state.borrow();
            let layout = state.styled.layout();
            assert_eq!(layout.len(), layout.text().len());
            (layout.bounds().size, layout.text())
        };
        let widths = [Some(60.0), None, Some(180.0)];
        let expected = widths.map(&mut measure);
        assert_eq!(expected[1].1, source);
        assert!(expected[0].1.ends_with('\u{2026}'));
        assert_ne!(expected[0].1, expected[2].1);
        let initial = piece.state.borrow().styled.layout().measurement_count();
        for _ in 0..3 {
            for (width, expected) in widths.into_iter().zip(&expected) {
                assert_eq!(&measure(width), expected);
            }
        }
        assert_eq!(
            piece.state.borrow().styled.layout().measurement_count(),
            initial
        );
        assert!(
            piece
                .state
                .borrow()
                .styled
                .layout()
                .retained_measurement_count()
                <= 4
        );
    }

    #[gpui::test]
    fn cached_text_remeasures_when_wrapping_becomes_unconstrained(cx: &mut TestAppContext) {
        use gpui::{AvailableSpace, Styled};
        let result = Rc::new(RefCell::new(None));
        let output = result.clone();
        let (_, visual) = cx.add_window_view(|_, _| {
            type MeasuredSizes = Rc<RefCell<Option<(gpui::Size<Pixels>, gpui::Size<Pixels>)>>>;
            struct MeasureHarness(MeasuredSizes);
            impl Render for MeasureHarness {
                fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                    let output = self.0.clone();
                    gpui::canvas(
                        move |_, window, cx| {
                            let cache = CachedTextPiece::for_text(
                                None,
                                "Several words with Unicode \u{e9} \u{1f980}",
                            );
                            let mut text = cache.element(0..0).into_any_element();
                            let narrow = text.layout_as_root(
                                size(
                                    AvailableSpace::Definite(px(60.0)),
                                    AvailableSpace::MaxContent,
                                ),
                                window,
                                cx,
                            );
                            let intrinsic = text.layout_as_root(
                                size(AvailableSpace::MaxContent, AvailableSpace::MaxContent),
                                window,
                                cx,
                            );
                            *output.borrow_mut() = Some((narrow, intrinsic));
                        },
                        |_, _, _, _| {},
                    )
                    .size(px(1.0))
                }
            }
            MeasureHarness(output)
        });
        visual.run_until_parked();
        let (narrow, intrinsic) = result.borrow().unwrap();
        assert!(intrinsic.width > narrow.width);
        assert!(intrinsic.height < narrow.height);
    }
    impl Render for CachedHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().w_full().text_size(gpui::rems(self.font_rems)).child(
                div()
                    .debug_selector(|| "cached-text".into())
                    .child(self.piece.element(0..0)),
            )
        }
    }

    #[gpui::test]
    fn cached_text_measures_inherited_style_rem_size_and_wrapping(cx: &mut TestAppContext) {
        let (harness, visual) = cx.add_window_view(|_, _| CachedHarness {
            piece: CachedTextPiece::for_text(None, &"Unicode \u{e9} \u{1f980} ".repeat(24)),
            font_rems: 1.0,
        });
        visual.simulate_resize(size(px(400.0), px(900.0)));
        visual.update(|window, _| window.set_rem_size(px(16.0)));
        visual.run_until_parked();
        let initial = visual.debug_bounds("cached-text").unwrap();
        harness.update(visual, |h, cx| {
            h.font_rems = 2.0;
            cx.notify();
        });
        visual.run_until_parked();
        let larger_font = visual.debug_bounds("cached-text").unwrap();
        assert!(larger_font.size.height > initial.size.height);
        visual.update(|window, _| window.set_rem_size(px(24.0)));
        harness.update(visual, |_, cx| cx.notify());
        visual.run_until_parked();
        let larger_rem = visual.debug_bounds("cached-text").unwrap();
        assert!(larger_rem.size.height > larger_font.size.height);
        visual.simulate_resize(size(px(180.0), px(900.0)));
        visual.run_until_parked();
        let narrow = visual.debug_bounds("cached-text").unwrap();
        assert!(narrow.size.width <= px(180.0));
        assert!(narrow.size.height > larger_rem.size.height);
        visual.simulate_resize(size(px(600.0), px(900.0)));
        visual.run_until_parked();
        assert!(visual.debug_bounds("cached-text").unwrap().size.height < narrow.size.height);
    }

    #[gpui::test]
    fn cached_streamed_table_and_diff_keep_complete_unicode_copy(cx: &mut TestAppContext) {
        cx.update(crate::text_input::register_text_input_keybindings);
        let diff = "--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-old\n+new \u{1f980}";
        let source = format!(
            "# Start\n\n| Column | Value |\n| --- | --- |\n| Unicode | caf\u{e9} \u{1f980} |\n\n```diff\n{diff}\n```"
        );
        let (harness, visual) = cx.add_window_view(|_, _| Harness(source.clone()));
        visual.simulate_resize(size(px(600.0), px(900.0)));
        visual.run_until_parked();
        harness.update(visual, |h, cx| {
            h.0.push_str("\n\nFinal \u{e9} \u{1f980}");
            cx.notify();
        });
        visual.run_until_parked();
        let first = visual.debug_bounds("test-message:text:0").unwrap();
        visual.simulate_mouse_down(first.center(), MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_up(first.center(), MouseButton::Left, Modifiers::default());
        visual.simulate_keystrokes("ctrl-a");
        let expected = format!(
            "Start\n\nColumn\n\nValue\n\nUnicode\n\ncaf\u{e9} \u{1f980}\n\nsrc/a.rs\n\n{diff}\n\nFinal \u{e9} \u{1f980}"
        );
        for width in [600.0, 300.0, 600.0] {
            visual.simulate_resize(size(px(width), px(900.0)));
            harness.update(visual, |_, cx| cx.notify());
            visual.run_until_parked();
            visual.simulate_keystrokes("ctrl-c");
            visual.update(|_, cx| {
                assert_eq!(
                    cx.read_from_clipboard().unwrap().text().as_deref(),
                    Some(expected.as_str())
                );
            });
        }
        let copy = visual.debug_bounds("test-message:copy-diff:2").unwrap();
        visual.simulate_click(copy.center(), Modifiers::default());
        visual.update(|_, cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().as_deref(),
                Some(diff)
            );
        });
    }

    #[test]
    fn message_projection_reuses_parsing_until_the_message_changes() {
        let source =
            "# Heading\n\n```diff\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-old\n+new\n```";
        let mut projection = MessageProjection::new(source.into());
        let original = projection.parts.clone();
        assert!(!projection.refresh(source.to_string().into()));
        assert!(Rc::ptr_eq(&original, &projection.parts));
        assert!(projection.parts.iter().any(|part| matches!(part, MessagePart::Markdown(ikore_editor::MarkdownBlock::Heading { text, .. }) if text == "Heading")));
        assert!(projection.parts.iter().any(|part| matches!(part, MessagePart::UnifiedDiff { file_path, .. } if file_path == "src/a.rs")));
        assert!(projection.refresh("Updated 🦀".into()));
        assert!(!Rc::ptr_eq(&original, &projection.parts));
        assert_eq!(
            projection.parts.as_ref(),
            &[MessagePart::Markdown(
                ikore_editor::MarkdownBlock::Paragraph("Updated 🦀".into())
            )]
        );
    }

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

    // Test-only reference: keep the legacy Paragraph tree literal. Do not route
    // this arm through the production renderer or a shared paragraph style builder.
    #[derive(Clone, Copy, Debug)]
    enum ParagraphArm {
        Production,
        LegacyLiteral,
    }

    #[derive(Clone, Copy, Debug)]
    enum ParagraphParent {
        FlexColumn,
        Block,
        Assistant,
        User85,
    }

    #[derive(Default)]
    struct ParagraphProbeOutput {
        nodes: usize,
        bounds: Option<Bounds<Pixels>>,
    }

    // Delegates the child's LayoutId: this observes real Taffy nodes, adding none.
    struct ParagraphProbe {
        child: gpui::AnyElement,
        output: Rc<RefCell<ParagraphProbeOutput>>,
    }

    impl IntoElement for ParagraphProbe {
        type Element = Self;
        fn into_element(self) -> Self {
            self
        }
    }

    impl Element for ParagraphProbe {
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
            cx: &mut App,
        ) -> (LayoutId, ()) {
            let before = window.layout_node_count().expect("layout engine available");
            let layout = self.child.request_layout(window, cx);
            self.output.borrow_mut().nodes = window
                .layout_node_count()
                .expect("layout engine available")
                .checked_sub(before)
                .unwrap();
            (layout, ())
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
            self.output.borrow_mut().bounds = Some(bounds);
            let _ = self.child.prepaint(window, cx);
        }
        fn paint(
            &mut self,
            _: Option<&GlobalElementId>,
            _: Option<&InspectorElementId>,
            _: Bounds<Pixels>,
            _: &mut (),
            _: &mut (),
            window: &mut Window,
            cx: &mut App,
        ) {
            self.child.paint(window, cx);
        }
    }

    fn paragraph_parent(parent: ParagraphParent, message: gpui::AnyElement) -> gpui::Div {
        match parent {
            ParagraphParent::FlexColumn => div().min_w_0().flex().flex_col().child(message),
            ParagraphParent::Block => div().child(message),
            // Geometric structure of chat_view: avatar/gap, flexible body and metadata.
            ParagraphParent::Assistant => div()
                .w_full()
                .flex()
                .items_start()
                .gap_3()
                .child(div().flex_shrink_0().size(px(28.0)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(div().h(px(14.0)))
                        .child(div().min_w_0().flex().flex_col().child(message)),
                ),
            ParagraphParent::User85 => div()
                .w_full()
                .flex()
                .flex_col()
                .items_end()
                .gap_1()
                .child(
                    div()
                        .max_w(gpui::relative(0.85))
                        .min_w_0()
                        .px_4()
                        .py_3()
                        .child(div().min_w_0().flex().flex_col().child(message)),
                )
                .child(div().h(px(14.0))),
        }
    }

    #[derive(Debug, PartialEq)]
    struct ParagraphCaretSnapshot {
        index: usize,
        position: Option<gpui::Point<Pixels>>,
        hit: Option<Result<usize, usize>>,
    }

    #[derive(Debug, PartialEq)]
    struct ParagraphLeafSnapshot {
        bounds: Bounds<Pixels>,
        text: String,
        line_height: Pixels,
        style: Option<(TextStyle, Pixels)>,
        // Preserve Option/Result exactly: an internal ligature/combining byte
        // need not have a distinct caret, and hit-testing can return an edge Err.
        carets: Vec<ParagraphCaretSnapshot>,
    }

    #[derive(Debug, PartialEq)]
    struct ParagraphGeometry {
        container: Bounds<Pixels>,
        blocks: Vec<Bounds<Pixels>>,
        leaves: Vec<ParagraphLeafSnapshot>,
    }

    struct ParagraphFrame {
        geometry: ParagraphGeometry,
        subtree_nodes: usize,
        paragraph_nodes: Vec<usize>,
    }

    struct ParagraphCorpus {
        texts: Vec<String>,
        pieces: Vec<CachedTextPiece>,
    }

    impl ParagraphCorpus {
        fn new(texts: &[String]) -> Self {
            Self {
                texts: texts.to_vec(),
                pieces: texts
                    .iter()
                    .map(|text| CachedTextPiece::for_text(None, text))
                    .collect(),
            }
        }
    }

    fn draw_paragraph_arm(
        corpus: &ParagraphCorpus,
        arm: ParagraphArm,
        parent: ParagraphParent,
        width: gpui::AvailableSpace,
        mono: bool,
        visual: &mut gpui::VisualTestContext,
    ) -> ParagraphFrame {
        let root = Rc::new(RefCell::new(ParagraphProbeOutput::default()));
        let blocks: Vec<_> = corpus
            .texts
            .iter()
            .map(|_| Rc::new(RefCell::new(ParagraphProbeOutput::default())))
            .collect();
        let root_output = root.clone();
        visual.draw(
            point(px(11.3), px(17.7)),
            size(width, gpui::AvailableSpace::MaxContent),
            |_, _| {
                let paragraphs = corpus
                    .texts
                    .iter()
                    .zip(&corpus.pieces)
                    .zip(&blocks)
                    .enumerate()
                    .map(|(index, ((text, cached), output))| {
                        let block = match arm {
                            ParagraphArm::Production => {
                                crate::workspace_view::render_markdown_block_with(
                                    &ikore_editor::MarkdownBlock::Paragraph(text.clone()),
                                    &mut |borrowed| {
                                        assert_eq!(borrowed, cached.text.as_ref());
                                        div()
                                            .debug_selector(move || format!("fusion-text-{index}"))
                                            .min_w_0()
                                            .child(cached.element(0..0))
                                    },
                                )
                            }
                            ParagraphArm::LegacyLiteral => div()
                                .mb_3()
                                .text_sm()
                                .line_height(px(22.0))
                                .child(
                                    div()
                                        .debug_selector(move || format!("fusion-text-{index}"))
                                        .min_w_0()
                                        .child(cached.element(0..0)),
                                )
                                .into_any_element(),
                        };
                        ParagraphProbe {
                            child: block,
                            output: output.clone(),
                        }
                    });
                let message = match parent {
                    ParagraphParent::Block => div().children(paragraphs),
                    _ => div().min_w_0().flex().flex_col().children(paragraphs),
                };
                let inherited_font = gpui::Font {
                    features: if mono {
                        gpui::FontFeatures::disable_ligatures()
                    } else {
                        gpui::FontFeatures::default()
                    },
                    weight: if mono {
                        gpui::FontWeight::BOLD
                    } else {
                        gpui::FontWeight::NORMAL
                    },
                    ..gpui::font(if mono {
                        theme::FONT_MONO
                    } else {
                        theme::FONT_UI
                    })
                };
                ParagraphProbe {
                    child: paragraph_parent(parent, message.into_any_element())
                        .font(inherited_font)
                        .text_color(theme::text())
                        .into_any_element(),
                    output: root_output,
                }
            },
        );
        let leaves = corpus
            .pieces
            .iter()
            .map(|piece| {
                let state = piece.state.borrow();
                let layout = state.styled.layout();
                assert_eq!(layout.text(), piece.text.as_ref());
                assert!(layout.retained_measurement_count() <= 4);
                let carets = piece
                    .text
                    .char_indices()
                    .map(|(index, _)| index)
                    .chain(std::iter::once(piece.text.len()))
                    .map(|index| {
                        let position = layout.position_for_index(index);
                        ParagraphCaretSnapshot {
                            index,
                            position,
                            hit: position.map(|position| layout.index_for_position(position)),
                        }
                    })
                    .collect();
                ParagraphLeafSnapshot {
                    bounds: layout.bounds(),
                    text: layout.text(),
                    line_height: layout.line_height(),
                    style: state.style.clone(),
                    carets,
                }
            })
            .collect();
        let root = root.borrow();
        ParagraphFrame {
            geometry: ParagraphGeometry {
                container: root.bounds.unwrap(),
                blocks: blocks
                    .iter()
                    .map(|block| block.borrow().bounds.unwrap())
                    .collect(),
                leaves,
            },
            subtree_nodes: root.nodes,
            paragraph_nodes: blocks.iter().map(|block| block.borrow().nodes).collect(),
        }
    }

    fn paragraph_test_dpi(visual: &mut gpui::VisualTestContext, scale: f32, rem: f32) {
        let viewport = size(px(840.0), px(700.0));
        visual.simulate_resize(viewport);
        visual.simulate_scale_factor(scale);
        visual.run_until_parked();
        visual.update(|window, _| {
            assert_eq!(
                window.scale_factor(),
                scale,
                "must exercise real Window scale"
            );
            assert_eq!(window.viewport_size(), viewport);
            window.set_rem_size(px(rem));
        });
    }

    #[gpui::test]
    fn paragraph_fusion_removes_one_real_taffy_node_per_paragraph(cx: &mut TestAppContext) {
        let (_, visual) = cx.add_window_view(|_, _| Harness(String::new()));
        paragraph_test_dpi(visual, 1.25, 19.75);
        let texts: Vec<_> = (0..16)
            .map(|index| format!("Paragraph {index}: caf\u{e9} \u{1f980}"))
            .collect();
        let production = ParagraphCorpus::new(&texts);
        let legacy = ParagraphCorpus::new(&texts);
        let width = gpui::AvailableSpace::Definite(px(280.25));
        let actual = draw_paragraph_arm(
            &production,
            ParagraphArm::Production,
            ParagraphParent::FlexColumn,
            width,
            false,
            visual,
        );
        let before = draw_paragraph_arm(
            &legacy,
            ParagraphArm::LegacyLiteral,
            ParagraphParent::FlexColumn,
            width,
            false,
            visual,
        );
        assert_eq!(actual.geometry, before.geometry);
        assert!(before.paragraph_nodes.iter().all(|nodes| *nodes == 3));
        // Behavioral RED on the frozen renderer: actual currently has 3, not 2.
        assert!(actual.paragraph_nodes.iter().all(|nodes| *nodes == 2));
        assert_eq!(before.subtree_nodes, actual.subtree_nodes + texts.len());
    }

    #[gpui::test]
    fn paragraph_fusion_keeps_exact_geometry_carets_and_style_at_real_dpi(cx: &mut TestAppContext) {
        use gpui::AvailableSpace::{Definite, MaxContent, MinContent};
        let (_, visual) = cx.add_window_view(|_, _| Harness(String::new()));
        let texts: Vec<String> = [
            "",
            " ",
            "a",
            "office affine",
            "a\u{e7}\u{e3}o e\u{301} \u{1f980} \u{4e2d}\u{6587}",
            "soft\nline\nhard",
            "two words near a wrap boundary",
            "a_very_long_unbroken_word_without_spaces",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        // Cover every real scale, while pairing typography variants instead of
        // taking a large product of every style, parent and constraint.
        for (scale, rem, mono) in [
            (1.0, 16.0, false),
            (1.25, 19.75, true),
            (1.5, 16.0, false),
            (1.75, 19.75, true),
            (2.0, 16.0, false),
        ] {
            paragraph_test_dpi(visual, scale, rem);
            for parent in [
                ParagraphParent::FlexColumn,
                ParagraphParent::Block,
                ParagraphParent::Assistant,
                ParagraphParent::User85,
            ] {
                let production = ParagraphCorpus::new(&texts);
                let legacy = ParagraphCorpus::new(&texts);
                let widths = match parent {
                    ParagraphParent::Assistant => vec![Definite(px(280.25)), Definite(px(81.3))],
                    ParagraphParent::Block => vec![Definite(px(280.25)), MinContent, MaxContent],
                    _ => vec![
                        Definite(px(280.25)),
                        MinContent,
                        MaxContent,
                        Definite(px(81.3)),
                        Definite(px(280.25)),
                    ],
                };
                // Revisit a width after intrinsic constraints where that is meaningful.
                for width in widths {
                    let actual = draw_paragraph_arm(
                        &production,
                        ParagraphArm::Production,
                        parent,
                        width,
                        mono,
                        visual,
                    );
                    let before = draw_paragraph_arm(
                        &legacy,
                        ParagraphArm::LegacyLiteral,
                        parent,
                        width,
                        mono,
                        visual,
                    );
                    assert_eq!(
                        actual.geometry, before.geometry,
                        "scale={scale}, rem={rem}, parent={parent:?}, width={width:?}"
                    );
                }
            }
        }
    }

    #[gpui::test]
    fn paragraph_fusion_has_no_accumulated_height_drift_or_wrap_threshold_change(
        cx: &mut TestAppContext,
    ) {
        use gpui::AvailableSpace::{Definite, MaxContent};
        let (_, visual) = cx.add_window_view(|_, _| Harness(String::new()));
        let texts: Vec<_> = (0..256)
            .map(|index| format!("Line {index}: office caf\u{e9} \u{1f980} final"))
            .collect();
        for scale in [1.0, 1.25, 1.5, 1.75, 2.0] {
            paragraph_test_dpi(visual, scale, 19.75);
            let production = ParagraphCorpus::new(&texts);
            let legacy = ParagraphCorpus::new(&texts);
            for parent in [ParagraphParent::FlexColumn, ParagraphParent::User85] {
                let actual = draw_paragraph_arm(
                    &production,
                    ParagraphArm::Production,
                    parent,
                    Definite(px(214.3)),
                    false,
                    visual,
                );
                let before = draw_paragraph_arm(
                    &legacy,
                    ParagraphArm::LegacyLiteral,
                    parent,
                    Definite(px(214.3)),
                    false,
                    visual,
                );
                assert_eq!(
                    actual.geometry, before.geometry,
                    "accumulated geometry at scale {scale}, {parent:?}"
                );
                assert_eq!(actual.geometry.blocks.last(), before.geometry.blocks.last());
            }
            // Derive a threshold from the native measured text, not a guessed character width.
            let threshold_text = vec!["office caf\u{e9} final word".to_owned()];
            let production = ParagraphCorpus::new(&threshold_text);
            let legacy = ParagraphCorpus::new(&threshold_text);
            draw_paragraph_arm(
                &production,
                ParagraphArm::Production,
                ParagraphParent::FlexColumn,
                MaxContent,
                false,
                visual,
            );
            let layout = production.pieces[0].state.borrow().styled.layout().clone();
            let advance = layout
                .position_for_index(threshold_text[0].len())
                .unwrap()
                .x
                - layout.position_for_index(0).unwrap().x;
            let mut heights = Vec::new();
            for width in [
                advance - px(1.0 / scale),
                advance,
                advance + px(1.0 / scale),
            ] {
                let actual = draw_paragraph_arm(
                    &production,
                    ParagraphArm::Production,
                    ParagraphParent::FlexColumn,
                    Definite(width),
                    false,
                    visual,
                );
                let before = draw_paragraph_arm(
                    &legacy,
                    ParagraphArm::LegacyLiteral,
                    ParagraphParent::FlexColumn,
                    Definite(width),
                    false,
                    visual,
                );
                assert_eq!(
                    actual.geometry, before.geometry,
                    "measured threshold at scale {scale}"
                );
                heights.push(actual.geometry.leaves[0].bounds.size.height);
            }
            assert!(
                heights[0] > heights[2],
                "the test must cross an actual wrap threshold"
            );
        }
    }

    struct ParagraphCopyHarness {
        content: String,
        parent: ParagraphParent,
    }
    impl Render for ParagraphCopyHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            paragraph_parent(
                self.parent,
                SelectableMessage::new("fusion-copy", self.content.clone()).into_any_element(),
            )
        }
    }

    fn reset_paragraph_copy_state(
        visual: &mut gpui::VisualTestContext,
        start: gpui::Point<Pixels>,
    ) {
        // Prove the selection is empty before every independent gesture.
        visual.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_up(start, MouseButton::Left, Modifiers::default());
        visual.update(|_, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(
                "paragraph selection sentinel".into(),
            ));
        });
        visual.simulate_keystrokes("ctrl-c");
        visual.update(|_, cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().as_deref(),
                Some("paragraph selection sentinel")
            );
        });
    }

    #[gpui::test]
    fn paragraph_fusion_keeps_actual_message_selection_and_complete_clipboard(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::text_input::register_text_input_keybindings);
        let first = "a\u{e7}\u{e3}o \u{1f980}";
        let second = "segunda linha";
        // Expected text is the literal reference projection; no copy/selection logic is reimplemented.
        let literal_projection = format!("{first}\n\n{second}");
        let legacy = ParagraphCorpus::new(&[first.to_owned(), second.to_owned()]);
        let (harness, visual) = cx.add_window_view(|_, _| ParagraphCopyHarness {
            content: literal_projection.clone(),
            parent: ParagraphParent::User85,
        });
        for scale in [1.0, 1.25, 1.5, 1.75, 2.0] {
            paragraph_test_dpi(visual, scale, 19.75);
            for parent in [ParagraphParent::Assistant, ParagraphParent::User85] {
                harness.update(visual, |h, cx| {
                    h.parent = parent;
                    cx.notify();
                });
                for width in [420.0, 180.0, 600.0] {
                    visual.simulate_resize(size(px(width), px(700.0)));
                    visual.run_until_parked();
                    let reference = draw_paragraph_arm(
                        &legacy,
                        ParagraphArm::LegacyLiteral,
                        parent,
                        gpui::AvailableSpace::Definite(px(width)),
                        false,
                        visual,
                    );
                    let reference_projection = reference
                        .geometry
                        .leaves
                        .iter()
                        .map(|leaf| leaf.text.as_str())
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    assert_eq!(reference_projection, literal_projection);
                    visual.run_until_parked();
                    let a = visual.debug_bounds("fusion-copy:text:0").unwrap();
                    let b = visual.debug_bounds("fusion-copy:text:1").unwrap();
                    let start = a.origin + point(px(0.0), px(2.0));
                    // End in the Paragraph's bottom margin, after its final
                    // caret. A point just inside the rounded right edge can
                    // still precede the last glyph at fractional DPI.
                    let end = point(b.left() + px(2.0), b.bottom() + px(2.0));
                    for (from, to) in [(start, end), (end, start)] {
                        reset_paragraph_copy_state(visual, start);
                        visual.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
                        visual.simulate_mouse_move(to, MouseButton::Left, Modifiers::default());
                        visual.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
                        visual.simulate_keystrokes("ctrl-c");
                        visual.update(|_, cx| {
                            assert_eq!(
                                cx.read_from_clipboard().unwrap().text().as_deref(),
                                Some(reference_projection.as_str())
                            )
                        });
                    }
                    reset_paragraph_copy_state(visual, start);
                    visual.simulate_keystrokes("ctrl-a ctrl-c");
                    visual.update(|_, cx| {
                        assert_eq!(
                            cx.read_from_clipboard().unwrap().text().as_deref(),
                            Some(reference_projection.as_str())
                        )
                    });
                }
            }
        }
    }
}
