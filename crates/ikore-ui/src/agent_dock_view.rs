//! The bottom agent dock's rendering (ADR-040, M3; specification 9): the
//! 32 px strip, or the expanded panel with its tabs, the "now" line and the
//! shared console, under a sash. The work panel's Run tab (`run_tab`) shows
//! the same agent with the pieces it borrows from here.

use std::time::Instant;

use gpui::{
    AnyElement, App, Context, CursorStyle, MouseButton, MouseMoveEvent, MouseUpEvent, Pixels,
    SharedString, Window, canvas, div, prelude::*, px,
};
use ikore_core::terminals::{AgentExecutionMode, TerminalRun, TerminalRunStatus};

use crate::agent_console::follow::FollowInput;
use crate::agent_console::view::{ConsoleSurface, console, duration_label, follow_control};
use crate::agent_dock::{
    DOCK_SURFACE, DockMode, SASH_HEIGHT, STRIP_HEIGHT, VISIBLE_TABS, pipeline_step,
};
use crate::agents::AgentIdentifier;
use crate::app_model::{AppModel, AppView};
use crate::icons::Icon;
use crate::theme;

/// How far a tab gives way on a narrow window: its dot, the start of its
/// label and its close button.
const TAB_MIN_WIDTH: f32 = 72.0;

/// What a tab and the strip say about an agent.
pub(crate) struct AgentSummary {
    pub(crate) agent: AgentIdentifier,
    pub(crate) label: String,
    pub(crate) color: gpui::Rgba,
    /// The agent's latest run.
    pub(crate) run: Option<TerminalRun>,
    pub(crate) api: bool,
}

impl AgentSummary {
    /// Failed, or waiting for a fallback decision (9.5).
    pub(crate) fn needs_attention(&self) -> bool {
        self.run.as_ref().is_some_and(|run| {
            matches!(
                run.status,
                TerminalRunStatus::Failed | TerminalRunStatus::AwaitingFallback
            )
        })
    }
}

/// The agent's label, colour and latest run, from the agents snapshot.
pub(crate) fn summarize(model: &AppModel, agent: &AgentIdentifier, cx: &App) -> AgentSummary {
    let agents = model.agents.read(cx);
    let snapshot = agents.snapshot.as_ref();
    match agent {
        AgentIdentifier::Canonical(role) => {
            let descriptor = ikore_core::terminals::canonical_agent(*role);
            let config = snapshot.and_then(|snapshot| {
                snapshot
                    .agents
                    .iter()
                    .find(|candidate| candidate.config.agent_role == *role)
            });
            AgentSummary {
                agent: agent.clone(),
                label: match pipeline_step(*role) {
                    Some(step) => format!("{step} · {}", descriptor.name),
                    None => descriptor.name.to_owned(),
                },
                color: theme::agent_color(*role),
                run: config.and_then(|agent| agent.recent_runs.first().cloned()),
                api: config.is_some_and(|agent| {
                    agent.config.execution_mode == AgentExecutionMode::ApiDirect
                }),
            }
        }
        AgentIdentifier::FreeForm(slot_id) => {
            let slot = snapshot.and_then(|snapshot| {
                snapshot
                    .free_form_slots
                    .iter()
                    .chain(&snapshot.archived_free_form_slots)
                    .find(|candidate| candidate.slot.id == *slot_id)
            });
            AgentSummary {
                agent: agent.clone(),
                label: slot.map_or_else(
                    || "Free-form agent".to_owned(),
                    |slot| slot.slot.label.clone(),
                ),
                color: theme::accent(),
                run: slot.and_then(|slot| slot.recent_runs.first().cloned()),
                api: slot
                    .is_some_and(|slot| slot.slot.execution_mode == AgentExecutionMode::ApiDirect),
            }
        }
        // `{pipeline} · {step}` (feature 25, section 6).
        AgentIdentifier::PipelineStep {
            pipeline_id,
            step_id,
        } => {
            let slot = agents.automation_slot(pipeline_id, step_id);
            AgentSummary {
                agent: agent.clone(),
                label: agents.pipeline_step_label(pipeline_id, step_id),
                color: theme::accent(),
                run: slot.and_then(|slot| slot.recent_runs.first().cloned()),
                api: slot
                    .is_some_and(|slot| slot.slot.execution_mode == AgentExecutionMode::ApiDirect),
            }
        }
    }
}

/// The agent's runs, newest first, from the agents snapshot.
fn agent_runs(model: &AppModel, agent: &AgentIdentifier, cx: &App) -> Vec<TerminalRun> {
    let agents = model.agents.read(cx);
    let Some(snapshot) = agents.snapshot.as_ref() else {
        return Vec::new();
    };
    match agent {
        AgentIdentifier::Canonical(role) => snapshot
            .agents
            .iter()
            .find(|candidate| candidate.config.agent_role == *role)
            .map(|candidate| candidate.recent_runs.clone()),
        AgentIdentifier::FreeForm(slot_id) => snapshot
            .free_form_slots
            .iter()
            .chain(&snapshot.archived_free_form_slots)
            .find(|candidate| candidate.slot.id == *slot_id)
            .map(|candidate| candidate.recent_runs.clone()),
        AgentIdentifier::PipelineStep {
            pipeline_id,
            step_id,
        } => agents
            .automation_slot(pipeline_id, step_id)
            .map(|candidate| candidate.recent_runs.clone()),
    }
    .unwrap_or_default()
}

/// The run a console of the dock's agent shows: the one a History row or a
/// unit chose while the dock is pinned on the agent (feature 25, R13), or
/// the latest. A chosen run comes with its place: `(3, 7)` is `Run 3/7`.
pub(crate) fn shown_run(
    model: &AppModel,
    summary: &AgentSummary,
    cx: &App,
) -> (Option<TerminalRun>, Option<(usize, usize)>) {
    let Some(run_id) = model.agent_dock.chosen_run_of(&summary.agent) else {
        return (summary.run.clone(), None);
    };
    let runs = agent_runs(model, &summary.agent, cx);
    match runs.iter().position(|run| run.id == run_id) {
        Some(index) => (runs.get(index).cloned(), Some((index + 1, runs.len()))),
        None => (summary.run.clone(), None),
    }
}

/// `Run 3/7 · Latest`: the dock shows an older run; a click brings the
/// latest back.
pub(crate) fn run_chip(place: (usize, usize), cx: &mut Context<AppModel>) -> AnyElement {
    let (index, total) = place;
    div()
        .id("agent-dock-run-chip")
        .debug_selector(|| "agent-dock-run-chip".into())
        .flex_shrink_0()
        .px_2()
        .py_1()
        .rounded_full()
        .border_1()
        .border_color(theme::accent())
        .text_xs()
        .text_color(theme::text())
        .cursor_pointer()
        .hover(|style| style.bg(theme::accent_soft()))
        .tooltip(|_, cx| {
            cx.new(|_| crate::widgets::Tooltip::new("Back to the latest run"))
                .into()
        })
        .on_click(cx.listener(|model, _, _, cx| model.show_latest_agent_run(cx)))
        .child(format!("Run {index}/{total} · Latest"))
        .into_any_element()
}

/// `2:14` while a run goes on, `3m ago` after it.
pub(crate) fn run_time(run: &TerminalRun) -> Option<String> {
    time_label(
        run.status.is_active(),
        run.started_at.as_deref().unwrap_or(&run.created_at),
        run.finished_at.as_deref().unwrap_or(&run.updated_at),
        chrono::Utc::now().fixed_offset(),
    )
}

pub(crate) fn time_label(
    active: bool,
    started: &str,
    ended: &str,
    now: chrono::DateTime<chrono::FixedOffset>,
) -> Option<String> {
    let parse = |value: &str| chrono::DateTime::parse_from_rfc3339(value).ok();
    if active {
        let ms = u64::try_from((now - parse(started)?).num_milliseconds()).ok()?;
        return Some(duration_label(ms));
    }
    let minutes = (now - parse(ended)?).num_minutes().max(0);
    Some(match minutes {
        0 => "just now".to_owned(),
        1..=59 => format!("{minutes}m ago"),
        _ => format!("{}h ago", minutes / 60),
    })
}

fn status_color(status: TerminalRunStatus) -> gpui::Rgba {
    match status {
        TerminalRunStatus::Failed | TerminalRunStatus::Interrupted => theme::danger(),
        TerminalRunStatus::AwaitingFallback | TerminalRunStatus::Cancelled => theme::warning(),
        TerminalRunStatus::Completed => theme::success(),
        _ => theme::running(),
    }
}

/// `Running · 2:14`, from the run; the pulse adds the phase (M5).
fn run_line(run: Option<&TerminalRun>) -> String {
    match run {
        None => "No run yet".to_owned(),
        Some(run) => {
            let mut line = crate::agents_view::run_status(run.status).to_owned();
            if let Some(time) = run_time(run) {
                line.push_str(" · ");
                line.push_str(&time);
            }
            line
        }
    }
}

/// The sash above the expanded dock: drag to resize, double click for the
/// default or the tallest height.
fn sash(dragging: bool, body: Pixels, cx: &mut Context<AppModel>) -> AnyElement {
    let app = cx.entity().downgrade();
    div()
        .id("agent-dock-sash")
        .debug_selector(|| "agent-dock-sash".into())
        .h(SASH_HEIGHT)
        .w_full()
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .cursor(CursorStyle::ResizeUpDown)
        .group("agent-dock-sash")
        .child(
            div()
                .h(px(2.0))
                .w(px(48.0))
                .rounded_full()
                .when(dragging, |handle| handle.bg(theme::accent()))
                .when(!dragging, |handle| {
                    handle
                        .bg(theme::border())
                        .group_hover("agent-dock-sash", |hovered| hovered.bg(theme::accent()))
                }),
        )
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |model, event: &gpui::MouseDownEvent, window, cx| {
                if event.click_count == 2 {
                    model.agent_dock.drag = None;
                    model.toggle_agent_dock_height(body, cx);
                } else {
                    model.agent_dock.drag =
                        Some((event.position.y, model.agent_dock.expanded_height(body)));
                }
                cx.stop_propagation();
                window.refresh();
            }),
        )
        .child(
            canvas(
                |_, _, _| {},
                move |_, _, window, _| {
                    if !dragging {
                        return;
                    }
                    window.set_window_cursor_style(CursorStyle::ResizeUpDown);
                    let move_app = app.clone();
                    window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                        if !phase.capture() {
                            return;
                        }
                        let _ = move_app.update(cx, |model, cx| {
                            if event.pressed_button == Some(MouseButton::Left) {
                                model.drag_agent_dock(event.position.y, body, cx);
                            } else {
                                model.end_agent_dock_drag(cx);
                            }
                        });
                        cx.stop_propagation();
                        window.refresh();
                    });
                    let up_app = app.clone();
                    window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                        if !phase.capture() || event.button != MouseButton::Left {
                            return;
                        }
                        let _ = up_app.update(cx, |model, cx| {
                            model.drag_agent_dock(event.position.y, body, cx);
                            model.end_agent_dock_drag(cx);
                        });
                        cx.stop_propagation();
                        window.refresh();
                    });
                },
            )
            .absolute()
            .size_full(),
        )
        .into_any_element()
}

fn dock_button(
    id: &'static str,
    glyph: Icon,
    tooltip: crate::widgets::Tooltip,
    cx: &mut Context<AppModel>,
    handler: impl Fn(&mut AppModel, &mut Window, &mut Context<AppModel>) + 'static,
) -> AnyElement {
    crate::widgets::icon_button(id, glyph, tooltip, false, None, true, cx, handler)
}

/// The dock under the current view, with its sash; `None` when it is hidden
/// or the view is Agents. Records the height it takes for the Companion.
pub(crate) fn dock(
    model: &mut AppModel,
    window: &mut Window,
    cx: &mut Context<AppModel>,
) -> Option<AnyElement> {
    let body = AppModel::shell_body_height(window);
    // The Run tab reads the same agent: its reading counts with the dock hidden too.
    let run_tab = crate::run_tab::on_screen(model, window, cx);
    if !model.agent_dock_visible(body) {
        model.agent_dock.rendered = px(0.0);
        note_reading(model, false, run_tab, cx);
        return None;
    }
    let mode = model.agent_dock.effective_mode(body);
    let tabs = model
        .agent_dock
        .tabs()
        .iter()
        .map(|agent| summarize(model, agent, cx))
        .collect::<Vec<_>>();
    let shown = model
        .agent_dock
        .shown()
        .map(|agent| summarize(model, &agent, cx));
    let expanded = mode == DockMode::Expanded && model.agent_dock.has_runs();
    let height = if expanded {
        px(model.agent_dock.expanded_height(body))
    } else {
        STRIP_HEIGHT
    };
    // The sash (or the strip's top gap) and the centre column's bottom padding count too:
    // the Companion sits that much higher (9.6).
    model.agent_dock.rendered = height + SASH_HEIGHT + px(8.0);
    let content = if expanded {
        expanded_dock(model, &tabs, shown.as_ref(), cx)
    } else {
        strip(model, &tabs, shown.as_ref(), mode, body, cx)
    };
    note_reading(model, expanded, run_tab, cx);
    let dragging = model.agent_dock.drag.is_some();
    // In the bottom padding of `shell-center`, which `agent_dock_inset` sizes: the sash (or
    // the strip's top gap) and the dock, 8 px above the status bar.
    let home = model.active_view() == AppView::Home;
    Some(
        div()
            .id("agent-dock-area")
            .absolute()
            .left_0()
            .right(if home { px(8.0) } else { px(0.0) })
            .bottom(px(8.0))
            .h(height + SASH_HEIGHT)
            .flex()
            .flex_col()
            .when(expanded, |area| area.child(sash(dragging, body, cx)))
            .when(!expanded, |area| area.pt(px(6.0)))
            .child(
                div()
                    .id("agent-dock")
                    .debug_selector(|| "agent-dock".into())
                    .key_context("AgentDock")
                    .h(height)
                    .w_full()
                    .flex()
                    .flex_col()
                    .min_h_0()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme::border())
                    .bg(theme::surface())
                    .overflow_hidden()
                    // Touching the dock holds back a switch for a moment (10.3, rule 4).
                    .capture_any_mouse_down(cx.listener(|model, _, _, cx| {
                        model.note_agent_dock_interaction(cx);
                    }))
                    .on_key_down(cx.listener(|model, event: &gpui::KeyDownEvent, _, cx| {
                        model.note_agent_dock_interaction(cx);
                        // The console closes its search first; then Esc folds the dock.
                        if event.keystroke.key == "escape" {
                            model.set_agent_dock_mode(DockMode::Strip, cx);
                            cx.stop_propagation();
                        }
                    }))
                    .child(content),
            )
            .into_any_element(),
    )
}

/// Reading a console of the followed agent higher up, or searching it, holds
/// a switch back (10.3, rule 4): the expanded dock's console, and the work
/// panel's Run tab while it is on screen.
fn note_reading(model: &mut AppModel, dock: bool, run_tab: bool, cx: &mut Context<AppModel>) {
    let read = |key: &str| {
        (dock && key == DOCK_SURFACE) || (run_tab && key == crate::run_tab::RUN_TAB_SURFACE)
    };
    let scrolled_up = {
        let consoles = &model.agents.read(cx).consoles;
        [DOCK_SURFACE, crate::run_tab::RUN_TAB_SURFACE]
            .into_iter()
            .any(|key| read(key) && !consoles.following(key))
    };
    let searching = model
        .agent_console_search
        .as_ref()
        .is_some_and(|(key, _)| read(key));
    let now = Instant::now();
    let mut switched = false;
    if model.agent_dock.follow.scrolled_up != scrolled_up {
        switched |= model.agent_dock.follow.apply(FollowInput::ScrolledUp {
            up: scrolled_up,
            at: now,
        });
    }
    if model.agent_dock.follow.search_focused != searching {
        switched |= model.agent_dock.follow.apply(FollowInput::SearchFocused {
            focused: searching,
            at: now,
        });
    }
    if switched {
        cx.notify();
    }
}

/// The strip: who is at work, what they are doing, and the other tabs as dots.
fn strip(
    model: &AppModel,
    tabs: &[AgentSummary],
    shown: Option<&AgentSummary>,
    mode: DockMode,
    body: Pixels,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let _ = model;
    let mut row = div()
        .id("agent-dock-strip")
        .debug_selector(|| "agent-dock-strip".into())
        .h(STRIP_HEIGHT)
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .text_xs();
    match shown {
        None => {
            row = row.child(
                div()
                    .flex_1()
                    .text_color(theme::text_muted())
                    .child("No agent activity yet · Ctrl+J"),
            );
        }
        Some(summary) => {
            row = row
                .child(crate::widgets::status_dot(summary.color))
                .child(
                    div()
                        .flex_shrink_0()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::text())
                        .child(summary.label.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(theme::text_muted())
                        .child(now_text(model, summary, cx)),
                )
                .children(
                    tabs.iter()
                        .filter(|tab| tab.agent != summary.agent)
                        .map(|tab| {
                            let agent = tab.agent.clone();
                            let selector = format!(
                                "agent-dock-mini:{}",
                                crate::agent_dock::agent_key(&tab.agent)
                            );
                            div()
                                .id(SharedString::from(selector.clone()))
                                .debug_selector(move || selector.clone())
                                .size_2()
                                .rounded_full()
                                .bg(tab.color)
                                .when(tab.needs_attention(), |dot| {
                                    dot.border_1().border_color(theme::danger())
                                })
                                .cursor_pointer()
                                .tooltip({
                                    let label = tab.label.clone();
                                    move |_, cx| {
                                        cx.new(|_| crate::widgets::Tooltip::new(label.clone()))
                                            .into()
                                    }
                                })
                                .on_click(cx.listener(move |model, _, _, cx| {
                                    model.pin_agent_dock(agent.clone(), cx);
                                }))
                        }),
                );
        }
    }
    let expand_to = if mode == DockMode::Expanded {
        // Expanded with no run yet keeps the strip's height.
        None
    } else {
        Some(DockMode::Expanded)
    };
    let _ = body;
    row.when_some(expand_to, |row, next| {
        row.child(dock_button(
            "agent-dock-expand",
            Icon::ChevronUp,
            crate::widgets::Tooltip::new("Expand the agent console"),
            cx,
            move |model, _, cx| model.set_agent_dock_mode(next, cx),
        ))
    })
    .child(dock_button(
        "agent-dock-hide",
        Icon::X,
        crate::widgets::Tooltip::new("Hide the agent console").with_shortcut("Ctrl+J"),
        cx,
        |model, _, cx| model.set_agent_dock_mode(DockMode::Hidden, cx),
    ))
    .into_any_element()
}

/// What the agent does now: its live phase, target and counters from the
/// pulse (M5), a phase of the host's own (PD5), or the run's status and time.
pub(crate) fn now_text(model: &AppModel, summary: &AgentSummary, cx: &App) -> String {
    let Some(run) = summary.run.as_ref() else {
        return run_line(None);
    };
    if let Some(live) = model.agents.read(cx).live.get(&run.id) {
        return live.line(Instant::now());
    }
    host_phase(model, run, cx).unwrap_or_else(|| run_line(Some(run)))
}

/// The phases only the host knows (PD5): waiting for the agent's slot,
/// waiting for a fallback decision, a run tried again after a restart, and a
/// stage whose review waits on the developer.
fn host_phase(model: &AppModel, run: &TerminalRun, cx: &App) -> Option<String> {
    use ikore_core::persistence_service::PulsationStatus;
    use ikore_core::terminals::ActivityPhase;
    let (phase, detail) = match run.status {
        TerminalRunStatus::Queued => (ActivityPhase::Queued, "waiting for the agent's slot"),
        TerminalRunStatus::AwaitingFallback => {
            (ActivityPhase::FallingBack, "waiting for your decision")
        }
        status
            if status.is_active()
                && run.continuation_reason.as_deref() == Some("restart_recovery") =>
        {
            (ActivityPhase::Retrying, "after a restart")
        }
        status
            if status.is_terminal()
                && run.pulsation_id.as_deref().is_some_and(|id| {
                    model
                        .pulsations
                        .read(cx)
                        .pulsations
                        .iter()
                        .any(|pulsation| {
                            pulsation.id == id && pulsation.status == PulsationStatus::Checkpoint
                        })
                }) =>
        {
            (ActivityPhase::WaitingOnUser, "the review is yours")
        }
        _ => return None,
    };
    let mut line = format!("{} · {detail}", phase.label());
    if let Some(time) = run_time(run) {
        line.push_str(" · ");
        line.push_str(&time);
    }
    Some(line)
}

fn tab(
    summary: &AgentSummary,
    shown: bool,
    marked: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let key = crate::agent_dock::agent_key(&summary.agent);
    let selector = format!("agent-dock-tab:{key}");
    let pin = summary.agent.clone();
    let close = summary.agent.clone();
    let status = summary.run.as_ref().map(|run| {
        let mut text = crate::agents_view::run_status(run.status).to_owned();
        if let Some(time) = run_time(run) {
            text.push(' ');
            text.push_str(&time);
        }
        (status_color(run.status), text)
    });
    // Tabs give way on a narrow window: their texts shorten, the close button and
    // the overflow control stay in reach (QA, 960x640).
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .flex()
        .min_w(px(TAB_MIN_WIDTH))
        .items_center()
        .gap_1()
        .px_2()
        .py_1()
        .rounded_md()
        .text_xs()
        .cursor_pointer()
        .when(shown, |tab| tab.bg(theme::surface_hover()))
        .hover(|style| style.bg(theme::surface_hover()))
        .on_click(cx.listener(move |model, _, _, cx| model.pin_agent_dock(pin.clone(), cx)))
        .child(crate::widgets::status_dot(summary.color))
        // A run started that the dock did not show (10.3, rule 1).
        .when(marked && !shown, |tab| {
            let selector = format!("agent-dock-tab-new:{key}");
            tab.child(
                div()
                    .debug_selector(move || selector.clone())
                    .flex_shrink_0()
                    .size_1()
                    .rounded_full()
                    .bg(theme::accent()),
            )
        })
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_color(if shown {
                    theme::text()
                } else {
                    theme::text_muted()
                })
                .child(summary.label.clone()),
        )
        .when_some(status, |tab, (color, text)| {
            tab.child(div().min_w_0().truncate().text_color(color).child(text))
        })
        .when(summary.needs_attention(), |tab| {
            tab.child(crate::widgets::icon(
                Icon::TriangleAlert,
                px(12.0),
                theme::warning(),
            ))
        })
        .child(
            div()
                .id(SharedString::from(format!("agent-dock-tab-close:{key}")))
                .debug_selector({
                    let selector = format!("agent-dock-tab-close:{key}");
                    move || selector.clone()
                })
                .flex_shrink_0()
                .px_1()
                .rounded_sm()
                .text_color(theme::text_subtle())
                .hover(|style| style.text_color(theme::text()))
                .on_click(cx.listener(move |model, _, _, cx| {
                    cx.stop_propagation();
                    model.close_agent_dock_tab(close.clone(), cx);
                }))
                .child("×"),
        )
        .into_any_element()
}

fn expanded_dock(
    model: &AppModel,
    tabs: &[AgentSummary],
    shown: Option<&AgentSummary>,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let now = Instant::now();
    let following = !model.agent_dock.follow.pinned();
    let chip = model
        .agent_dock
        .follow
        .chip(now)
        .map(|agent| summarize(model, agent, cx).label);
    let overflow = tabs.len().saturating_sub(VISIBLE_TABS);
    let (shown_run, place) = shown
        .map(|summary| shown_run(model, summary, cx))
        .unwrap_or_default();
    // The chip shows only for an older run: the latest needs no way back.
    let place = place.filter(|(index, _)| *index > 1);
    let header = div()
        .id("agent-dock-header")
        .h(STRIP_HEIGHT)
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap_1()
        .px_1()
        .border_b_1()
        .border_color(theme::border())
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap_1()
                .overflow_hidden()
                .children(tabs.iter().take(VISIBLE_TABS).map(|summary| {
                    let is_shown = shown.is_some_and(|shown| shown.agent == summary.agent);
                    let marked = model.agent_dock.follow.is_marked(&summary.agent);
                    tab(summary, is_shown, marked, cx)
                }))
                .when(overflow > 0, |row| {
                    row.child(
                        div()
                            .flex_shrink_0()
                            .child(crate::select_menu::compact_control(
                                "agent-dock-more".to_owned(),
                                "More",
                                format!("+{overflow}"),
                                true,
                                cx,
                            )),
                    )
                }),
        )
        .when_some(chip, |header, label| {
            header.child(switch_chip("agent-dock-switch-chip", &label, cx))
        })
        .when_some(place, |header, place| header.child(run_chip(place, cx)))
        .child(agent_follow_control("agent-dock-follow", following, cx))
        .when_some(
            shown.map(|summary| summary.agent.clone()),
            |header, agent| {
                header.child(dock_button(
                    "agent-dock-open",
                    Icon::Bot,
                    crate::widgets::Tooltip::new("Open in Agents"),
                    cx,
                    move |model, _, cx| model.open_agent_in_agents(agent.clone(), cx),
                ))
            },
        )
        .child(dock_button(
            "agent-dock-collapse",
            Icon::ChevronDown,
            crate::widgets::Tooltip::new("Fold to the strip").with_shortcut("Esc"),
            cx,
            |model, _, cx| model.set_agent_dock_mode(DockMode::Strip, cx),
        ))
        .child(dock_button(
            "agent-dock-hide",
            Icon::X,
            crate::widgets::Tooltip::new("Hide the agent console").with_shortcut("Ctrl+J"),
            cx,
            |model, _, cx| model.set_agent_dock_mode(DockMode::Hidden, cx),
        ));
    let now_line = shown.map_or_else(String::new, |summary| now_text(model, summary, cx));
    let body = match shown {
        None => div()
            .flex_1()
            .p_2()
            .text_xs()
            .text_color(theme::text_muted())
            .child("No agent activity yet · Ctrl+J")
            .into_any_element(),
        Some(summary) if summary.api => api_lines(model, summary, "agent-dock-api", cx),
        Some(summary) => {
            let filter = model
                .agents
                .read(cx)
                .event_filters
                .get(DOCK_SURFACE)
                .copied()
                .unwrap_or_default();
            console(
                model,
                ConsoleSurface {
                    key: DOCK_SURFACE.to_owned(),
                    agent_label: &summary.label,
                    run: shown_run.as_ref(),
                    filter,
                    expanded: true,
                    narrow: false,
                },
                cx,
            )
        }
    };
    div()
        .flex()
        .flex_col()
        .size_full()
        .min_h_0()
        .child(header)
        .child(
            div()
                .id("agent-dock-now")
                .debug_selector(|| "agent-dock-now".into())
                .h(px(24.0))
                .flex_shrink_0()
                .flex()
                .items_center()
                .px_2()
                .text_xs()
                .text_color(theme::text_muted())
                .truncate()
                .child(now_line),
        )
        .child(
            div()
                .id("agent-dock-console")
                .debug_selector(|| "agent-dock-console".into())
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .px_1()
                .pb_1()
                .child(body),
        )
        .into_any_element()
}

/// `Switch to {agent} →`: the switch the developer's reading held back
/// (10.3, rule 4), now.
pub(crate) fn switch_chip(id: &'static str, label: &str, cx: &mut Context<AppModel>) -> AnyElement {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .flex_shrink_0()
        .px_2()
        .py_1()
        .rounded_full()
        .bg(theme::accent())
        .text_xs()
        .text_color(theme::surface())
        .cursor_pointer()
        .on_click(cx.listener(|model, _, _, cx| model.take_agent_dock_switch(cx)))
        .child(format!("Switch to {label} →"))
        .into_any_element()
}

/// `Following`, or `Pinned · Resume following` (9.4).
pub(crate) fn agent_follow_control(
    selector: &'static str,
    following: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    div()
        .id(selector)
        .debug_selector(move || selector.into())
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap_1()
        .px_2()
        .py_1()
        .rounded_md()
        .text_xs()
        .text_color(if following {
            theme::success()
        } else {
            theme::accent()
        })
        .when(!following, |control| {
            control
                .cursor_pointer()
                .hover(|style| style.bg(theme::surface_hover()))
                .on_click(cx.listener(|model, _, _, cx| model.resume_agent_dock_following(cx)))
        })
        .child(crate::widgets::status_dot(if following {
            theme::success()
        } else {
            theme::text_subtle()
        }))
        .child(if following {
            "Following"
        } else {
            "Pinned · Resume following"
        })
        .into_any_element()
}

/// An API Mode agent's live lines, oldest first (vector 10). `key` names the
/// surface's own scroll and its elements: the dock's, or the Run tab's.
pub(crate) fn api_lines(
    model: &AppModel,
    summary: &AgentSummary,
    key: &str,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let key = key.to_owned();
    let (lines, view) = {
        let agents = model.agents.read(cx);
        let lines = agents
            .api_activity
            .get(&summary.agent)
            .cloned()
            .unwrap_or_default();
        let view =
            agents
                .consoles
                .lines_view(&key, lines.len(), lines.last().map_or(0, String::len));
        (lines, view)
    };
    let follow_key = key.clone();
    let pill_key = key.clone();
    let mut frame = crate::agent_console::view::frame(key.clone(), key.clone(), true)
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .border_b_1()
                .border_color(theme::border())
                .text_xs()
                .child(
                    div()
                        .flex_1()
                        .text_color(theme::text())
                        .child(format!("API · {}", summary.label)),
                )
                .child(follow_control(
                    format!("{key}-follow"),
                    view.following,
                    cx,
                    move |model, cx| model.console_follow_lines(&follow_key, cx),
                )),
        )
        .child(
            div()
                .id(SharedString::from(format!("{key}-lines")))
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&view.scroll)
                .p_2()
                .when(lines.is_empty(), |body| {
                    body.child(
                        div()
                            .text_xs()
                            .text_color(theme::text_muted())
                            .child("No API activity in this session."),
                    )
                })
                .children(lines.into_iter().map(|line| {
                    div()
                        .py_1()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(line)
                })),
        );
    if !view.following && view.unseen > 0 {
        frame = frame.child(crate::agent_console::view::new_pill(
            format!("{key}-pill"),
            view.unseen,
            cx,
            move |model, cx| model.console_follow_lines(&pill_key, cx),
        ));
    }
    frame.into_any_element()
}

impl AppModel {
    /// `Open in Agents`: the page, with the agent's node selected and the
    /// dock still pinned on it (feature 25, M1).
    pub(crate) fn open_agent_in_agents(&mut self, agent: AgentIdentifier, cx: &mut Context<Self>) {
        self.select_view(AppView::Agents, cx);
        self.select_canvas_agent(agent, cx);
    }

    /// The status bar's `● {agent} · {phase} · {elapsed}`, or
    /// `No agent running` (9.5).
    pub(crate) fn agent_console_status(&self, cx: &App) -> (Option<gpui::Rgba>, String, usize) {
        let attention = self
            .agent_dock
            .tabs()
            .iter()
            .map(|agent| summarize(self, agent, cx))
            .filter(AgentSummary::needs_attention)
            .count();
        let running = self
            .agent_dock
            .shown()
            .map(|agent| summarize(self, &agent, cx))
            .filter(|summary| {
                summary
                    .run
                    .as_ref()
                    .is_some_and(|run| run.status.is_active())
            });
        match running {
            Some(summary) => {
                let line = now_text(self, &summary, cx);
                (
                    Some(summary.color),
                    format!("{} · {line}", summary.label),
                    attention,
                )
            }
            None => (None, "No agent running".to_owned(), attention),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tab_reads_the_time_running_or_how_long_ago() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-11-15T10:05:00Z").unwrap();
        let start = "2026-11-15T10:02:46Z";
        assert_eq!(time_label(true, start, start, now).as_deref(), Some("2:14"));
        // Past the hour, hours lead instead of a minute count that keeps growing.
        let long = "2026-11-15T08:02:46Z";
        assert_eq!(
            time_label(true, long, long, now).as_deref(),
            Some("2:02:14")
        );
        let end = "2026-11-15T10:02:00Z";
        assert_eq!(
            time_label(false, start, end, now).as_deref(),
            Some("3m ago")
        );
        assert_eq!(
            time_label(false, start, "2026-11-15T10:04:59Z", now).as_deref(),
            Some("just now")
        );
        assert_eq!(time_label(false, start, "not a time", now), None);
    }
}
