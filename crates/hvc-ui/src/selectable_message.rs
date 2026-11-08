use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    App, Bounds, ClipboardItem, Element, ElementId, FocusHandle, GlobalElementId, HighlightStyle,
    InspectorElementId, IntoElement, LayoutId, MouseButton, Pixels, RenderOnce, SharedString,
    StyledText, TextLayout, TextStyle, Window, div, prelude::*, px,
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
    Markdown(hvc_editor::MarkdownBlock),
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
                .into_any_element()
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
                    hvc_editor::MarkdownBlock::Paragraph(text),
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
        use hvc_editor::MarkdownBlock;
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
                                    &mut |text| text.to_owned().into_any_element(),
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
        use hvc_editor::MarkdownBlock;
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
                        text.to_owned().into_any_element()
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
        assert!(projection.parts.iter().any(|part| matches!(part, MessagePart::Markdown(hvc_editor::MarkdownBlock::Heading { text, .. }) if text == "Heading")));
        assert!(projection.parts.iter().any(|part| matches!(part, MessagePart::UnifiedDiff { file_path, .. } if file_path == "src/a.rs")));
        assert!(projection.refresh("Updated 🦀".into()));
        assert!(!Rc::ptr_eq(&original, &projection.parts));
        assert_eq!(
            projection.parts.as_ref(),
            &[MessagePart::Markdown(hvc_editor::MarkdownBlock::Paragraph(
                "Updated 🦀".into()
            ))]
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
}
