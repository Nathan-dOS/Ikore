//! Plan-quota UI (ADR-028): one bar component shared by the header pill's popover, the Agents
//! cards and Settings › Connections. The core validates every number; this module only renders
//! it, and always pairs colour with text and an icon.

use std::collections::HashSet;
use std::time::Duration;

use chrono::{DateTime, Local, Utc};
use gpui::{
    AnyElement, Context, FocusHandle, Rgba, SharedString, Task, Window, deferred, div, prelude::*,
    px, relative,
};
use ikore_core::quota::{
    BackendQuota, CredentialDisclosure, Projection, QuotaOverview, QuotaState, QuotaWindow,
    UnavailableReason,
};
use ikore_core::terminals::CliBackend;

use crate::app_model::AppModel;
use crate::icons::Icon;
use crate::theme;

/// The header pill's refresh cadence while the window is active (G6: 55 s cache).
const POLL_EVERY: Duration = Duration::from_secs(55);
/// From here the bar turns amber; from `DANGER_PERCENT` red.
pub(crate) const WARN_PERCENT: f64 = 70.0;
pub(crate) const DANGER_PERCENT: f64 = 90.0;
/// Windows an Agents card shows at most.
const MAX_CARD_WINDOWS: usize = 4;
const POPOVER_WIDTH: f32 = 380.0;

#[derive(Default)]
pub(crate) struct QuotaUi {
    pub(crate) overview: Option<QuotaOverview>,
    pub(crate) loading: bool,
    pub(crate) popover_open: bool,
    pub(crate) window_active: bool,
    task: Option<Task<()>>,
    poller: Option<Task<()>>,
    settings_task: Option<Task<()>>,
    focus: Option<FocusHandle>,
    return_focus: Option<FocusHandle>,
    /// Backends whose first-use disclosure already reached the notifications.
    announced: HashSet<CliBackend>,
}

impl QuotaUi {
    pub(crate) fn backend(&self, backend: CliBackend) -> Option<&BackendQuota> {
        self.overview.as_ref()?.backend(backend)
    }

    pub(crate) fn dispose(&mut self) {
        self.task.take();
        self.poller.take();
        self.settings_task.take();
    }
}

impl AppModel {
    /// Reads plan usage shortly after start, then every 55 s while the window is active.
    pub(crate) fn start_quota_poller(&mut self, cx: &mut Context<Self>) {
        if self.settings_backend.is_none() {
            return;
        }
        let timer = cx.background_executor().clone();
        self.quota.poller = Some(cx.spawn(async move |this, cx| {
            timer.timer(Duration::from_secs(3)).await;
            loop {
                let alive = this.update(cx, |model, cx| {
                    if model.is_disposed() {
                        return false;
                    }
                    if model.quota.window_active {
                        model.refresh_quota(false, cx);
                    }
                    true
                });
                if !matches!(alive, Ok(true)) {
                    break;
                }
                timer.timer(POLL_EVERY).await;
            }
        }));
    }

    /// Called from render: credential collectors follow the window's activation (G6).
    pub(crate) fn note_window_activation(&mut self, active: bool, cx: &mut Context<Self>) {
        if self.quota.window_active == active {
            return;
        }
        self.quota.window_active = active;
        if let Some(backend) = &self.settings_backend {
            backend.set_quota_foreground(active);
        }
        let stale = self.quota.overview.as_ref().is_none_or(|overview| {
            Utc::now() - overview.checked_at
                > chrono::Duration::from_std(POLL_EVERY)
                    .expect("the fixed polling interval fits a chrono duration")
        });
        if active && stale {
            self.refresh_quota(false, cx);
        }
    }

    pub(crate) fn refresh_quota(&mut self, force: bool, cx: &mut Context<Self>) {
        if self.is_disposed() || self.quota.loading {
            return;
        }
        let Some(backend) = self.settings_backend.clone() else {
            return;
        };
        self.quota.loading = true;
        cx.notify();
        self.quota.task = Some(cx.spawn(async move |this, cx| {
            let result = backend.quota_overview(force).await;
            let _ = this.update(cx, |model, cx| {
                model.quota.loading = false;
                if let Ok(overview) = result {
                    model.apply_quota_overview(overview, cx);
                }
                cx.notify();
            });
        }));
    }

    pub(crate) fn apply_quota_overview(&mut self, overview: QuotaOverview, cx: &mut Context<Self>) {
        for quota in &overview.backends {
            if let Some(disclosure) = &quota.disclosure
                && self.quota.announced.insert(quota.backend)
            {
                self.push_header_notice(
                    crate::session::HeaderNoticeKind::Notice,
                    format!(
                        "{} Open the plan-usage panel in the header to keep or turn this off.",
                        disclosure_text(disclosure)
                    ),
                    cx,
                );
            }
        }
        self.quota.overview = Some(overview);
    }

    pub(crate) fn toggle_quota_popover(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.quota.popover_open {
            self.quota.popover_open = false;
            if let Some(focus) = self.quota.return_focus.take() {
                focus.focus(window);
            }
        } else {
            self.close_dropdown(cx);
            self.notifications.history_open = false;
            self.quota.return_focus = window.focused(cx);
            self.quota.popover_open = true;
            self.quota
                .focus
                .get_or_insert_with(|| cx.focus_handle())
                .focus(window);
            if self.quota.overview.is_none() {
                self.refresh_quota(false, cx);
            }
        }
        cx.notify();
    }

    pub(crate) fn set_quota_endpoint(
        &mut self,
        backend: CliBackend,
        enabled: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(settings) = self.settings_backend.clone() else {
            return;
        };
        self.quota.announced.insert(backend);
        self.quota.settings_task = Some(cx.spawn(async move |this, cx| {
            let result = settings.set_quota_endpoint(backend, enabled).await;
            let _ = this.update(cx, |model, cx| {
                if let Err(error) = result {
                    model.push_header_notice(
                        crate::session::HeaderNoticeKind::Error,
                        error.presentable().to_string(),
                        cx,
                    );
                }
                model.refresh_quota(false, cx);
            });
        }));
    }

    pub(crate) fn acknowledge_quota_disclosure(
        &mut self,
        backend: CliBackend,
        cx: &mut Context<Self>,
    ) {
        let Some(settings) = self.settings_backend.clone() else {
            return;
        };
        // Hide it at once; the core records the answer.
        if let Some(overview) = &mut self.quota.overview {
            for quota in &mut overview.backends {
                if quota.backend == backend {
                    quota.disclosure = None;
                }
            }
        }
        cx.notify();
        self.quota.settings_task = Some(cx.spawn(async move |_, _| {
            let _ = settings.acknowledge_quota_disclosure(backend).await;
        }));
    }
}

fn tone(used: f64) -> (Rgba, Option<Icon>) {
    if used >= DANGER_PERCENT {
        (theme::danger(), Some(Icon::TriangleAlert))
    } else if used >= WARN_PERCENT {
        (theme::warning(), Some(Icon::TriangleAlert))
    } else {
        (theme::accent(), None)
    }
}

pub(crate) fn percent(used: f64) -> String {
    // Never round up to a full window that is not reached yet.
    if used >= 10.0 || used.fract() == 0.0 {
        format!("{:.0}%", used.floor())
    } else {
        format!("{used:.1}%")
    }
}

/// "in 2 h 10 min", "in 45 min", "in 3 d 4 h".
pub(crate) fn until(delta: chrono::Duration) -> String {
    let minutes = delta.num_minutes().max(0);
    if minutes < 1 {
        "in under a minute".into()
    } else if minutes < 60 {
        format!("in {minutes} min")
    } else if minutes < 24 * 60 {
        match minutes % 60 {
            0 => format!("in {} h", minutes / 60),
            rest => format!("in {} h {rest} min", minutes / 60),
        }
    } else {
        match (minutes / 60) % 24 {
            0 => format!("in {} d", minutes / (24 * 60)),
            hours => format!("in {} d {hours} h", minutes / (24 * 60)),
        }
    }
}

/// "updated just now", "updated 3 min ago", "updated 2 h ago".
pub(crate) fn updated(observed_at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    format!("updated {}", crate::time::relative_since(observed_at, now))
}

fn clock(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let local = at.with_timezone(&Local);
    if at - now < chrono::Duration::hours(20) {
        local.format("%H:%M").to_string()
    } else {
        local.format("%a %d %b %H:%M").to_string()
    }
}

/// "5h", "Weekly · Opus".
pub(crate) fn window_title(window: &QuotaWindow) -> String {
    match &window.scope {
        Some(scope) => format!("{} · {scope}", window.label()),
        None => window.label(),
    }
}

fn reset_line(window: &QuotaWindow, projection: Option<Projection>, now: DateTime<Utc>) -> String {
    let mut parts = Vec::new();
    match window.resets_at {
        Some(reset) => parts.push(format!(
            "Resets {} ({})",
            clock(reset, now),
            until(reset - now)
        )),
        None => parts.push("Reset time not reported".into()),
    }
    if let Some(projection) = projection
        && projection.before_reset
    {
        parts.push(format!(
            "estimate: runs out ~{} at this pace",
            clock(projection.exhausts_at, now)
        ));
    }
    parts.join(" · ")
}

/// The used-percent bar with a pace marker at the elapsed share of the window.
fn bar(selector: String, window: &QuotaWindow, color: Rgba, now: DateTime<Utc>) -> AnyElement {
    let pace = window.elapsed_fraction(now);
    let pace_selector = format!("{selector}:pace");
    div()
        .debug_selector(move || selector.clone())
        .w_full()
        .h(px(6.0))
        .relative()
        .rounded_full()
        .bg(theme::border())
        .child(
            div()
                .absolute()
                .left_0()
                .top_0()
                .bottom_0()
                .w(relative((window.used_percent / 100.0) as f32))
                .rounded_full()
                .bg(color),
        )
        .when_some(pace, |track, pace| {
            track.child(
                div()
                    .debug_selector(move || pace_selector.clone())
                    .absolute()
                    .top(px(-2.0))
                    .bottom(px(-2.0))
                    .left(relative(pace as f32))
                    .w(px(2.0))
                    .rounded_full()
                    .bg(theme::text_muted()),
            )
        })
        .into_any_element()
}

/// One window: title, used percent (text, icon and colour), the bar, and its reset.
pub(crate) fn window_row(
    selector: String,
    window: &QuotaWindow,
    projection: Option<Projection>,
    now: DateTime<Utc>,
) -> AnyElement {
    let (color, icon) = tone(window.used_percent);
    let used = if window.is_exhausted() {
        "100% used · limit reached".to_string()
    } else {
        format!("{} used", percent(window.used_percent))
    };
    let pace_ahead = window
        .elapsed_fraction(now)
        .is_some_and(|pace| window.used_percent / 100.0 > pace + 0.05);
    let bar_selector = format!("{selector}:bar");
    div()
        .debug_selector(move || selector.clone())
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .w_full()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .text_xs()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_ellipsis()
                        .child(window_title(window)),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .gap_1()
                        .text_color(color)
                        .when_some(icon, |row, icon| {
                            row.child(crate::widgets::icon(icon, px(12.0), color))
                        })
                        .child(used),
                ),
        )
        .child(bar(bar_selector, window, color, now))
        .child(
            div()
                .text_xs()
                .text_color(theme::text_muted())
                // One text run, so the pace note wraps with the reset line instead of
                // starting a block of its own.
                .child(SharedString::from(format!(
                    "{}{}",
                    reset_line(window, projection, now),
                    if pace_ahead && !window.is_exhausted() {
                        " · ahead of the pace marker"
                    } else {
                        ""
                    }
                ))),
        )
        .into_any_element()
}

pub(crate) fn reason_text(reason: UnavailableReason, backend: CliBackend) -> String {
    match reason {
        UnavailableReason::NotSignedIn => format!(
            "Signed out. Sign in through the {} CLI, then refresh.",
            backend.label()
        ),
        UnavailableReason::CliMissing => format!("{} CLI not installed.", backend.label()),
        other => other.message().to_string(),
    }
}

/// A short state label and its colour, e.g. ("Stale · updated 12 min ago", muted).
pub(crate) fn state_label(quota: &BackendQuota, now: DateTime<Utc>) -> (String, Rgba) {
    let age = quota
        .snapshot
        .as_ref()
        .map(|snapshot| updated(snapshot.observed_at, now))
        .unwrap_or_default();
    match quota.state {
        QuotaState::Fresh => (age, theme::text_muted()),
        QuotaState::Stale => (format!("Stale · {age}"), theme::text_muted()),
        QuotaState::Exhausted => (format!("Limit reached · {age}"), theme::danger()),
        QuotaState::Unavailable(UnavailableReason::NotSignedIn) => {
            ("Signed out".into(), theme::warning())
        }
        QuotaState::Unavailable(UnavailableReason::CliMissing) => {
            ("Not installed".into(), theme::text_muted())
        }
        QuotaState::Unavailable(UnavailableReason::CliCheckFailed) => {
            ("CLI check failed".into(), theme::warning())
        }
        QuotaState::Unavailable(UnavailableReason::VersionUnsupported) => {
            ("Version not validated".into(), theme::warning())
        }
        QuotaState::Unavailable(UnavailableReason::Disabled) => {
            ("Turned off".into(), theme::text_muted())
        }
        QuotaState::Unavailable(_) => ("Unavailable".into(), theme::text_muted()),
    }
}

/// Every window of one CLI, its state and where the data came from.
pub(crate) fn backend_section(
    quota: &BackendQuota,
    surface: &str,
    now: DateTime<Utc>,
) -> AnyElement {
    let selector = format!("quota:{surface}:{}", quota.backend.as_str());
    let (label, label_color) = state_label(quota, now);
    div()
        .debug_selector(move || selector.clone())
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .w_full()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .flex_shrink_0()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(quota.backend.label()),
                )
                .child(
                    div()
                        .min_w_0()
                        .text_xs()
                        .text_color(label_color)
                        .text_ellipsis()
                        .child(label),
                ),
        )
        .child(windows_block(quota, surface, now))
        .into_any_element()
}

/// The current windows with their source and plan, or why there are none.
pub(crate) fn windows_block(quota: &BackendQuota, surface: &str, now: DateTime<Utc>) -> AnyElement {
    let backend = quota.backend.as_str();
    let mut body = div().w_full().min_w_0().flex().flex_col().gap_2();
    match (&quota.snapshot, quota.state) {
        (Some(snapshot), QuotaState::Fresh | QuotaState::Stale | QuotaState::Exhausted) => {
            for (index, window) in snapshot.windows.iter().enumerate() {
                if !window.is_current(now) {
                    continue;
                }
                let projection = quota.projections.get(index).copied().flatten();
                body = body.child(window_row(
                    format!("quota-window:{surface}:{backend}:{index}"),
                    window,
                    projection,
                    now,
                ));
            }
            let mut meta = vec![format!("Source: {}", snapshot.source.label())];
            if let Some(plan) = &snapshot.plan {
                meta.push(format!("Plan: {plan}"));
            }
            body = body.child(
                div()
                    .text_xs()
                    .text_color(theme::text_subtle())
                    .child(meta.join(" · ")),
            );
        }
        (_, QuotaState::Unavailable(reason)) => {
            body = body.child(
                div()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(reason_text(reason, quota.backend)),
            );
        }
        _ => {}
    }
    body.into_any_element()
}
/// The windows on an Agents card: at most four, tightest first.
pub(crate) fn card_rows(
    quota: &BackendQuota,
    role: &str,
    now: DateTime<Utc>,
) -> Option<AnyElement> {
    let snapshot = quota.snapshot.as_ref()?;
    if matches!(quota.state, QuotaState::Unavailable(_)) {
        return None;
    }
    let mut windows = snapshot
        .windows
        .iter()
        .enumerate()
        .filter(|(_, window)| window.is_current(now))
        .collect::<Vec<_>>();
    windows.sort_by(|left, right| right.1.used_percent.total_cmp(&left.1.used_percent));
    let (label, _) = state_label(quota, now);
    let selector = format!("agent-quota:{role}");
    Some(
        div()
            .debug_selector(move || selector.clone())
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .children(
                windows
                    .into_iter()
                    .take(MAX_CARD_WINDOWS)
                    .map(|(index, window)| {
                        window_row(
                            format!("agent-quota:{role}:{index}"),
                            window,
                            quota.projections.get(index).copied().flatten(),
                            now,
                        )
                    }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_subtle())
                    .child(format!("Plan usage · {label}")),
            )
            .into_any_element(),
    )
}

/// "Plan usage during this run: 5h +4% · Weekly +1%", marked approximate when runs overlapped.
pub(crate) fn run_delta_line(deltas: &[ikore_core::quota::RunQuotaDelta]) -> Option<String> {
    if deltas.is_empty() {
        return None;
    }
    let parts = deltas
        .iter()
        .map(|delta| {
            let label = ikore_core::quota::window_label(delta.window_minutes);
            let title = match &delta.scope {
                Some(scope) => format!("{label} · {scope}"),
                None => label,
            };
            let change = delta.delta();
            if change < 0.0 {
                format!("{title} -{}", percent(-change))
            } else {
                format!("{title} +{}", percent(change))
            }
        })
        .collect::<Vec<_>>();
    let approximate = deltas.iter().any(|delta| delta.approximate);
    Some(format!(
        "Plan usage during this run: {}{}",
        parts.join(" · "),
        if approximate {
            " (approx.: another run of this CLI overlapped)"
        } else {
            ""
        }
    ))
}

pub(crate) fn disclosure_text(disclosure: &CredentialDisclosure) -> String {
    let cli = match disclosure.backend {
        CliBackend::Codex => "Codex",
        _ => "Claude Code",
    };
    format!(
        "To show your plan usage, Ikore reads {cli}'s sign-in token from {} and sends it only to {}. The token stays in memory; it is never saved, logged or refreshed, and never reaches agents.",
        disclosure.file, disclosure.host
    )
}

/// The one-time notice (G7) with its two answers.
pub(crate) fn disclosure_banner(
    disclosure: &CredentialDisclosure,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let backend = disclosure.backend;
    let (keep, off): (&'static str, &'static str) = match backend {
        CliBackend::Codex => ("quota-disclosure-keep:codex", "quota-disclosure-off:codex"),
        _ => (
            "quota-disclosure-keep:claude",
            "quota-disclosure-off:claude",
        ),
    };
    let selector = format!("quota-disclosure:{}", backend.as_str());
    div()
        .debug_selector(move || selector.clone())
        .w_full()
        .p_3()
        .flex()
        .flex_col()
        .gap_2()
        .rounded_md()
        .bg(theme::accent_soft())
        .child(
            div()
                .text_sm()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(format!(
                    "Plan usage for {}",
                    match backend {
                        CliBackend::Codex => "Codex",
                        _ => "Claude Code",
                    }
                )),
        )
        .child(div().text_xs().child(disclosure_text(disclosure)))
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .child(crate::settings_view::action_with(
                    keep,
                    "Keep on",
                    crate::widgets::ButtonVariant::Primary,
                    true,
                    cx,
                    move |model, cx| model.acknowledge_quota_disclosure(backend, cx),
                ))
                .child(crate::settings_view::action_with(
                    off,
                    "Turn off",
                    crate::widgets::ButtonVariant::Secondary,
                    true,
                    cx,
                    move |model, cx| model.set_quota_endpoint(backend, false, cx),
                )),
        )
        .into_any_element()
}

/// The tightest current window across the CLIs with data: (backend, window).
pub(crate) fn worst_window(
    overview: &QuotaOverview,
    now: DateTime<Utc>,
) -> Option<(&BackendQuota, &QuotaWindow)> {
    overview
        .backends
        .iter()
        .filter(|quota| !matches!(quota.state, QuotaState::Unavailable(_)))
        .filter_map(|quota| Some((quota, quota.tightest(now)?)))
        .max_by(|left, right| {
            left.1
                .used_percent
                .total_cmp(&right.1.used_percent)
                .then_with(|| {
                    right
                        .1
                        .window_minutes
                        .unwrap_or(u32::MAX)
                        .cmp(&left.1.window_minutes.unwrap_or(u32::MAX))
                })
        })
}

/// The header pill: the worst window, or a neutral entry point to the popover.
pub(crate) fn header_pill(model: &AppModel, cx: &mut Context<AppModel>) -> Option<AnyElement> {
    let overview = model.quota.overview.as_ref()?;
    if overview
        .backends
        .iter()
        .all(|quota| quota.state == QuotaState::Unavailable(UnavailableReason::CliMissing))
    {
        return None;
    }
    let now = Utc::now();
    let attention = overview
        .backends
        .iter()
        .any(|quota| quota.disclosure.is_some());
    let (text, color, icon, tooltip) = match worst_window(overview, now) {
        Some((quota, window)) => {
            let stale = quota.state == QuotaState::Stale;
            let (color, icon) = if stale {
                (theme::text_muted(), None)
            } else {
                tone(window.used_percent)
            };
            (
                format!(
                    "{} {} · {}",
                    quota.backend.label(),
                    window_title(window),
                    percent(window.used_percent)
                ),
                color,
                icon,
                format!(
                    "Plan usage: tightest window across your CLIs{}. Click for every window.",
                    if stale { " (stale)" } else { "" }
                ),
            )
        }
        None => (
            "Plan usage".to_string(),
            theme::text_muted(),
            None,
            "Plan usage: no window reported yet. Click for details.".to_string(),
        ),
    };
    let open = model.quota.popover_open;
    Some(
        div()
            .id("header-quota")
            .debug_selector(|| "header-quota".into())
            .flex_shrink_0()
            .h(px(28.0))
            .px_2()
            .flex()
            .items_center()
            .gap_1()
            .rounded_full()
            .border_1()
            .border_color(if open {
                theme::border_strong()
            } else {
                theme::border()
            })
            .bg(theme::surface())
            .cursor_pointer()
            .hover(|style| style.bg(theme::surface_hover()))
            .tooltip(crate::widgets::text_tooltip(tooltip))
            .on_click(cx.listener(|model, _: &gpui::ClickEvent, window, cx| {
                model.toggle_quota_popover(window, cx);
            }))
            .child(crate::widgets::icon(
                icon.unwrap_or(Icon::Timer),
                px(14.0),
                color,
            ))
            .child(div().text_xs().text_color(color).child(text))
            .when(attention, |pill| {
                pill.child(crate::widgets::status_dot(theme::accent()))
            })
            .into_any_element(),
    )
}

/// Right edge of the pill: the header padding, then the health and bell controls with their gaps.
fn popover_right_inset() -> gpui::Pixels {
    px(12.0) + crate::widgets::ICON_BUTTON_SIZE * 2.0 + px(8.0)
}

/// The popover under the pill: every CLI and window, disclosures first.
pub(crate) fn popover(
    model: &mut AppModel,
    window: &mut Window,
    cx: &mut Context<AppModel>,
) -> Option<AnyElement> {
    let blocked = model.active_dropdown.is_some()
        || model.forge.new_open
        || model.settings_modal.open
        || model.notifications.history_open;
    if !model.quota.popover_open || blocked {
        return None;
    }
    let viewport = window.viewport_size();
    let right = popover_right_inset();
    let top = crate::notifications::overlay_top();
    let width = px(POPOVER_WIDTH).min(viewport.width - right - px(12.0));
    let focus = model
        .quota
        .focus
        .get_or_insert_with(|| cx.focus_handle())
        .clone();
    let now = Utc::now();
    let loading = model.quota.loading;
    let mut panel = div()
        .id("quota-popover")
        .debug_selector(|| "quota-popover".into())
        .track_focus(&focus)
        .w(width)
        .max_h((viewport.height - top - px(40.0)).max(px(120.0)))
        .overflow_y_scroll()
        .p_3()
        .flex()
        .flex_col()
        .gap_3()
        .rounded_lg()
        .shadow_lg()
        .bg(theme::surface_raised())
        .border_1()
        .border_color(theme::border())
        .occlude()
        .on_mouse_down_out(cx.listener(|model, _, window, cx| {
            model.toggle_quota_popover(window, cx);
            cx.stop_propagation();
        }))
        .on_key_down(
            cx.listener(|model, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    model.toggle_quota_popover(window, cx);
                    cx.stop_propagation();
                }
            }),
        )
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("Plan usage"),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(crate::widgets::icon_button(
                            "quota-refresh",
                            Icon::RotateCw,
                            if loading { "Checking…" } else { "Refresh" },
                            false,
                            None,
                            !loading,
                            cx,
                            |model, _, cx| model.refresh_quota(true, cx),
                        ))
                        .child(crate::widgets::icon_button(
                            "quota-close",
                            Icon::X,
                            crate::widgets::Tooltip::new("Close").with_shortcut("escape"),
                            false,
                            None,
                            true,
                            cx,
                            |model, window, cx| model.toggle_quota_popover(window, cx),
                        )),
                ),
        );
    match model.quota.overview.clone() {
        Some(overview) => {
            for quota in &overview.backends {
                if let Some(disclosure) = &quota.disclosure {
                    panel = panel.child(disclosure_banner(disclosure, cx));
                }
            }
            for quota in &overview.backends {
                panel = panel.child(backend_section(quota, "popover", now));
            }
            panel = panel.child(
                div()
                    .text_xs()
                    .text_color(theme::text_subtle())
                    .child("Figures come from each CLI or its provider; the marker on a bar is how much of the window has passed. Settings › Connections lists the sources."),
            );
        }
        None => {
            panel = panel.child(div().text_xs().text_color(theme::text_muted()).child(
                if loading {
                    "Checking plan usage…"
                } else {
                    "No plan usage checked yet."
                },
            ));
        }
    }
    Some(
        deferred(div().absolute().top(top).right(right).child(panel))
            .with_priority(15)
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_and_percentages_read_naturally() {
        assert_eq!(until(chrono::Duration::seconds(20)), "in under a minute");
        assert_eq!(until(chrono::Duration::minutes(45)), "in 45 min");
        assert_eq!(until(chrono::Duration::minutes(130)), "in 2 h 10 min");
        assert_eq!(until(chrono::Duration::minutes(120)), "in 2 h");
        assert_eq!(until(chrono::Duration::hours(76)), "in 3 d 4 h");
        let now = Utc::now();
        assert_eq!(updated(now, now), "updated just now");
        assert_eq!(
            updated(now - chrono::Duration::minutes(12), now),
            "updated 12 min ago"
        );
        assert_eq!(percent(0.0), "0%");
        assert_eq!(percent(4.7), "4.7%");
        assert_eq!(percent(46.0), "46%");
        assert_eq!(percent(99.6), "99%");
        // Colour is never the only signal: amber and red come with an icon.
        assert!(tone(10.0).1.is_none());
        assert!(tone(70.0).1.is_some() && tone(95.0).1.is_some());
    }

    #[test]
    fn run_deltas_read_as_a_change_per_window() {
        let delta = |minutes, start, end, approximate| ikore_core::quota::RunQuotaDelta {
            backend: CliBackend::Codex,
            scope: None,
            window_minutes: Some(minutes),
            start_percent: start,
            end_percent: end,
            approximate,
        };
        assert_eq!(run_delta_line(&[]), None);
        assert_eq!(
            run_delta_line(&[
                delta(300, 10.0, 14.0, false),
                delta(10_080, 3.0, 4.0, false)
            ])
            .unwrap(),
            "Plan usage during this run: 5h +4% · Weekly +1%"
        );
        assert!(
            run_delta_line(&[delta(300, 10.0, 12.5, true)])
                .unwrap()
                .ends_with("5h +2.5% (approx.: another run of this CLI overlapped)")
        );
    }
}
