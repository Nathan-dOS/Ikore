use std::{cell::Cell, rc::Rc};

use gpui::{
    AnyElement, App, Bounds, CursorStyle, Entity, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, SharedString, canvas, div, prelude::*, px, relative,
};

use crate::theme;

const SASH_WIDTH: f32 = 6.0;
/// Painted width of the handle inside the gutter; the whole gutter stays the hit target.
const SASH_HANDLE_WIDTH: f32 = 2.0;
/// Keeps the handle clear of the panes' `rounded_lg` corners (8px radius).
const SASH_HANDLE_INSET: f32 = 10.0;

#[derive(Clone, Copy)]
enum WidthPreference {
    Pixels(f32),
    Fraction(f32),
}

impl WidthPreference {
    fn resolve(self, available: f32) -> f32 {
        match self {
            Self::Pixels(width) => width,
            Self::Fraction(fraction) => available * fraction,
        }
    }
}

/// Session-local width preference, kept by the owner when its view is hidden.
/// Bounds are measured in logical pixels and never persisted as user preferences.
pub(crate) struct SplitPaneState {
    default: WidthPreference,
    preferred: WidthPreference,
    min_left: f32,
    max_left: f32,
    min_right: f32,
    bounds: Rc<Cell<Bounds<Pixels>>>,
    drag_offset: Option<f32>,
}

impl SplitPaneState {
    pub(crate) fn new(
        default_left_px: f32,
        min_left_px: f32,
        max_left_px: f32,
        min_right_px: f32,
    ) -> Self {
        assert!(default_left_px.is_finite() && default_left_px >= 0.0);
        assert!(min_left_px.is_finite() && min_left_px >= 0.0);
        assert!(max_left_px >= min_left_px);
        assert!(min_right_px.is_finite() && min_right_px >= 0.0);
        Self {
            default: WidthPreference::Pixels(default_left_px),
            preferred: WidthPreference::Pixels(default_left_px),
            min_left: min_left_px,
            max_left: max_left_px,
            min_right: min_right_px,
            bounds: Rc::new(Cell::new(Bounds::default())),
            drag_offset: None,
        }
    }

    /// Content splits preserve their proportion on window resize, normally
    /// keeping each side between 20% and 80% of the available width.
    pub(crate) fn proportional(default_fraction: f32, min_left_px: f32, min_right_px: f32) -> Self {
        assert!(default_fraction.is_finite() && (0.0..=1.0).contains(&default_fraction));
        let mut state = Self::new(0.0, min_left_px, f32::INFINITY, min_right_px);
        state.default = WidthPreference::Fraction(default_fraction);
        state.preferred = state.default;
        state
    }

    fn available(total: f32) -> f32 {
        (total - SASH_WIDTH).max(0.0)
    }

    fn limits(&self, available: f32) -> (f32, f32) {
        let minimum_sum = self.min_left + self.min_right;
        if available < minimum_sum {
            // A narrow window must not force either pane outside its parent.
            let left = available * self.min_left / minimum_sum;
            return (left, left);
        }
        let mut lower = self.min_left;
        let mut upper = self.max_left.min(available - self.min_right);
        if matches!(self.default, WidthPreference::Fraction(_)) {
            upper = upper.min((available * 0.8).max(lower));
            lower = lower.max((available * 0.2).min(upper));
        }
        (lower, upper)
    }

    fn left_width(&self, total: f32) -> f32 {
        let available = Self::available(total);
        let (lower, upper) = self.limits(available);
        self.preferred.resolve(available).clamp(lower, upper)
    }

    fn resize_to(&mut self, width: f32) {
        let available = Self::available(self.bounds.get().size.width.into());
        let (lower, upper) = self.limits(available);
        let width = width.clamp(lower, upper);
        self.preferred = match self.default {
            WidthPreference::Pixels(_) => WidthPreference::Pixels(width),
            WidthPreference::Fraction(_) if available > 0.0 => {
                WidthPreference::Fraction(width / available)
            }
            WidthPreference::Fraction(_) => self.preferred,
        };
    }

    fn move_pointer(&mut self, x: Pixels) {
        if let Some(offset) = self.drag_offset {
            let bounds = self.bounds.get();
            self.resize_to(f32::from(x - bounds.left()) - offset);
        }
    }

    fn reset(&mut self) {
        self.preferred = self.default;
        self.drag_offset = None;
    }
}

/// A clipped two-column layout with a mouse-resizable sash. `id` must be unique
/// in the containing view. Child roots should fill their available pane.
pub(crate) fn split_pane(
    id: &'static str,
    state: &Entity<SplitPaneState>,
    left: AnyElement,
    right: AnyElement,
    cx: &App,
) -> AnyElement {
    let snapshot = state.read(cx);
    let bounds = snapshot.bounds.clone();
    let measured_width = f32::from(bounds.get().size.width);
    let width = snapshot.left_width(measured_width);
    let dragging = snapshot.drag_offset.is_some();
    let initial_width = match snapshot.default {
        WidthPreference::Pixels(default) => default,
        WidthPreference::Fraction(_) => snapshot.min_left,
    };
    let proportional = matches!(snapshot.default, WidthPreference::Fraction(_));
    let fraction = if measured_width > 0.0 {
        width / measured_width
    } else if let WidthPreference::Fraction(fraction) = snapshot.preferred {
        fraction
    } else {
        0.5
    };
    let sash_id = format!("{id}-sash");
    let handle_id = format!("{id}-sash-handle");
    let left_id = format!("{id}-left");
    let right_id = format!("{id}-right");
    let mouse_state = state.clone();
    let down_state = state.clone();
    let notify_state = state.clone();

    div()
        .id(id)
        .debug_selector(move || id.to_string())
        .relative()
        .flex()
        .size_full()
        .min_w_0()
        .min_h_0()
        .overflow_hidden()
        .child(
            div()
                .debug_selector(move || left_id.clone())
                .w(px(if measured_width > 0.0 {
                    width
                } else {
                    initial_width
                }))
                // Percentages resolve during this layout, including nested splits.
                // A stale measurement must never push the other pane outside on resize.
                .when(proportional, |pane| pane.w(relative(fraction)))
                .max_w(relative(0.8))
                .h_full()
                .min_w_0()
                .min_h_0()
                .when(measured_width > 0.0, |pane| pane.flex_shrink_0())
                .overflow_hidden()
                .child(left),
        )
        .child(
            // The sash is a transparent gutter the width of the hit area; only a thin handle,
            // inset from the panes' rounded corners, is painted — and only while the pointer is
            // over the gutter or a drag is in progress. A solid full-height bar used to sit
            // flush against the corner radii and read as a notch between the two cards.
            div()
                .id(SharedString::from(sash_id.clone()))
                .debug_selector(move || sash_id.clone())
                .group("split-sash")
                .w(px(SASH_WIDTH))
                .max_w_full()
                .h_full()
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .py(px(SASH_HANDLE_INSET))
                .cursor(CursorStyle::ResizeLeftRight)
                .child(
                    div()
                        .debug_selector(move || handle_id.clone())
                        .w(px(SASH_HANDLE_WIDTH))
                        .h_full()
                        .rounded_full()
                        .when(dragging, |handle| handle.bg(theme::accent()))
                        .when(!dragging, |handle| {
                            handle.group_hover("split-sash", |hovered| hovered.bg(theme::accent()))
                        }),
                )
                .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                    down_state.update(cx, |state, cx| {
                        if event.click_count == 2 {
                            state.reset();
                        } else {
                            let bounds = state.bounds.get();
                            state.drag_offset = Some(
                                f32::from(event.position.x - bounds.left())
                                    - state.left_width(bounds.size.width.into()),
                            );
                        }
                        cx.notify();
                    });
                    cx.stop_propagation();
                    window.refresh();
                }),
        )
        .child(
            div()
                .debug_selector(move || right_id.clone())
                .flex_1()
                .h_full()
                .min_w_0()
                .min_h_0()
                .overflow_hidden()
                .child(right),
        )
        .child(
            canvas(
                move |new_bounds, window, cx| {
                    let previous = bounds.replace(new_bounds);
                    if previous.size.width != new_bounds.size.width {
                        notify_state.update(cx, |_, cx| cx.notify());
                        window.refresh();
                    }
                },
                move |_, _, window, cx| {
                    if mouse_state.read(cx).drag_offset.is_some() {
                        window.set_window_cursor_style(CursorStyle::ResizeLeftRight);
                    }
                    let move_state = mouse_state.clone();
                    window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                        if !phase.capture() || move_state.read(cx).drag_offset.is_none() {
                            return;
                        }
                        move_state.update(cx, |state, cx| {
                            if event.pressed_button == Some(MouseButton::Left) {
                                state.move_pointer(event.position.x);
                            } else {
                                state.drag_offset = None;
                            }
                            cx.notify();
                        });
                        // Only a drag initiated on this sash captures movement.
                        cx.stop_propagation();
                        window.refresh();
                    });
                    let up_state = mouse_state.clone();
                    window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                        if !phase.capture()
                            || event.button != MouseButton::Left
                            || up_state.read(cx).drag_offset.is_none()
                        {
                            return;
                        }
                        up_state.update(cx, |state, cx| {
                            state.move_pointer(event.position.x);
                            state.drag_offset = None;
                            cx.notify();
                        });
                        cx.stop_propagation();
                        window.refresh();
                    });
                    window.on_mouse_event(move |_: &MouseDownEvent, phase, window, cx| {
                        // Recover from a mouse-up lost while the window/view
                        // was hidden. Normal content clicks remain untouched.
                        if phase.capture() && mouse_state.read(cx).drag_offset.is_some() {
                            mouse_state.update(cx, |state, cx| {
                                state.drag_offset = None;
                                cx.notify();
                            });
                            window.refresh();
                        }
                    });
                },
            )
            .absolute()
            .size_full(),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Context, Modifiers, Render, TestAppContext, Window, point, size};

    #[test]
    fn widths_respect_limits_and_recover_after_a_narrow_window() {
        let state = SplitPaneState::new(300.0, 180.0, 500.0, 320.0);
        assert_eq!(state.left_width(1200.0), 300.0);
        assert_eq!(state.left_width(0.0), 0.0);
        assert_eq!(state.left_width(6.0), 0.0);
        assert_eq!(state.left_width(506.0), 180.0);
        assert_eq!(state.left_width(256.0), 90.0);
        assert_eq!(state.left_width(1200.0), 300.0);
    }

    #[test]
    fn content_widths_preserve_ratios_with_absolute_minimums() {
        let state = SplitPaneState::proportional(0.65, 240.0, 200.0);
        assert_eq!(state.left_width(1006.0), 650.0);
        assert_eq!(state.left_width(2006.0), 1300.0);
        assert_eq!(state.left_width(446.0), 240.0);
        let low = SplitPaneState::proportional(0.0, 100.0, 100.0);
        let high = SplitPaneState::proportional(1.0, 100.0, 100.0);
        assert_eq!(low.left_width(1006.0), 200.0);
        assert_eq!(high.left_width(1006.0), 800.0);
    }

    struct Harness {
        split: Entity<SplitPaneState>,
        content_clicks: usize,
    }

    impl Render for Harness {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            cx.observe(&self.split, |_, _, cx| cx.notify()).detach();
            let left = div().size_full().on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, _, _, _| view.content_clicks += 1),
            );
            split_pane(
                "test-split",
                &self.split,
                left.into_any_element(),
                div().size_full().into_any_element(),
                cx,
            )
        }
    }

    #[gpui::test]
    fn dragging_captures_outside_sash_and_stops_on_release(cx: &mut TestAppContext) {
        let (view, visual) = cx.add_window_view(|_, cx| Harness {
            split: cx.new(|_| SplitPaneState::new(300.0, 180.0, 500.0, 320.0)),
            content_clicks: 0,
        });
        visual.simulate_resize(size(px(1000.0), px(500.0)));
        visual.run_until_parked();
        let sash = visual.debug_bounds("test-split-sash").unwrap();
        let start = sash.center();
        visual.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_move(
            point(px(900.0), start.y),
            MouseButton::Left,
            Modifiers::default(),
        );
        visual.run_until_parked();
        assert_eq!(
            visual.debug_bounds("test-split-left").unwrap().size.width,
            px(500.0)
        );
        visual.simulate_mouse_up(
            point(px(900.0), start.y),
            MouseButton::Left,
            Modifiers::default(),
        );
        visual.simulate_mouse_move(point(px(50.0), start.y), None, Modifiers::default());
        visual.run_until_parked();
        assert_eq!(
            visual.debug_bounds("test-split-left").unwrap().size.width,
            px(500.0)
        );
        visual.simulate_click(point(px(50.0), start.y), Modifiers::default());
        visual.run_until_parked();
        assert_eq!(view.read_with(visual, |view, _| view.content_clicks), 1);
    }

    #[gpui::test]
    fn sash_paints_a_thin_handle_inset_from_the_pane_corners(cx: &mut TestAppContext) {
        let (_, visual) = cx.add_window_view(|_, cx| Harness {
            split: cx.new(|_| SplitPaneState::new(300.0, 180.0, 500.0, 320.0)),
            content_clicks: 0,
        });
        visual.simulate_resize(size(px(1000.0), px(500.0)));
        visual.run_until_parked();
        let sash = visual.debug_bounds("test-split-sash").unwrap();
        let handle = visual.debug_bounds("test-split-sash-handle").unwrap();
        // The whole gutter stays the hit target...
        assert_eq!(sash.size.width, px(SASH_WIDTH));
        assert_eq!(sash.size.height, px(500.0));
        // ...but only a centred 2px line is painted, clear of the corner radii above and below.
        assert_eq!(handle.size.width, px(SASH_HANDLE_WIDTH));
        assert_eq!(handle.top(), sash.top() + px(SASH_HANDLE_INSET));
        assert_eq!(handle.bottom(), sash.bottom() - px(SASH_HANDLE_INSET));
        assert!((handle.center().x - sash.center().x).abs() <= px(1.0));
    }

    #[gpui::test]
    fn double_click_resets_width_and_resize_keeps_panes_inside(cx: &mut TestAppContext) {
        let (_, visual) = cx.add_window_view(|_, cx| Harness {
            split: cx.new(|_| SplitPaneState::new(300.0, 180.0, 500.0, 320.0)),
            content_clicks: 0,
        });
        visual.simulate_resize(size(px(1000.0), px(500.0)));
        visual.run_until_parked();
        let start = visual.debug_bounds("test-split-sash").unwrap().center();
        visual.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_up(
            point(start.x + px(150.0), start.y),
            MouseButton::Left,
            Modifiers::default(),
        );
        visual.run_until_parked();
        assert_eq!(
            visual.debug_bounds("test-split-left").unwrap().size.width,
            px(450.0)
        );
        let sash = visual.debug_bounds("test-split-sash").unwrap().center();
        visual.simulate_event(MouseDownEvent {
            position: sash,
            button: MouseButton::Left,
            click_count: 2,
            ..Default::default()
        });
        visual.run_until_parked();
        assert_eq!(
            visual.debug_bounds("test-split-left").unwrap().size.width,
            px(300.0)
        );
        visual.simulate_resize(size(px(256.0), px(500.0)));
        visual.simulate_resize(size(px(256.0), px(500.0)));
        visual.run_until_parked();
        let left = visual.debug_bounds("test-split-left").unwrap();
        let right = visual.debug_bounds("test-split-right").unwrap();
        assert_eq!(left.size.width, px(90.0));
        assert!(left.left() >= px(0.0));
        assert!(right.right() <= px(256.0));
        visual.simulate_resize(size(px(1000.0), px(500.0)));
        visual.simulate_resize(size(px(1000.0), px(500.0)));
        visual.run_until_parked();
        assert_eq!(
            visual.debug_bounds("test-split-left").unwrap().size.width,
            px(300.0)
        );
    }
}
