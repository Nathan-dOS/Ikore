//! The work panel's Run tab (ADR-042, item 6; PI-Desktop PD7): the run of the
//! agent the bottom dock follows, in the panel's column.
//!
//! The tab and the dock show the same agent: they follow, pin and switch
//! together, so they never disagree about who is at work. The tab keeps its
//! own place in the console, so scrolling here leaves the dock where it is.
//! Reading the tab holds a switch back as reading the dock does
//! (specification 10.3, rule 4), and touching it counts as touching the dock.
//! Nothing opens the panel or picks this tab but the developer (item 7).

use std::time::Instant;

use gpui::{AnyElement, App, Context, FontWeight, KeyDownEvent, Window, div, prelude::*};

use crate::agent_console::view::{ConsoleSurface, console};
use crate::agent_dock_view::{agent_follow_control, api_lines, now_text, summarize, switch_chip};
use crate::app_model::{AppModel, AppView};
use crate::icons::Icon;
use crate::theme;
use crate::work_panel::{WorkPanelTab, note};

/// The console surface the tab renders: its own scroll, selection and filter.
pub(crate) const RUN_TAB_SURFACE: &str = "run-tab";

/// Whether the tab is on screen: the panel is open on it, under a view that
/// has the panel.
pub(crate) fn on_screen(model: &AppModel, window: &Window, cx: &App) -> bool {
    model.work_panel.tab == WorkPanelTab::Run
        && model.active_view() != AppView::Home
        && !crate::context_rail::effective_collapsed(model, window, cx).0
}

/// The followed agent, what it does now, and its run's console.
pub(crate) fn run_tab(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let column = div()
        .id("work-run")
        .debug_selector(|| "work-run".into())
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .gap_1()
        .px_2()
        .pb_2()
        // Touching the tab holds a switch back for a moment, as touching the dock does.
        .capture_any_mouse_down(cx.listener(|model, _, _, cx| {
            model.note_agent_dock_interaction(cx);
        }))
        .on_key_down(cx.listener(|model, _: &KeyDownEvent, _, cx| {
            model.note_agent_dock_interaction(cx);
        }));
    let Some(agent) = model.agent_dock.shown() else {
        return column
            .child(note("No agent activity yet."))
            .child(note(
                "When an agent runs, the run the agent dock follows shows here.",
            ))
            .into_any_element();
    };
    let summary = summarize(model, &agent, cx);
    let following = !model.agent_dock.follow.pinned();
    let chip = model
        .agent_dock
        .follow
        .chip(Instant::now())
        .map(|agent| summarize(model, agent, cx).label);
    let header = div()
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap_2()
        .child(crate::widgets::status_dot(summary.color))
        .child(
            div()
                .id("run-tab-agent")
                .debug_selector(|| "run-tab-agent".into())
                .flex_1()
                .min_w_0()
                .truncate()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::text())
                .child(summary.label.clone()),
        )
        .child(crate::widgets::icon_button(
            "run-tab-open",
            Icon::Bot,
            crate::widgets::Tooltip::new("Open in Agents"),
            false,
            None,
            true,
            cx,
            move |model, _, cx| model.open_agent_in_agents(agent.clone(), cx),
        ));
    let follow = div()
        .flex_shrink_0()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_1()
        .child(agent_follow_control("run-tab-follow", following, cx))
        .when_some(chip, |row, label| {
            row.child(switch_chip("run-tab-switch-chip", &label, cx))
        });
    let now = div()
        .id("run-tab-now")
        .debug_selector(|| "run-tab-now".into())
        .flex_shrink_0()
        .text_xs()
        .text_color(theme::text_muted())
        .truncate()
        .child(now_text(model, &summary, cx));
    let body = if summary.api {
        api_lines(model, &summary, "run-tab-api", cx)
    } else {
        let filter = model
            .agents
            .read(cx)
            .event_filters
            .get(RUN_TAB_SURFACE)
            .copied()
            .unwrap_or_default();
        console(
            model,
            ConsoleSurface {
                key: RUN_TAB_SURFACE.to_owned(),
                agent_label: &summary.label,
                run: summary.run.as_ref(),
                filter,
                expanded: true,
                narrow: true,
            },
            cx,
        )
    };
    column
        .child(header)
        .child(follow)
        .child(now)
        .child(div().flex_1().min_h_0().flex().flex_col().child(body))
        .into_any_element()
}
