//! Session-only notifications. Presentation never participates in the shell layout.
use crate::{
    AppModel,
    session::{HeaderNotice, HeaderNoticeKind, HeaderNoticeQueue},
    theme,
};
use gpui::{
    AnyElement, Bounds, Context, FocusHandle, IntoElement, Pixels, Task, Window, deferred, div,
    point, prelude::*, px, size,
};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

const FADE: Duration = Duration::from_millis(180);
const DISPLAY: Duration = Duration::from_secs(7);

#[derive(Default)]
pub(crate) struct ToastClock {
    id: Option<u64>,
    elapsed: Duration,
    exiting: Option<Duration>,
}
impl ToastClock {
    fn advance(
        &mut self,
        id: Option<u64>,
        delta: Duration,
        blocked: bool,
        paused: bool,
        reduced: bool,
    ) -> (bool, f32) {
        if self.id != id {
            self.id = None;
            self.elapsed = Duration::ZERO;
            self.exiting = None;
        }
        if id.is_none() || (self.id.is_none() && (blocked || paused)) {
            return (false, 0.0);
        }
        self.id = id;
        if blocked {
            return (false, 0.0);
        }
        if !paused {
            self.elapsed += delta;
        }
        if self.elapsed >= DISPLAY && self.exiting.is_none() {
            self.exiting = Some(Duration::ZERO);
        }
        if let Some(exit) = &mut self.exiting {
            *exit += delta;
            let progress = if reduced {
                1.0
            } else {
                (exit.as_secs_f32() / FADE.as_secs_f32()).min(1.0)
            };
            return (progress >= 1.0, 1.0 - progress);
        }
        (
            false,
            if reduced {
                1.0
            } else {
                (self.elapsed.as_secs_f32() / FADE.as_secs_f32()).min(1.0)
            },
        )
    }
    fn dismiss(&mut self) {
        self.exiting = Some(Duration::ZERO);
    }
}

#[derive(Default)]
pub(crate) struct Notifications {
    pub(crate) queue: HeaderNoticeQueue,
    pub(crate) history_open: bool,
    clock: ToastClock,
    last_tick: Option<Instant>,
    pub(crate) wake: Option<Task<()>>,
    pub(crate) focus: Option<FocusHandle>,
    return_focus: Option<FocusHandle>,
    project: Option<String>,
    errors: HashMap<&'static str, String>,
    pub(crate) copied: HashMap<String, Instant>,
}

/// Where the toast and the history panel hang from: the bell. The header lays the bell out as
/// the last 36 px control inside a 12 px padding, so the overlay's right edge meets the bell's
/// left edge and its top sits a few pixels under the header line — the panel reads as a menu
/// dropping from the bell's bottom-left corner instead of a card floating mid-screen.
pub(crate) fn overlay_right_inset() -> Pixels {
    px(12.0) + crate::widgets::ICON_BUTTON_SIZE
}

pub(crate) fn overlay_top() -> Pixels {
    crate::sizing::HEADER_HEIGHT + px(4.0)
}

impl AppModel {
    pub(crate) fn collect_notifications(&mut self, cx: &mut Context<Self>) {
        let project = self
            .workspace
            .read(cx)
            .project_id
            .as_ref()
            .map(ToString::to_string);
        if self.notifications.project != project {
            self.notifications.project = project.clone();
            self.notifications.errors.clear();
            self.notifications.history_open = false;
            self.notifications.clock = ToastClock::default();
            self.close_dropdown(cx);
        }
        self.notifications.queue.set_project(project);
        let notices = [
            ("Agents", self.agents.update(cx, |m, _| m.notice.take())),
            (
                "Settings",
                self.settings
                    .update(cx, |m, _| m.notice.take().map(|s| s.to_string())),
            ),
            (
                "Pulsations",
                self.pulsations.update(cx, |m, _| m.notice.take()),
            ),
            ("Vault", self.vault.update(cx, |m, _| m.notice.take())),
            ("Forge", self.forge.notice.take()),
        ];
        // The Settings modal shows its own outcome in a status line; its notices still reach the
        // history, just without a toast over the modal.
        let settings_open = self.settings_modal.open;
        for (source, message) in notices {
            if let Some(message) = message {
                if settings_open && source == "Settings" {
                    self.notifications.queue.push_quiet(
                        source.into(),
                        HeaderNoticeKind::Notice,
                        message,
                    );
                } else {
                    self.notifications.queue.push_from(
                        source.into(),
                        HeaderNoticeKind::Notice,
                        message,
                        None,
                    );
                }
            }
        }
        let errors = [
            (
                "Logs",
                self.logs
                    .read(cx)
                    .error
                    .as_ref()
                    .map(|e| e.presentable().to_string()),
            ),
            (
                "Chat",
                self.chat
                    .read(cx)
                    .error
                    .as_ref()
                    .map(|e| e.presentable().to_string()),
            ),
            (
                "Agents",
                self.agents
                    .read(cx)
                    .error
                    .as_ref()
                    .map(|e| e.presentable().to_string()),
            ),
            (
                "Settings",
                self.settings
                    .read(cx)
                    .error
                    .as_ref()
                    .map(|e| e.presentable().to_string()),
            ),
            ("Pulsations", self.pulsations.read(cx).error.clone()),
            ("Vault", self.vault.read(cx).error.clone()),
            ("Forge", self.forge.error.clone()),
        ];
        for (source, error) in errors {
            if let Some(error) = error {
                if self.notifications.errors.get(source) != Some(&error) {
                    if settings_open && source == "Settings" {
                        self.notifications.queue.push_quiet(
                            source.into(),
                            HeaderNoticeKind::Error,
                            error.clone(),
                        );
                    // Some command handlers also publish this error globally. Keep a single card.
                    } else if !self.notifications.queue.has_pending_global_error(&error) {
                        self.notifications.queue.push_from(
                            source.into(),
                            HeaderNoticeKind::Error,
                            error.clone(),
                            None,
                        );
                    }
                    self.notifications.errors.insert(source, error);
                }
            } else {
                self.notifications.errors.remove(source);
            }
        }
        self.notifications
            .copied
            .retain(|_, when| when.elapsed() < Duration::from_secs(3));
    }

    pub(crate) fn toggle_notification_history(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.notifications.history_open {
            self.notifications.history_open = false;
            if let Some(focus) = self.notifications.return_focus.take() {
                focus.focus(window);
            }
        } else {
            self.close_dropdown(cx);
            self.notifications.return_focus = window.focused(cx);
            self.notifications.history_open = true;
            self.notifications.queue.mark_read();
            self.notifications.clock = ToastClock::default();
            self.notifications
                .focus
                .get_or_insert_with(|| cx.focus_handle())
                .focus(window);
        }
        cx.notify();
    }

    pub(crate) fn activate_notification(&mut self, notice: &HeaderNotice, cx: &mut Context<Self>) {
        let project = self
            .workspace
            .read(cx)
            .project_id
            .as_ref()
            .map(ToString::to_string);
        let Some(action) = &notice.action else {
            return;
        };
        if notice.project != project {
            return;
        }
        let state = self.pulsations.read(cx);
        let valid = state.project_id.as_ref().map(ToString::to_string) == project
            && state
                .hitl_checkpoints
                .iter()
                .any(|c| c.pulsation_id == action.pulsation_id)
            && state.pulsations.iter().any(|p| {
                p.id == action.pulsation_id
                    && p.status == hvc_core::persistence_service::PulsationStatus::Checkpoint
            });
        if valid {
            self.notifications.history_open = false;
            self.open_pulsation_review(action.pulsation_id.clone(), cx);
            self.notifications.clock.dismiss();
        } else {
            self.push_header_notice(
                HeaderNoticeKind::Notice,
                "This review is no longer pending.",
                cx,
            );
        }
    }

    pub(crate) fn notification_overlay(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let viewport = window.viewport_size();
        let right_inset = overlay_right_inset();
        let top = overlay_top();
        let width = px(360.0).min(viewport.width - right_inset - px(12.0));
        let area = Bounds::new(
            point(viewport.width - width - right_inset, top),
            size(width, px(224.0)),
        );
        let blocked = self.active_dropdown.is_some()
            || self.forge.new_open
            || self.settings_modal.open
            || self.agents.read(cx).active_model_picker.is_some();
        if self.notifications.history_open && !blocked {
            let focus = self
                .notifications
                .focus
                .get_or_insert_with(|| cx.focus_handle())
                .clone();
            let history = self.notifications.queue.history();
            let panel = div()
                .id("notification-history")
                .debug_selector(|| "notification-history".into())
                .track_focus(&focus)
                .w(width)
                .max_h((viewport.height - top - px(40.0)).max(px(100.0)))
                .overflow_y_scroll()
                .p_3()
                .rounded_lg()
                .shadow_lg()
                .bg(theme::surface_raised())
                .border_1()
                .border_color(theme::border())
                .occlude()
                .on_mouse_down_out(cx.listener(|m, _, window, cx| {
                    m.toggle_notification_history(window, cx);
                    cx.stop_propagation();
                }))
                .on_key_down(cx.listener(|m, event: &gpui::KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" {
                        m.toggle_notification_history(window, cx);
                        cx.stop_propagation();
                    }
                }))
                .child(
                    div()
                        .flex()
                        .justify_between()
                        .items_center()
                        .child("Notifications")
                        .child(crate::widgets::icon_button(
                            "notifications-close",
                            crate::icons::Icon::X,
                            crate::widgets::Tooltip::new("Close").with_shortcut("escape"),
                            false,
                            None,
                            true,
                            cx,
                            |m, window, cx| m.toggle_notification_history(window, cx),
                        )),
                )
                .when(history.is_empty(), |d| {
                    d.child(
                        div()
                            .py_3()
                            .text_color(theme::text_muted())
                            .child("No notifications in this project"),
                    )
                })
                .children(history.iter().map(|n| notification_card(n, true, cx)));
            return deferred(div().absolute().top(top).right(right_inset).child(panel))
                .with_priority(15)
                .into_any_element();
        }
        let now = Instant::now();
        let delta = self
            .notifications
            .last_tick
            .replace(now)
            .map(|last| now - last)
            .unwrap_or_default()
            .min(Duration::from_millis(100));
        let current = self.notifications.queue.current().cloned();
        let focused = self
            .notifications
            .focus
            .as_ref()
            .is_some_and(|f| f.contains_focused(window, cx));
        let reduced = crate::motion::reduced_motion();
        let (done, opacity) = self.notifications.clock.advance(
            current.as_ref().map(|n| n.id),
            delta,
            blocked || self.notifications.history_open,
            area.contains(&window.mouse_position()) || focused,
            reduced,
        );
        if done {
            self.notifications.queue.dismiss();
        }
        if (current.is_some() || !self.notifications.copied.is_empty())
            && self.notifications.wake.is_none()
        {
            let timer = cx.background_executor().clone();
            let duration = if !reduced && opacity > 0.0 && opacity < 1.0 {
                Duration::from_millis(16)
            } else {
                Duration::from_millis(100)
            };
            self.notifications.wake = Some(cx.spawn(async move |this, cx| {
                timer.timer(duration).await;
                let _ = this.update(cx, |m, cx| {
                    m.notifications.wake = None;
                    cx.notify();
                });
            }));
        }
        if opacity == 0.0 || done {
            return div().absolute().into_any_element();
        }
        self.notifications.queue.pinned = current.as_ref().map(|n| n.id);
        let Some(notice) = current else {
            return div().absolute().into_any_element();
        };
        let focus = self
            .notifications
            .focus
            .get_or_insert_with(|| cx.focus_handle())
            .clone();
        deferred(
            div()
                .id("notification-toast")
                .debug_selector(|| "notification-toast".into())
                .track_focus(&focus)
                .absolute()
                .right(right_inset)
                .top(top - px(if reduced { 0.0 } else { 8.0 * (1.0 - opacity) }))
                .w(width)
                .opacity(opacity)
                .shadow_lg()
                .occlude()
                .child(notification_card(&notice, false, cx)),
        )
        .with_priority(5)
        .into_any_element()
    }
}

fn notification_card(
    notice: &HeaderNotice,
    history: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let color = match notice.kind {
        HeaderNoticeKind::Notice => theme::accent(),
        HeaderNoticeKind::Warning => theme::warning(),
        HeaderNoticeKind::Error => theme::danger(),
    };
    let id = notice.id;
    let action_notice = notice.clone();
    div()
        .id(gpui::SharedString::from(format!("notification-{id}")))
        .p_3()
        .rounded_lg()
        .bg(theme::surface_raised())
        .border_1()
        .border_color(theme::border())
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .justify_between()
                .child(div().text_sm().text_color(color).child(format!(
                    "{}{}",
                    notice.kind.label(),
                    if notice.repeats > 1 {
                        format!(" ×{}", notice.repeats)
                    } else {
                        String::new()
                    }
                )))
                .when(!history, |d| {
                    d.child(crate::widgets::icon_button(
                        format!("notice-close-{id}"),
                        crate::icons::Icon::X,
                        "Dismiss",
                        false,
                        None,
                        true,
                        cx,
                        |m, _, cx| {
                            m.notifications.clock.dismiss();
                            cx.notify();
                        },
                    ))
                }),
        )
        .child(
            div()
                .id(gpui::SharedString::from(format!("notice-message-{id}")))
                .text_sm()
                .max_h(if history { px(10000.0) } else { px(80.0) })
                .overflow_y_scroll()
                .child(notice.message.clone()),
        )
        .when(history, |d| {
            d.child(
                div()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(crate::time::format_system_time(notice.created)),
            )
        })
        .when_some(notice.action.clone(), |d, a| {
            d.child(crate::widgets::btn(
                format!("notice-action-{id}"),
                a.label,
                crate::widgets::ButtonVariant::Secondary,
                true,
                cx,
                move |m, cx| m.activate_notification(&action_notice, cx),
            ))
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[gpui::test]
    fn visible_cards_keep_geometry_and_body_click_does_not_dismiss(cx: &mut gpui::TestAppContext) {
        let (model, visual) = cx.add_window_view(|_, cx| crate::layout_tests::fixture(cx));
        for (width, height) in [(960.0, 640.0), (1440.0, 900.0)] {
            visual.simulate_resize(size(px(width), px(height)));
            model.update(visual, |m, cx| m.select_view(crate::AppView::Agents, cx));
            visual.run_until_parked();
            let controls = visual.debug_bounds("agents-cli-scroll").unwrap();
            let nav = visual.debug_bounds("rail-settings").unwrap();
            model.update(visual, |m, cx| {
                m.push_header_notice(
                    HeaderNoticeKind::Error,
                    "A long diagnostic. ".repeat(60),
                    cx,
                );
                let id = m.notifications.queue.current().map(|n| n.id);
                m.notifications.clock.advance(id, FADE, false, false, false);
            });
            visual.run_until_parked();
            let toast = visual.debug_bounds("notification-toast").unwrap();
            assert!(toast.right() <= px(width) && toast.bottom() <= px(height));
            assert!(toast.size.width <= px(360.0));
            // The toast hangs from the bell: top-right corner at the bell's bottom-left.
            let bell = visual.debug_bounds("notifications").unwrap();
            assert_eq!(toast.right(), bell.left(), "{toast:?} vs {bell:?}");
            assert!(
                toast.top() >= bell.bottom() && toast.top() - bell.bottom() <= px(12.0),
                "{toast:?} vs {bell:?}"
            );
            assert_eq!(visual.debug_bounds("agents-cli-scroll").unwrap(), controls);
            assert_eq!(visual.debug_bounds("rail-settings").unwrap(), nav);
            visual.simulate_click(toast.center(), gpui::Modifiers::default());
            visual.run_until_parked();
            model.read_with(visual, |m, _| {
                assert!(m.notifications.queue.current().is_some());
                assert!(m.notifications.clock.exiting.is_none());
            });
            model.update(visual, |m, cx| {
                m.notifications.queue.dismiss();
                cx.notify();
            });
            visual.run_until_parked();
            assert_eq!(visual.debug_bounds("agents-cli-scroll").unwrap(), controls);
            assert_eq!(visual.debug_bounds("rail-settings").unwrap(), nav);
        }
    }

    #[gpui::test]
    fn stale_review_actions_never_navigate(cx: &mut gpui::TestAppContext) {
        let (model, visual) = cx.add_window_view(|_, cx| crate::layout_tests::fixture(cx));
        model.update(visual, |m, cx| {
            m.select_view(crate::AppView::Agents, cx);
            m.push_actionable_notice(
                HeaderNoticeKind::Warning,
                "Review pending",
                Some(crate::session::HeaderNoticeAction {
                    label: "Review now".into(),
                    pulsation_id: "pulse-a".into(),
                }),
                cx,
            );
            let notice = m.notifications.queue.current().unwrap().clone();
            m.activate_notification(&notice, cx);
            assert_eq!(m.active_view(), crate::AppView::Agents);
            assert!(
                m.notifications
                    .queue
                    .history()
                    .iter()
                    .any(|n| n.message == "This review is no longer pending.")
            );
            m.workspace
                .update(cx, |w, _| w.project_id = Some("other-project".into()));
            let count = m.notifications.queue.history().len();
            m.activate_notification(&notice, cx);
            assert_eq!(m.active_view(), crate::AppView::Agents);
            assert_eq!(m.notifications.queue.history().len(), count);
        });
    }

    #[test]
    fn toast_waits_for_pointer_pauses_and_exits() {
        let mut clock = ToastClock::default();
        assert_eq!(
            clock.advance(Some(1), DISPLAY, false, true, false),
            (false, 0.0)
        );
        assert_eq!(
            clock.advance(Some(1), FADE, false, false, false),
            (false, 1.0)
        );
        assert_eq!(
            clock.advance(Some(1), DISPLAY, false, true, false),
            (false, 1.0)
        );
        assert_eq!(
            clock.advance(Some(1), Duration::ZERO, true, false, false),
            (false, 0.0)
        );
        clock.dismiss();
        assert_eq!(
            clock.advance(Some(1), FADE, false, false, false),
            (true, 0.0)
        );
        assert_eq!(
            clock.advance(Some(2), Duration::ZERO, false, false, true),
            (false, 1.0)
        );
    }
}
