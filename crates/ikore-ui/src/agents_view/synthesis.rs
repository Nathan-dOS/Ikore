//! A node's synthesis on the Agents canvas (feature 25, M1; section 1 of the
//! feature): under the header and its run actions, how the agent runs, what
//! it does now, the numbers of its current or last run, a failure and its
//! running units; then `Configure`, `History`, `Open review` and `Run…`,
//! each a drawer over the synthesis. The console is the dock's.

use std::time::Instant;

use gpui::{AnyElement, Context, FontWeight, IntoElement, SharedString, div, prelude::*, px};
use ikore_core::terminals::{
    AgentExecutionMode, SessionSlot, TerminalRun, TerminalRunStatus, ThinkingLevel,
};

use crate::agents::{AgentDetailTab, AgentIdentifier};
use crate::app_model::AppModel;
use crate::icons::Icon;
use crate::theme;

use super::{action_button, icon_action, run_status, status_color};

/// What the synthesis says under the header.
pub(super) struct SynthesisFacts {
    /// The selectors' key: the role, or `free-form:{slot}`.
    pub(super) key: String,
    pub(super) agent: AgentIdentifier,
    /// `CLI · claude · sonnet · high`.
    pub(super) transport: String,
    /// What the run does now, from its pulse.
    pub(super) now: Option<String>,
    /// `12 actions · 4 read · 1 changed · 2:41`.
    pub(super) numbers: Option<String>,
    /// The quota the run used.
    pub(super) quota: Option<String>,
    /// The failed run's message.
    pub(super) failure: Option<String>,
    /// The composition units running now, when more than one: run and status.
    pub(super) units: Vec<(String, TerminalRunStatus)>,
    /// The pulsation waiting on the developer at this stage's checkpoint.
    pub(super) review: Option<String>,
}

/// `CLI · claude · sonnet · high`: transport, model and effort.
pub(super) fn transport_line(
    badge: &str,
    mode: AgentExecutionMode,
    model: Option<&str>,
    thinking: ThinkingLevel,
) -> String {
    let mut line = badge.to_owned();
    if mode == AgentExecutionMode::CliProcess {
        line.push_str(" · ");
        line.push_str(model.unwrap_or("CLI default"));
        if thinking != ThinkingLevel::Default {
            line.push_str(" · ");
            line.push_str(thinking.as_str());
        }
    }
    line
}

/// The run's actions and files, from its live pulses or the pulses of a run
/// that ended in this session, and how long it took.
pub(super) fn run_numbers(model: &AppModel, run: &TerminalRun, cx: &gpui::App) -> Option<String> {
    let agents = model.agents.read(cx);
    let counters = agents
        .live
        .get(&run.id)
        .map(|live| live.counters.clone())
        .or_else(|| agents.live.finished(&run.id).cloned());
    let mut parts = Vec::new();
    if let Some(counters) = counters {
        parts.push(format!("{} actions", counters.tool_calls));
        parts.push(format!("{} read", counters.files_read));
        parts.push(format!("{} changed", counters.files_changed));
    }
    if let Some(duration) = crate::agent_console::view::run_duration_ms(run) {
        parts.push(crate::agent_console::view::duration_label(duration));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// The quota the run used, when the meter saw it.
pub(super) fn run_quota(model: &AppModel, run: &TerminalRun, cx: &gpui::App) -> Option<String> {
    crate::quota_bar::run_delta_line(model.agents.read(cx).quota_deltas.get(&run.id)?)
}

/// What the run does now, while it runs.
pub(super) fn now_line(model: &AppModel, run: &TerminalRun, cx: &gpui::App) -> Option<String> {
    run.status
        .is_active()
        .then(|| {
            model
                .agents
                .read(cx)
                .live
                .get(&run.id)
                .map(|live| live.line(Instant::now()))
        })
        .flatten()
}

/// The composition units running now, newest last, when there is more than
/// one: `u1`, `u2`…
pub(super) fn running_units(runs: &[TerminalRun]) -> Vec<(String, TerminalRunStatus)> {
    let mut units = runs
        .iter()
        .filter(|run| matches!(run.slot, SessionSlot::Ephemeral { .. }) && run.status.is_active())
        .map(|run| (run.id.clone(), run.status))
        .collect::<Vec<_>>();
    units.reverse();
    if units.len() > 1 { units } else { Vec::new() }
}

/// The pulsation waiting at the checkpoint of the stage this agent runs.
pub(super) fn waiting_review(
    model: &AppModel,
    agent: &AgentIdentifier,
    cx: &gpui::App,
) -> Option<String> {
    let AgentIdentifier::Canonical(role) = agent else {
        return None;
    };
    let pulsations = model.pulsations.read(cx);
    let active = pulsations.active()?;
    let waiting = active.status == ikore_core::persistence_service::PulsationStatus::Checkpoint
        && ikore_core::llm::AgentRole::for_stage(crate::pulsations::stage_label(active.stage))
            == *role;
    waiting.then(|| active.id.clone())
}

fn line(text: String, selector: String) -> AnyElement {
    div()
        .debug_selector(move || selector.clone())
        .text_xs()
        .text_color(theme::text_muted())
        .child(text)
        .into_any_element()
}

/// The synthesis under the header.
pub(super) fn summary(facts: SynthesisFacts, cx: &mut Context<AppModel>) -> AnyElement {
    let SynthesisFacts {
        key,
        agent,
        transport,
        now,
        numbers,
        quota,
        failure,
        units,
        review: _,
    } = facts;
    div()
        .id(SharedString::from(format!(
            "agents-synthesis-summary:{key}"
        )))
        .flex()
        .flex_col()
        .gap_2()
        .child(line(transport, format!("agents-synthesis-transport:{key}")))
        .when_some(now, |summary, now| {
            summary.child(
                div()
                    .debug_selector({
                        let key = key.clone();
                        move || format!("agents-synthesis-now:{key}")
                    })
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::running())
                            .child("Now"),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(theme::text())
                            .child(now),
                    ),
            )
        })
        .when_some(numbers, |summary, numbers| {
            summary.child(line(numbers, format!("agents-synthesis-numbers:{key}")))
        })
        .when_some(quota, |summary, quota| {
            summary.child(line(quota, format!("agent-run-quota:{key}")))
        })
        .when_some(failure, |summary, failure| {
            summary.child(
                div()
                    .debug_selector({
                        let key = key.clone();
                        move || format!("agents-synthesis-failure:{key}")
                    })
                    .p_2()
                    .rounded_md()
                    .bg(theme::danger_soft())
                    .text_xs()
                    .text_color(theme::danger())
                    .child(failure),
            )
        })
        .when(!units.is_empty(), |summary| {
            summary.child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::text_muted())
                            .child("Units"),
                    )
                    .children(
                        units
                            .into_iter()
                            .enumerate()
                            .map(|(index, (run_id, status))| {
                                let selector = format!("agents-synthesis-unit:{}", index + 1);
                                let agent = agent.clone();
                                div()
                                    .id(SharedString::from(selector.clone()))
                                    .debug_selector(move || selector.clone())
                                    .px_2()
                                    .py(px(2.0))
                                    .rounded_md()
                                    .border_1()
                                    .border_color(theme::border())
                                    .text_xs()
                                    .cursor_pointer()
                                    .hover(|style| style.bg(theme::hover_bg()))
                                    .on_click(cx.listener(move |model, _, _, cx| {
                                        model.open_agent_run(agent.clone(), run_id.clone(), cx);
                                    }))
                                    .child(
                                        div()
                                            .flex()
                                            .gap_1()
                                            .child(format!("u{}", index + 1))
                                            .child(
                                                div()
                                                    .text_color(status_color(Some(status)))
                                                    .child(run_status(status)),
                                            ),
                                    )
                            }),
                    ),
            )
        })
        .into_any_element()
}

/// `Configure`, `History`, `Open review` and `Run…`: the drawers, and the
/// review the pulsation waits on.
pub(super) fn synthesis_actions(
    key: &str,
    tab: AgentDetailTab,
    review: Option<String>,
    composer: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let drawer = |drawer: AgentDetailTab,
                  selector: &str,
                  label: &'static str,
                  cx: &mut Context<AppModel>| {
        let selector = SharedString::from(format!("agents-synthesis-{selector}"));
        let active = tab == drawer;
        div()
            .id(selector.clone())
            .debug_selector(move || selector.to_string())
            .px_3()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(if active {
                theme::accent()
            } else {
                theme::border()
            })
            .bg(if active {
                theme::accent_soft()
            } else {
                theme::surface()
            })
            .text_sm()
            .cursor_pointer()
            .hover(|style| style.bg(theme::hover_bg()))
            .on_click(cx.listener(move |model, _, _, cx| {
                model.agents.update(cx, |agents, _| {
                    agents.detail_tab = if agents.detail_tab == drawer {
                        AgentDetailTab::default()
                    } else {
                        drawer
                    };
                });
                cx.notify();
            }))
            .child(label)
            .into_any_element()
    };
    let configure = drawer(AgentDetailTab::Configuration, "configure", "Configure", cx);
    let history = drawer(AgentDetailTab::History, "history", "History", cx);
    let run = composer.then(|| drawer(AgentDetailTab::Run, "run", "Run…", cx));
    div()
        .debug_selector({
            let key = key.to_owned();
            move || format!("agents-synthesis-actions:{key}")
        })
        .flex()
        .flex_wrap()
        .items_center()
        .gap_2()
        .child(configure)
        .child(history)
        .when_some(review, |row, pulsation_id| {
            row.child(action_button(
                "agents-synthesis-review".into(),
                "Open review".into(),
                true,
                false,
                cx,
                move |model, cx| model.open_pulsation_review(pulsation_id.clone(), cx),
            ))
        })
        .children(run)
        .into_any_element()
}

/// A drawer over the synthesis: its title, `Close` and its content.
pub(super) fn drawer(
    tab: AgentDetailTab,
    content: AnyElement,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let selector = format!("agents-drawer:{}", tab.id());
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .flex()
        .flex_col()
        .gap_3()
        .p_3()
        .rounded_lg()
        .border_1()
        .border_color(theme::border())
        .bg(theme::surface_raised())
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_sm()
                        .child(tab.label()),
                )
                .child(icon_action(
                    "agents-drawer-close".into(),
                    Icon::X,
                    "Close",
                    true,
                    cx,
                    |model, cx| {
                        model.agents.update(cx, |agents, _| {
                            agents.detail_tab = AgentDetailTab::default();
                        });
                        cx.notify();
                    },
                )),
        )
        .child(content)
        .into_any_element()
}
