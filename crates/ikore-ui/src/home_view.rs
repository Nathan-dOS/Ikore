//! Home: the shell's centre when nothing more specific is open. It answers three questions
//! without a click — where is the active pulsation, what should I do next, and what is waiting on
//! me — then shows the week at a glance (tokens, agent run times, pulsation durations; see
//! `home_charts`). It stays read-mostly: the only controls are the flow steps and jumps to other
//! views.

use gpui::{
    AnimationExt, AnyElement, Context, FontWeight, IntoElement, SharedString, Window, div,
    prelude::*, px,
};

use crate::app_model::{AppModel, AppView};
use crate::context_rail::Density;
use crate::flow::{FlowTarget, NextStep};
use crate::icons::Icon;
use crate::pipeline_status::PipelineStatus;
use crate::theme;
use crate::widgets::{ICON_SIZE, icon, pipeline_stepper, status_dot};

/// Content width cap so the hero reads as a card rather than a banner on wide windows, with
/// room for two charts side by side.
const HOME_MAX_WIDTH: f32 = 1120.0;
/// Below this window width the three secondary cards stack instead of sharing a row.
const HOME_STACK_BELOW: f32 = 1180.0;
/// Activity lines shown on Home; the rest stays in Logs.
const HOME_ACTIVITY_LIMIT: usize = 8;

pub(crate) fn home_panel(
    model: &AppModel,
    window: &Window,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let project_open = model.workspace.read(cx).project_id.is_some();
    let steps = crate::flow::next_steps(model, cx);
    let stacked = window.viewport_size().width < px(HOME_STACK_BELOW);

    let body = if project_open {
        let (headline, charts) = crate::home_charts::insights(model, stacked, cx);
        div()
            .w_full()
            .max_w(px(HOME_MAX_WIDTH))
            .flex()
            .flex_col()
            .gap_4()
            .child(hero(model, cx))
            .child(next_steps(steps, cx))
            .child(headline)
            .child(
                div()
                    .id("home-cards")
                    .debug_selector(|| "home-cards".into())
                    .w_full()
                    .flex()
                    .when(stacked, |row| row.flex_col())
                    .when(!stacked, |row| row.items_start())
                    .gap_4()
                    .child(waiting_card(model, cx))
                    .child(health_card(model, cx))
                    .child(activity_card(model, cx)),
            )
            .child(charts)
    } else {
        div()
            .w_full()
            .max_w(px(HOME_MAX_WIDTH))
            .flex()
            .flex_col()
            .gap_4()
            .child(open_project(steps, cx))
    };

    div()
        .id("home-scroll")
        .debug_selector(|| "home-scroll".into())
        .flex_1()
        .min_w_0()
        .min_h_0()
        .overflow_y_scroll()
        .p_6()
        .flex()
        .flex_col()
        .items_center()
        .child(body)
        .into_any_element()
}

/// The active pulsation: name, stage stepper and current state. The header carries the same
/// facts in one line; here they get room.
fn hero(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let active = {
        let pulsations = model.pulsations.read(cx);
        let agents = model.agents.read(cx);
        pulsations.active().map(|active| {
            (
                SharedString::from(active.feature_name.trim().to_string()),
                PipelineStatus::derive(active, agents.snapshot.as_ref()),
            )
        })
    };
    let card = div()
        .id("home-hero")
        .debug_selector(|| "home-hero".into())
        .w_full()
        .p_5()
        .rounded_lg()
        .bg(theme::surface_raised())
        .flex()
        .flex_col()
        .gap_4()
        .child(
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child("ACTIVE PULSATION"),
        );
    match active {
        Some((feature, status)) => card
            .child(
                div()
                    .id("home-hero-title")
                    .debug_selector(|| "home-hero-title".into())
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_ellipsis()
                    .child(if feature.is_empty() {
                        SharedString::from("Untitled pulsation")
                    } else {
                        feature
                    }),
            )
            .child(pipeline_stepper(&status, "home-pipeline", false))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .child(hero_dot(status.tone))
                    .child(div().child(format!("{} · {}", status.agent_name, status.state))),
            )
            .children(crate::home_charts::active_timeline(model, cx))
            .into_any_element(),
        None => card
            .child(
                div()
                    .id("home-hero-title")
                    .debug_selector(|| "home-hero-title".into())
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text_muted())
                    .child("No active pulsation"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(theme::text_muted())
                    .child("Compose one from the Forge backlog, or pick one in Pulsations."),
            )
            .into_any_element(),
    }
}

/// The hero's tone dot breathes while a stage is actually running — the one place the shell
/// moves on its own — unless Windows asks for reduced motion.
fn hero_dot(tone: crate::pipeline_status::StatusTone) -> AnyElement {
    let color = tone.color();
    if tone != crate::pipeline_status::StatusTone::Active || crate::motion::reduced_motion() {
        return status_dot(color);
    }
    div()
        .flex_shrink_0()
        .size_2()
        .rounded_full()
        .bg(color)
        .with_animation("home-hero-dot", crate::motion::pulse(), |dot, delta| {
            dot.opacity(delta)
        })
        .into_any_element()
}

/// The flow engine's suggestions: the primary step as one large control, the rest as quiet
/// alternatives. Everything runs through `run_next_step`, the same entry point as `ctrl-enter`.
fn next_steps(steps: Vec<NextStep>, cx: &mut Context<AppModel>) -> AnyElement {
    let mut steps = steps.into_iter();
    let Some(primary) = steps.next() else {
        return div()
            .id("home-next-step-empty")
            .debug_selector(|| "home-next-step-empty".into())
            .text_sm()
            .text_color(theme::text_muted())
            .child("Nothing to do right now.")
            .into_any_element();
    };
    let target = primary.target.clone();
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_3()
        .child(
            div()
                .id("home-next-step")
                .debug_selector(|| "home-next-step".into())
                .flex()
                .items_center()
                .gap_3()
                .px_4()
                .py_3()
                .rounded_lg()
                .bg(theme::accent())
                .text_color(theme::canvas())
                .cursor_pointer()
                .tab_index(0)
                .hover(|style| style.opacity(theme::HOVER_OPACITY))
                .on_click(cx.listener(move |model, _, window, cx| {
                    model.run_next_step(target.clone(), window, cx);
                }))
                .child(icon(primary.icon, ICON_SIZE, theme::canvas()))
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(primary.label.clone()),
                )
                .child(
                    div()
                        .font_family(theme::FONT_MONO)
                        .text_xs()
                        .opacity(0.8)
                        .child("ctrl-enter"),
                ),
        )
        .children(steps.map(|step| {
            let target = step.target.clone();
            div()
                .id(SharedString::from(format!("home-step-{}", step.id)))
                .debug_selector(move || format!("home-step-{}", step.id))
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_2()
                .rounded_md()
                .border_1()
                .border_color(theme::border())
                .text_sm()
                .text_color(theme::text())
                .cursor_pointer()
                .hover(|style| style.bg(theme::surface_raised()))
                .on_click(cx.listener(move |model, _, window, cx| {
                    model.run_next_step(target.clone(), window, cx);
                }))
                .child(icon(step.icon, px(14.0), theme::text_muted()))
                .child(step.label.clone())
        }))
        .into_any_element()
}

/// No project yet: one call to action, nothing else to look at.
fn open_project(steps: Vec<NextStep>, cx: &mut Context<AppModel>) -> AnyElement {
    let target = steps
        .into_iter()
        .next()
        .map(|step| step.target)
        .unwrap_or(FlowTarget::OpenProject);
    div()
        .w_full()
        .p_6()
        .rounded_lg()
        .bg(theme::surface_raised())
        .flex()
        .flex_col()
        .items_start()
        .gap_3()
        .child(
            div()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .child("Open a project to begin"),
        )
        .child(
            div()
                .text_sm()
                .text_color(theme::text_muted())
                .child("Ikore works inside one workspace folder at a time."),
        )
        .child(
            div()
                .id("home-open-project")
                .debug_selector(|| "home-open-project".into())
                .flex()
                .items_center()
                .gap_2()
                .px_4()
                .py_2()
                .rounded_md()
                .bg(theme::accent())
                .text_color(theme::canvas())
                .cursor_pointer()
                .tab_index(0)
                .hover(|style| style.opacity(theme::HOVER_OPACITY))
                .on_click(cx.listener(move |model, _, window, cx| {
                    model.run_next_step(target.clone(), window, cx);
                }))
                .child(icon(Icon::FolderOpen, ICON_SIZE, theme::canvas()))
                .child("Open project"),
        )
        .into_any_element()
}

fn card(id: &'static str, eyebrow: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .flex_1()
        .min_w_0()
        .p_4()
        .rounded_lg()
        .bg(theme::surface_raised())
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child(eyebrow),
        )
}

/// Decisions the runtime is parked on, one clickable line each.
fn waiting_card(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let rows = crate::context_rail::waiting_rows(model, Density::Hero, cx);
    let card = card("home-waiting", "WAITING ON YOU");
    if rows.is_empty() {
        card.child(
            div()
                .text_sm()
                .text_color(theme::text_muted())
                .child("Nothing waiting on you"),
        )
        .into_any_element()
    } else {
        card.children(rows).into_any_element()
    }
}

/// The same runtime facts as the context rail, with room to breathe.
fn health_card(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let lines = crate::context_rail::health_lines(model, cx);
    card("home-health", "RUNTIME")
        .cursor_pointer()
        .hover(|style| style.border_color(theme::text_muted()))
        .on_click(cx.listener(|model, _, _, cx| {
            let page = model.settings_health_page(cx);
            model.open_settings(Some(page), cx);
        }))
        .children(lines.into_iter().map(|(glyph, color, text)| {
            div()
                .min_w_0()
                .flex()
                .items_center()
                .gap_2()
                .font_family(theme::FONT_MONO)
                .text_sm()
                .text_color(color)
                .child(icon(glyph, px(14.0), color))
                .child(div().flex_1().min_w_0().text_ellipsis().child(text))
        }))
        .into_any_element()
}

/// The last few milestones, newest first; each line jumps where it happened.
fn activity_card(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let entries: Vec<(usize, crate::activity::ActivityEntry)> = model
        .activity
        .recent(HOME_ACTIVITY_LIMIT)
        .cloned()
        .enumerate()
        .collect();
    let card = card("home-activity", "RECENT ACTIVITY");
    if entries.is_empty() {
        return card
            .child(
                div()
                    .text_sm()
                    .text_color(theme::text_muted())
                    .child("Nothing has happened yet this session."),
            )
            .into_any_element();
    }
    card.children(entries.into_iter().map(|(index, entry)| {
        let id = SharedString::from(format!("home-activity-{index}"));
        let selector = id.clone();
        let color = if entry.attention {
            theme::warning()
        } else {
            theme::text()
        };
        let target = entry.target.clone();
        div()
            .id(id)
            .debug_selector(move || selector.to_string())
            .min_w_0()
            .flex()
            .items_center()
            .gap_2()
            .px_1()
            .py_1()
            .rounded_md()
            .text_sm()
            .when(target.is_some(), |row| {
                row.cursor_pointer()
                    .hover(|style| style.bg(theme::surface_raised()))
            })
            .when_some(target, |row, target| {
                row.on_click(cx.listener(move |model, _, window, cx| {
                    model.run_next_step(target.clone(), window, cx);
                }))
            })
            .child(
                div()
                    .flex_shrink_0()
                    .font_family(theme::FONT_MONO)
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(crate::time::clock_hhmm(entry.at)),
            )
            .child(icon(entry.kind.icon(), px(14.0), color))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_ellipsis()
                    .text_color(color)
                    .child(entry.summary.clone()),
            )
    }))
    .child(
        div()
            .id("home-open-logs")
            .debug_selector(|| "home-open-logs".into())
            .text_xs()
            .text_color(theme::accent())
            .cursor_pointer()
            .hover(|style| style.opacity(theme::HOVER_OPACITY))
            .on_click(cx.listener(|model, _, _, cx| model.select_view(AppView::Logs, cx)))
            .child("Open logs"),
    )
    .into_any_element()
}
