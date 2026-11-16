//! Question navigator of the Companion: a rail with one marker per question of the open
//! conversation, a preview of each exchange, and jumps from one question to another.
//!
//! The rail costs nothing per message: it knows questions by their position in the conversation.
//! The text of a preview is read from the messages only when its tooltip opens.

use std::{cell::RefCell, rc::Rc};

use gpui::{
    AnyElement, App, Bounds, Context, IntoElement, Pixels, ScrollHandle, Window, div, point,
    prelude::*, px,
};
use ikore_core::chat_service::ChatMessage;

use crate::{AppModel, theme};

/// Width of the rail, and of the gutter that balances it on the other side of the conversation.
pub(crate) const RAIL_WIDTH: f32 = 26.0;
/// Space kept above a question the navigator scrolls to.
const JUMP_MARGIN: f32 = 24.0;
/// How far down the viewport, as a share of its height, the question being read is looked for.
const READING_LINE: f32 = 0.3;
/// Characters of a question or of an answer in a preview: about three lines of the card.
const PREVIEW_CHARS: usize = 160;
/// Markers on each side of the active one that Tab reaches. Alt+Up and Alt+Down walk them all.
const TAB_REACH: usize = 2;
/// Frames a jump waits for its question to be laid out, as after the history is expanded.
const JUMP_FRAMES: u8 = 2;

/// Where the questions of a conversation sit among its messages.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct QuestionSpan {
    /// Questions in the part of the history that is not rendered.
    pub(crate) hidden: usize,
    /// Position of each rendered question among the rendered messages.
    pub(crate) rendered: Vec<usize>,
}

impl QuestionSpan {
    /// `hidden_messages` is how many messages at the start of the conversation are not rendered.
    pub(crate) fn of(messages: &[ChatMessage], hidden_messages: usize) -> Self {
        let hidden_messages = hidden_messages.min(messages.len());
        Self {
            hidden: messages[..hidden_messages]
                .iter()
                .filter(|message| is_question(message))
                .count(),
            rendered: messages[hidden_messages..]
                .iter()
                .enumerate()
                .filter(|(_, message)| is_question(message))
                .map(|(position, _)| position)
                .collect(),
        }
    }

    pub(crate) fn total(&self) -> usize {
        self.hidden + self.rendered.len()
    }
}

fn is_question(message: &ChatMessage) -> bool {
    message.role == "user"
}

/// What the last layout of the conversation told the navigator, and what it asks of the next one.
/// Tops and offsets are measured down from the start of the scrolled content.
#[derive(Debug, Default)]
pub(crate) struct QuestionLayout {
    hidden: usize,
    /// Top of each rendered question.
    tops: Vec<Pixels>,
    viewport_height: Pixels,
    /// The furthest the conversation scrolls. Zero when it fits its viewport.
    max_scroll: Pixels,
    /// Question to bring to the top once it is laid out, and the frames left to wait for it.
    pending: Option<(usize, u8)>,
    /// Question last jumped to and the scroll position that jump left. It stays the active one
    /// until the developer scrolls, even when the next question starts above the reading line.
    pinned: Option<(usize, Pixels)>,
}

impl QuestionLayout {
    /// Forgets the conversation that was laid out, when another one opens.
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn scrolls(&self) -> bool {
        self.max_scroll > px(0.5)
    }

    pub(crate) fn request_jump(&mut self, question: usize) {
        self.pending = Some((question, JUMP_FRAMES));
    }

    /// Scroll position that shows `question` under the jump margin.
    fn anchor(&self, question: usize) -> Option<Pixels> {
        let top = *self.tops.get(question.checked_sub(self.hidden)?)?;
        Some((top - px(JUMP_MARGIN)).max(px(0.0)).min(self.max_scroll))
    }

    /// The question being read at `scroll_top`: the last one that starts at or above the reading
    /// line. At the end of the conversation it is the last question, which may never reach the line.
    pub(crate) fn active(&self, scroll_top: Pixels) -> Option<usize> {
        if let Some((question, position)) = self.pinned
            && (position - scroll_top).abs() < px(0.5)
        {
            return Some(question);
        }
        let last = self.tops.len().checked_sub(1)?;
        if self.scrolls() && scroll_top >= self.max_scroll - px(0.5) {
            return Some(self.hidden + last);
        }
        let line = scroll_top + self.viewport_height * READING_LINE;
        match self.tops.iter().rposition(|top| *top <= line) {
            Some(position) => Some(self.hidden + position),
            // Above the first rendered question: the tail of a hidden one's answer, if any.
            None => Some(self.hidden.saturating_sub(1)),
        }
    }

    /// The question a step from `scroll_top` lands on. Stepping back from the first rendered
    /// question reaches the hidden history, which the jump then expands.
    pub(crate) fn step(&self, scroll_top: Pixels, forward: bool) -> Option<usize> {
        let mut anchors = (0..self.tops.len()).filter_map(|position| {
            let question = self.hidden + position;
            Some((question, self.anchor(question)?))
        });
        if forward {
            anchors
                .find(|(_, anchor)| *anchor > scroll_top + px(1.0))
                .map(|(question, _)| question)
        } else {
            anchors
                .rev()
                .find(|(_, anchor)| *anchor < scroll_top - px(1.0))
                .map(|(question, _)| question)
                .or_else(|| self.hidden.checked_sub(1))
        }
    }

    /// Takes in a fresh layout and resolves a waiting jump. Returns the scroll position to move
    /// to, if any, and whether the conversation has to be drawn again: only when what the rail
    /// shows changed, so a reply that streams does not cost a second frame per token.
    fn measured(
        &mut self,
        hidden: usize,
        tops: Vec<Pixels>,
        viewport_height: Pixels,
        max_scroll: Pixels,
        scroll_top: Pixels,
    ) -> (Option<Pixels>, bool) {
        let before = (self.scrolls(), self.active(scroll_top));
        self.hidden = hidden;
        self.tops = tops;
        self.viewport_height = viewport_height;
        self.max_scroll = max_scroll;
        let mut redraw = before != (self.scrolls(), self.active(scroll_top));
        let mut jump = None;
        if let Some((question, frames)) = self.pending.take() {
            match self.anchor(question) {
                Some(anchor) => {
                    self.pinned = Some((question, anchor));
                    jump = Some(anchor);
                    redraw = true;
                }
                None if frames > 0 => {
                    self.pending = Some((question, frames - 1));
                    redraw = true;
                }
                None => {}
            }
        }
        (jump, redraw)
    }
}

/// Listener for the prepaint of the messages column: measures where the rendered questions are
/// and carries out a jump that was waiting for them. `view` is drawn again when the rail changed.
pub(crate) fn measure_questions(
    view: gpui::EntityId,
    layout: Rc<RefCell<QuestionLayout>>,
    scroll: ScrollHandle,
    span: QuestionSpan,
    // Children of the column that come before its first message.
    leading_children: usize,
) -> impl Fn(Vec<Bounds<Pixels>>, &mut Window, &mut App) + 'static {
    move |children, _, cx| {
        let viewport = scroll.bounds();
        let offset = scroll.offset();
        let tops = span
            .rendered
            .iter()
            .filter_map(|position| children.get(leading_children + position))
            .map(|bounds| bounds.top() - viewport.top() - offset.y)
            .collect();
        let (jump, redraw) = layout.borrow_mut().measured(
            span.hidden,
            tops,
            viewport.size.height,
            scroll.max_offset().height,
            -offset.y,
        );
        if let Some(top) = jump {
            scroll.set_offset(point(offset.x, -top));
        }
        if redraw {
            // A view cannot be notified while it is drawn; the notice waits for this frame to end.
            cx.defer(move |cx| cx.notify(view));
        }
    }
}

/// How the answer to a question ended, or that it has not ended yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnswerStatus {
    Missing,
    Running,
    Completed,
    Cancelled,
    Failed,
}

impl AnswerStatus {
    fn of(answer: Option<&ChatMessage>) -> Self {
        match answer.map(|answer| answer.status.as_str()) {
            None => Self::Missing,
            Some("pending" | "streaming") => Self::Running,
            Some("completed") => Self::Completed,
            Some("cancelled") => Self::Cancelled,
            Some(_) => Self::Failed,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Missing => "no answer",
            Self::Running => "answering…",
            Self::Completed => "✓",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }
}

/// The start of one question and of its answer, for the marker's tooltip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExchangePreview {
    asked_at: String,
    question: String,
    answer: String,
    /// Model, effort and route of the answer, as written under it in the conversation.
    model_line: Option<String>,
    status: AnswerStatus,
}

impl ExchangePreview {
    /// The `question`-th question of the conversation and the last answer given before the next
    /// question.
    pub(crate) fn of(messages: &[ChatMessage], question: usize) -> Option<Self> {
        let start = messages
            .iter()
            .enumerate()
            .filter(|(_, message)| is_question(message))
            .nth(question)?
            .0;
        let asked = &messages[start];
        let answer = messages[start + 1..]
            .iter()
            .take_while(|message| !is_question(message))
            .filter(|message| message.role == "assistant")
            .last();
        Some(Self {
            asked_at: crate::time::format_timestamp(&asked.created_at),
            question: excerpt(&asked.content),
            answer: answer
                .map(
                    |answer| match (&answer.error_message, answer.content.is_empty()) {
                        (Some(error), true) => excerpt(error),
                        _ => excerpt(&answer.content),
                    },
                )
                .unwrap_or_default(),
            model_line: answer.and_then(crate::chat_view::message_model_line),
            status: AnswerStatus::of(answer),
        })
    }
}

/// The first `PREVIEW_CHARS` characters of `text` on one line. It reads only what it keeps, so a
/// long answer costs no more than a short one.
fn excerpt(text: &str) -> String {
    let mut kept = String::new();
    let mut length = 0;
    for word in text.split_whitespace() {
        let separator = usize::from(length > 0);
        let word_length = word.chars().count();
        if length + separator + word_length > PREVIEW_CHARS {
            let room = PREVIEW_CHARS.saturating_sub(length + separator);
            if separator == 1 && room > 0 {
                kept.push(' ');
            }
            kept.extend(word.chars().take(room));
            kept.push('…');
            return kept;
        }
        if separator == 1 {
            kept.push(' ');
        }
        kept.push_str(word);
        length += separator + word_length;
    }
    kept
}

/// The tooltip of a marker.
struct QuestionPreview(Option<ExchangePreview>);

impl gpui::Render for QuestionPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let card = div()
            .debug_selector(|| "chat-question-preview".into())
            .w(px(320.0))
            .p_2()
            .rounded_md()
            .border_1()
            .border_color(theme::border())
            .bg(theme::surface_raised())
            .font_family(theme::FONT_UI)
            .text_xs()
            .text_color(theme::text())
            .flex()
            .flex_col()
            .gap_1();
        let Some(preview) = self.0.clone() else {
            return card.child("This question is no longer in the conversation.");
        };
        let caption = |text: String| div().text_color(theme::text_muted()).child(text);
        let answer_caption = [
            Some("Answer".to_owned()),
            preview.model_line,
            Some(preview.status.label().to_owned()),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        card.child(caption(format!("Question · {}", preview.asked_at)))
            .child(div().min_w_0().child(preview.question))
            .child(caption(answer_caption))
            .when(!preview.answer.is_empty(), |card| {
                card.child(div().min_w_0().child(preview.answer))
            })
    }
}

/// The rail: one marker per question, the one being read drawn wider. Hovering or focusing a
/// marker previews its exchange; clicking it brings the question to the top.
pub(crate) fn question_rail(
    total: usize,
    active: Option<usize>,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let model = cx.entity().downgrade();
    div()
        .debug_selector(|| "chat-question-rail".into())
        .w(px(RAIL_WIDTH))
        .flex_shrink_0()
        .min_h_0()
        .overflow_hidden()
        .flex()
        .flex_col()
        .justify_center()
        .children((0..total).map(|question| {
            let reading = active == Some(question);
            let near = active.is_some_and(|active| active.abs_diff(question) <= TAB_REACH);
            let model = model.clone();
            div()
                .id(("chat-question", question))
                .debug_selector(move || format!("chat-question:{question}"))
                .group("chat-question")
                .w_full()
                .flex_1()
                .min_h(px(3.0))
                .max_h(px(14.0))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .rounded_sm()
                .focus_visible(|style| style.bg(theme::accent_soft()))
                // Tab reaches the questions around the one being read, not hundreds of them.
                .when(near, |marker| marker.tab_index(0))
                .child(
                    div()
                        .h(px(2.0))
                        .w(px(if reading { 18.0 } else { 13.0 }))
                        .rounded_full()
                        .bg(if reading {
                            theme::accent()
                        } else {
                            theme::text_subtle()
                        })
                        .group_hover("chat-question", |style| style.bg(theme::accent())),
                )
                .tooltip(move |_, cx| {
                    let preview = model.upgrade().and_then(|model| {
                        let chat = model.read(cx).chat.read(cx);
                        ExchangePreview::of(chat.active_messages(), question)
                    });
                    cx.new(|_| QuestionPreview(preview)).into()
                })
                .on_click(cx.listener(move |model, _, _, cx| {
                    model.jump_to_chat_question(question, cx);
                }))
                .on_key_down(
                    cx.listener(move |model, event: &gpui::KeyDownEvent, _, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            model.jump_to_chat_question(question, cx);
                            cx.stop_propagation();
                        }
                    }),
                )
        }))
        .into_any_element()
}

impl AppModel {
    /// Brings a question of the open conversation to the top of the chat. A question in the
    /// hidden part of the history expands it first.
    pub(crate) fn jump_to_chat_question(&mut self, question: usize, cx: &mut Context<Self>) {
        self.chat.update(cx, |chat, _| {
            let (_, hidden_messages) = chat.rendered_messages();
            let span = QuestionSpan::of(chat.active_messages(), hidden_messages);
            if question < span.hidden {
                chat.history_expanded = chat.active_conversation.clone();
            }
        });
        self.chat_question_layout
            .borrow_mut()
            .request_jump(question);
        cx.notify();
    }

    /// Goes to the question after or before the top of the chat.
    pub(crate) fn step_chat_question(&mut self, forward: bool, cx: &mut Context<Self>) {
        let scroll_top = -self.chat_scroll.offset().y;
        let target = self.chat_question_layout.borrow().step(scroll_top, forward);
        if let Some(question) = target {
            self.jump_to_chat_question(question, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(id: &str, role: &str, content: &str, status: &str) -> ChatMessage {
        ChatMessage {
            id: id.into(),
            conversation_id: "conversation".into(),
            ordinal: 0,
            role: role.into(),
            content: content.into(),
            status: status.into(),
            execution_id: None,
            generation_id: None,
            last_sequence: 0,
            input_tokens: None,
            output_tokens: None,
            estimated_cost_usd: None,
            error_code: None,
            error_message: None,
            target_label: None,
            model_evidence: None,
            created_at: "now".into(),
            updated_at: "now".into(),
        }
    }

    fn conversation() -> Vec<ChatMessage> {
        vec![
            message(
                "q0",
                "user",
                "Why did  the pulsation\nstop at Test?",
                "completed",
            ),
            message("a0", "assistant", "Criterion C3 failed.", "completed"),
            message("q1", "user", "And the fix?", "completed"),
            message("a1-first", "assistant", "", "failed"),
            message("a1", "assistant", "Working on it", "streaming"),
            message("q2", "user", "Still there?", "completed"),
        ]
    }

    fn layout(tops: &[f32], hidden: usize, viewport: f32, max_scroll: f32) -> QuestionLayout {
        QuestionLayout {
            hidden,
            tops: tops.iter().copied().map(px).collect(),
            viewport_height: px(viewport),
            max_scroll: px(max_scroll),
            ..Default::default()
        }
    }

    #[test]
    fn the_span_counts_hidden_questions_and_places_the_rendered_ones() {
        let messages = conversation();
        assert_eq!(
            QuestionSpan::of(&messages, 0),
            QuestionSpan {
                hidden: 0,
                rendered: vec![0, 2, 5],
            }
        );
        // Three messages are hidden: two questions, and the third starts the rendered tail.
        let tail = QuestionSpan::of(&messages, 3);
        assert_eq!(
            tail,
            QuestionSpan {
                hidden: 2,
                rendered: vec![2],
            }
        );
        assert_eq!(tail.total(), 3);
        assert_eq!(QuestionSpan::of(&messages, 99).total(), 3);
        assert_eq!(QuestionSpan::of(&[], 0).total(), 0);
    }

    #[test]
    fn a_preview_pairs_a_question_with_its_last_answer() {
        let messages = conversation();
        let first = ExchangePreview::of(&messages, 0).unwrap();
        // Line breaks and runs of spaces read as one line.
        assert_eq!(first.question, "Why did the pulsation stop at Test?");
        assert_eq!(first.answer, "Criterion C3 failed.");
        assert_eq!(first.status, AnswerStatus::Completed);
        assert_eq!(first.asked_at, "now");
        assert_eq!(first.model_line, None);

        // A retried turn previews the answer that is running, not the one that failed.
        let second = ExchangePreview::of(&messages, 1).unwrap();
        assert_eq!(second.answer, "Working on it");
        assert_eq!(second.status, AnswerStatus::Running);

        let third = ExchangePreview::of(&messages, 2).unwrap();
        assert_eq!(third.answer, "");
        assert_eq!(third.status, AnswerStatus::Missing);
        assert_eq!(ExchangePreview::of(&messages, 3), None);

        let mut failed = conversation();
        failed[1].content.clear();
        failed[1].status = "failed".into();
        failed[1].error_message = Some("The CLI returned an invalid envelope".into());
        failed[1].target_label = Some("claude / sonnet / high".into());
        let preview = ExchangePreview::of(&failed, 0).unwrap();
        assert_eq!(preview.answer, "The CLI returned an invalid envelope");
        assert_eq!(preview.status, AnswerStatus::Failed);
        assert_eq!(
            preview.model_line.as_deref(),
            Some("requested: claude / sonnet / high")
        );
    }

    #[test]
    fn an_excerpt_stops_at_the_preview_length() {
        assert_eq!(excerpt("  short\n\ntext  "), "short text");
        let long = "word ".repeat(100);
        let cut = excerpt(&long);
        assert!(cut.starts_with("word word "));
        assert!(cut.chars().count() <= PREVIEW_CHARS + 1, "{cut}");
        assert!(cut.ends_with('…'));
        // One word longer than the preview is cut inside the word.
        let cut = excerpt(&"x".repeat(500));
        assert_eq!(cut.chars().count(), PREVIEW_CHARS + 1);
        assert_eq!(excerpt(""), "");
    }

    #[test]
    fn the_active_question_is_the_last_one_above_the_reading_line() {
        // Viewport of 1000: the reading line is 300 below the top of the viewport.
        let layout = layout(&[8.0, 900.0, 2400.0], 0, 1000.0, 3000.0);
        assert_eq!(layout.active(px(0.0)), Some(0));
        assert_eq!(layout.active(px(599.0)), Some(0));
        assert_eq!(layout.active(px(600.0)), Some(1));
        assert_eq!(layout.active(px(2100.0)), Some(2));
        // At the end the last question is the one being read, wherever it sits.
        let short_end = self::layout(&[8.0, 900.0, 2900.0], 0, 1000.0, 2000.0);
        assert_eq!(short_end.active(px(1999.0)), Some(1));
        assert_eq!(short_end.active(px(2000.0)), Some(2));
        // With history hidden, questions keep their number in the whole conversation.
        let tail = self::layout(&[500.0, 1500.0], 4, 1000.0, 3000.0);
        assert_eq!(tail.active(px(0.0)), Some(3));
        assert_eq!(tail.active(px(300.0)), Some(4));
        assert_eq!(tail.active(px(1300.0)), Some(5));
        assert_eq!(QuestionLayout::default().active(px(0.0)), None);
    }

    #[test]
    fn a_jump_scrolls_under_the_margin_and_pins_its_question() {
        let mut layout = layout(&[8.0, 300.0, 500.0, 2400.0], 0, 1000.0, 3000.0);
        layout.request_jump(1);
        let tops = layout.tops.clone();
        let (jump, redraw) = layout.measured(0, tops, px(1000.0), px(3000.0), px(0.0));
        assert_eq!(jump, Some(px(276.0)));
        assert!(redraw);
        // The next question already starts above the reading line; the one jumped to stays active.
        assert_eq!(layout.active(px(276.0)), Some(1));
        // Scrolling by hand lets go of it.
        assert_eq!(layout.active(px(277.0)), Some(2));

        // A question near the end cannot reach the top: the jump stops at the end.
        layout.request_jump(3);
        let tops = layout.tops.clone();
        let (jump, _) = layout.measured(0, tops, px(1000.0), px(2000.0), px(276.0));
        assert_eq!(jump, Some(px(2000.0)));
    }

    #[test]
    fn a_jump_waits_for_a_question_that_is_not_laid_out_yet() {
        let mut layout = layout(&[100.0, 900.0], 3, 1000.0, 3000.0);
        // Question 1 is in the hidden history: nothing to scroll to yet.
        layout.request_jump(1);
        let tops = layout.tops.clone();
        let (jump, redraw) = layout.measured(3, tops, px(1000.0), px(3000.0), px(0.0));
        assert_eq!(jump, None);
        assert!(redraw, "the jump asks for the frame that will lay it out");
        // The history expanded: every question is rendered now.
        let expanded: Vec<Pixels> = [8.0, 700.0, 1400.0, 2100.0, 2800.0].map(px).into();
        let (jump, _) = layout.measured(0, expanded, px(1000.0), px(4000.0), px(0.0));
        assert_eq!(jump, Some(px(676.0)));
        assert_eq!(layout.pending, None);

        // A question that never appears is given up after the frames it may wait.
        layout.request_jump(40);
        for _ in 0..=JUMP_FRAMES {
            let tops = layout.tops.clone();
            assert_eq!(
                layout
                    .measured(0, tops, px(1000.0), px(4000.0), px(676.0))
                    .0,
                None
            );
        }
        assert_eq!(layout.pending, None);
    }

    #[test]
    fn a_measure_asks_for_another_frame_only_when_the_rail_changes() {
        let mut layout = layout(&[8.0, 900.0], 0, 1000.0, 3000.0);
        // A reply that streams makes the conversation longer; the rail shows the same thing.
        let tops = layout.tops.clone();
        let (jump, redraw) = layout.measured(0, tops, px(1000.0), px(3400.0), px(100.0));
        assert_eq!((jump, redraw), (None, false));
        // A new question that becomes the one being read is a change.
        let tops = vec![px(8.0), px(900.0), px(1000.0)];
        let (_, redraw) = layout.measured(0, tops, px(1000.0), px(3400.0), px(800.0));
        assert!(redraw);
        // So is a conversation that stops scrolling.
        let tops = layout.tops.clone();
        let (_, redraw) = layout.measured(0, tops, px(1000.0), px(0.0), px(0.0));
        assert!(redraw);
        assert!(!layout.scrolls());
    }

    #[test]
    fn a_step_goes_to_the_question_after_or_before_the_top() {
        let layout = layout(&[8.0, 900.0, 2400.0, 2950.0], 0, 1000.0, 2500.0);
        // Anchors: 0, 876, 2376, and 2500 for the last one, which stops at the end.
        assert_eq!(layout.step(px(0.0), true), Some(1));
        assert_eq!(layout.step(px(876.0), true), Some(2));
        assert_eq!(layout.step(px(876.0), false), Some(0));
        assert_eq!(layout.step(px(1000.0), false), Some(1));
        assert_eq!(layout.step(px(2376.0), true), Some(3));
        assert_eq!(layout.step(px(2500.0), true), None);
        assert_eq!(layout.step(px(2500.0), false), Some(2));
        assert_eq!(layout.step(px(0.0), false), None);
        // Before the first rendered question comes the hidden history.
        let tail = self::layout(&[8.0, 900.0], 5, 1000.0, 2500.0);
        assert_eq!(tail.step(px(0.0), false), Some(4));
        assert_eq!(tail.step(px(0.0), true), Some(6));
        assert_eq!(tail.step(px(900.0), false), Some(6));
    }
}
