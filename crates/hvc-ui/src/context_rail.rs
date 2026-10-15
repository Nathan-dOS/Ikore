//! The context rail: the right-hand column that keeps the state of the run visible while a
//! workbench view owns the centre. Five short cards — pipeline, what is waiting on the developer,
//! the flow engine's next actions, runtime health and the active pulsation's token usage — each
//! collapsing to one icon on the 48px strip. Home does not render it: Home *is* the rail
//! unfolded.

use gpui::{
    AnyElement, App, Context, FontWeight, IntoElement, SharedString, Window, div, prelude::*, px,
};
use hvc_core::orchestration::IndexingEvent;
use hvc_core::terminals::CliAvailabilityState;

use crate::app_model::{AppModel, AppView, TabBadges};
use crate::flow::FlowTarget;
use crate::icons::Icon;
use crate::pipeline_status::PipelineStatus;
use crate::sizing::{
    CONTEXT_RAIL_AUTO_COLLAPSE_BELOW, CONTEXT_RAIL_COLLAPSED_WIDTH, CONTEXT_RAIL_WIDTH,
};
use crate::theme;
use crate::widgets::{badge, icon, icon_button, pipeline_stepper, status_dot};

/// How much room a card has: the rail's compact column or Home's wide cards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Density {
    Compact,
    #[allow(dead_code)]
    Hero,
}

/// `(collapsed, forced)`: the rail is folded when the developer folded it or when the window is
/// too narrow for the centre column to keep every split above its minimum width.
pub(crate) fn effective_collapsed(model: &AppModel, window: &Window, cx: &App) -> (bool, bool) {
    let forced = window.viewport_size().width < CONTEXT_RAIL_AUTO_COLLAPSE_BELOW;
    (forced || model.context_rail_collapsed(cx), forced)
}

/// The rail element, or `None` on Home.
pub(crate) fn context_rail(
    model: &AppModel,
    window: &Window,
    cx: &mut Context<AppModel>,
) -> Option<AnyElement> {
    if model.active_view() == AppView::Home {
        return None;
    }
    let (collapsed, forced) = effective_collapsed(model, window, cx);
    let toggle = icon_button(
        "context-rail-toggle",
        if collapsed {
            Icon::PanelRightOpen
        } else {
            Icon::PanelRightClose
        },
        if forced {
            crate::widgets::Tooltip::new("Widen the window to expand the context rail")
        } else if collapsed {
            crate::widgets::Tooltip::new("Expand context rail").with_shortcut("ctrl-b")
        } else {
            crate::widgets::Tooltip::new("Collapse context rail").with_shortcut("ctrl-b")
        },
        false,
        None,
        !forced,
        cx,
        |model, _, cx| model.toggle_context_rail(cx),
    );
    let rail = div()
        .id("context-rail")
        .debug_selector(|| "context-rail".into())
        .w(if collapsed {
            CONTEXT_RAIL_COLLAPSED_WIDTH
        } else {
            CONTEXT_RAIL_WIDTH
        })
        .flex_shrink_0()
        .min_h_0()
        .flex()
        .flex_col()
        .border_l_1()
        .border_color(theme::border())
        .bg(theme::surface());
    Some(if collapsed {
        rail.items_center()
            .gap_1()
            .py_2()
            .child(toggle)
            .child(crate::widgets::rail_divider())
            .child(strip(model, cx))
            .into_any_element()
    } else {
        rail.child(
            div()
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_between()
                .pl_3()
                .pr_1()
                .py_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child("CONTEXT"),
                )
                .child(toggle),
        )
        .child(
            div()
                .id("context-rail-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .px_2()
                .pb_2()
                .flex()
                .flex_col()
                .gap_2()
                .child(pipeline_card(model, cx))
                .child(waiting_card(model, cx))
                .child(actions_card(model, cx))
                .child(health_card(model, cx))
                .when_some(usage_card(model, cx), |column, card| column.child(card)),
        )
        .into_any_element()
    })
}

fn card(id: &'static str, eyebrow: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .flex_shrink_0()
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(theme::border())
        .bg(theme::canvas())
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

fn active_status(model: &AppModel, cx: &App) -> Option<(SharedString, PipelineStatus)> {
    let pulsations = model.pulsations.read(cx);
    let agents = model.agents.read(cx);
    pulsations.active().map(|active| {
        (
            SharedString::from(active.feature_name.trim().to_string()),
            PipelineStatus::derive(active, agents.snapshot.as_ref()),
        )
    })
}

/// Where the active pulsation is. Clicking anywhere on the card opens Pulsations.
fn pipeline_card(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let card = card("rail-card-pipeline", "PIPELINE")
        .cursor_pointer()
        .hover(|style| style.border_color(theme::text_muted()))
        .on_click(cx.listener(|model, _, _, cx| model.select_view(AppView::Pulsations, cx)));
    match active_status(model, cx) {
        Some((feature, status)) => card
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_ellipsis()
                    .child(if feature.is_empty() {
                        SharedString::from("Untitled pulsation")
                    } else {
                        feature
                    }),
            )
            .child(pipeline_stepper(&status, "rail-pipeline", true))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(status_dot(status.tone.color()))
                    .child(format!("{} · {}", status.agent_name, status.state)),
            )
            .into_any_element(),
        None => card
            .child(
                div()
                    .text_sm()
                    .text_color(theme::text_muted())
                    .child("No active pulsation"),
            )
            .into_any_element(),
    }
}

/// One line per kind of decision the runtime is parked on. Only non-zero rows render.
pub(crate) fn waiting_rows(
    model: &AppModel,
    density: Density,
    cx: &mut Context<AppModel>,
) -> Vec<AnyElement> {
    let (hitl, failures, active_id) = {
        let pulsations = model.pulsations.read(cx);
        (
            pulsations.awaiting_hitl,
            pulsations.awaiting_failure,
            pulsations.active_id(),
        )
    };
    let clarifications = model.resolution.read(cx).chips.len();
    let first_chip = model
        .resolution
        .read(cx)
        .chips
        .first()
        .map(|chip| chip.id.clone());
    let fallbacks = model.agents.read(cx).fallbacks.len();

    let review_target = match active_id {
        Some(id) => {
            FlowTarget::PulsationSection(id, crate::pulsations::PulsationDetailSection::Review)
        }
        None => FlowTarget::View(AppView::Pulsations),
    };
    let prefix = match density {
        Density::Compact => "rail-waiting-",
        Density::Hero => "home-waiting-",
    };
    let mut rows: Vec<AnyElement> = Vec::new();
    let mut push =
        |id: &'static str, count: usize, label: &'static str, color, target: WaitingTarget| {
            if count == 0 {
                return;
            }
            let id = SharedString::from(format!("{prefix}{id}"));
            let selector = id.clone();
            let row = div()
                .id(id)
                .debug_selector(move || selector.to_string())
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .hover(|style| style.bg(theme::surface_raised()))
                .on_click(cx.listener(move |model, _, window, cx| match &target {
                    WaitingTarget::Flow(target) => model.run_next_step(target.clone(), window, cx),
                    WaitingTarget::Chip(id) => model.toggle_resolution_chip(id.clone(), cx),
                }))
                .child(badge(count, theme::surface_raised(), color))
                .child(
                    div()
                        .text_color(theme::text())
                        .when(density == Density::Compact, |label| label.text_xs())
                        .when(density == Density::Hero, |label| label.text_sm())
                        .child(label),
                );
            rows.push(row.into_any_element());
        };
    push(
        "hitl",
        hitl,
        "review(s) waiting for approval",
        theme::warning(),
        WaitingTarget::Flow(review_target),
    );
    push(
        "failures",
        failures,
        "stage failure(s) to resolve",
        theme::danger(),
        WaitingTarget::Flow(FlowTarget::View(AppView::Pulsations)),
    );
    push(
        "clarifications",
        clarifications,
        "clarification(s) from Specification",
        theme::accent(),
        match first_chip {
            Some(id) => WaitingTarget::Chip(id),
            None => WaitingTarget::Flow(FlowTarget::View(AppView::Pulsations)),
        },
    );
    push(
        "fallbacks",
        fallbacks,
        "CLI fallback decision(s)",
        theme::warning(),
        WaitingTarget::Flow(FlowTarget::View(AppView::Agents)),
    );
    rows
}

#[derive(Clone)]
enum WaitingTarget {
    Flow(FlowTarget),
    Chip(String),
}

fn waiting_card(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let rows = waiting_rows(model, Density::Compact, cx);
    let card = card("rail-card-waiting", "WAITING ON YOU");
    if rows.is_empty() {
        card.child(
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child("Nothing waiting on you"),
        )
        .into_any_element()
    } else {
        card.children(rows).into_any_element()
    }
}

/// The flow engine's next steps as icon rows; the first one is the primary (`ctrl-enter`).
fn actions_card(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let steps = crate::flow::next_steps(model, cx);
    let card = card("rail-card-actions", "NEXT");
    if steps.is_empty() {
        return card
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child("Nothing to do right now"),
            )
            .into_any_element();
    }
    card.children(steps.into_iter().enumerate().map(|(index, step)| {
        let primary = index == 0;
        let target = step.target.clone();
        let id = SharedString::from(format!("rail-step-{}", step.id));
        let selector = id.clone();
        let ink = if primary {
            theme::canvas()
        } else {
            theme::text()
        };
        div()
            .id(id)
            .debug_selector(move || selector.to_string())
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_md()
            .cursor_pointer()
            .when(primary, |row| row.bg(theme::accent()))
            .when(!primary, |row| {
                row.hover(|style| style.bg(theme::surface_raised()))
            })
            .when(primary, |row| {
                row.hover(|style| style.opacity(theme::HOVER_OPACITY))
            })
            .text_color(ink)
            .on_click(cx.listener(move |model, _, window, cx| {
                model.run_next_step(target.clone(), window, cx);
            }))
            .child(icon(step.icon, px(14.0), ink))
            .child(div().text_xs().child(step.label.clone()))
    }))
    .into_any_element()
}

/// Three monospace facts about the runtime, plus background work when there is any.
pub(crate) fn health_lines(model: &AppModel, cx: &App) -> Vec<(Icon, gpui::Rgba, String)> {
    let mut lines = Vec::new();

    let cli = model.agents.read(cx).snapshot.as_ref().map(|snapshot| {
        let ready = snapshot
            .discovery
            .iter()
            .filter(|cli| cli.state == CliAvailabilityState::Available)
            .count();
        (ready, snapshot.discovery.len())
    });
    lines.push(match cli {
        Some((ready, total)) if total > 0 => (
            Icon::Cpu,
            if ready > 0 {
                theme::text()
            } else {
                theme::warning()
            },
            format!("CLI  {ready} of {total} ready"),
        ),
        Some(_) => (Icon::Cpu, theme::text_muted(), "CLI  none detected".into()),
        None => (Icon::Cpu, theme::text_muted(), "CLI  not checked".into()),
    });

    let settings = model.settings.read(cx);
    lines.push(match settings.snapshot.as_ref() {
        Some(snapshot) => {
            let providers = snapshot.connections.iter().filter(|c| c.enabled).count();
            let cooling = snapshot
                .resilience
                .states
                .iter()
                .filter(|state| state.state != "closed")
                .count();
            (
                Icon::Zap,
                if cooling > 0 {
                    theme::warning()
                } else {
                    theme::text()
                },
                if cooling > 0 {
                    format!("LLM  {providers} provider(s) · {cooling} cooling")
                } else {
                    format!("LLM  {providers} provider(s)")
                },
            )
        }
        None => (Icon::Zap, theme::text_muted(), "LLM  not checked".into()),
    });

    let embeddings_enabled = settings
        .snapshot
        .as_ref()
        .is_some_and(|snapshot| snapshot.config.embedding_enabled);
    lines.push(match (&settings.embedding_readiness, embeddings_enabled) {
        (_, false) => (Icon::Database, theme::text_muted(), "EMB  disabled".into()),
        (Some(report), true) => {
            let ok = report.ollama_reachable && report.model_installed && report.embedding_call_ok;
            (
                Icon::Database,
                if ok { theme::text() } else { theme::warning() },
                if ok {
                    format!("EMB  ready · {}", report.model)
                } else if !report.ollama_reachable {
                    "EMB  Ollama unreachable".into()
                } else if !report.model_installed {
                    format!("EMB  {} missing", report.model)
                } else {
                    "EMB  embedding call failed".into()
                },
            )
        }
        (None, true) => (
            Icon::Database,
            theme::text_muted(),
            "EMB  not checked".into(),
        ),
    });

    match model.indexing_status() {
        Some(IndexingEvent::Progress { remaining, .. }) => lines.push((
            Icon::Database,
            theme::accent(),
            format!("IDX  {remaining} remaining"),
        )),
        Some(IndexingEvent::Paused { remaining, .. }) => lines.push((
            Icon::Pause,
            theme::text_muted(),
            format!("IDX  paused · {remaining} remaining"),
        )),
        Some(IndexingEvent::Failed { .. }) => {
            lines.push((Icon::TriangleAlert, theme::danger(), "IDX  failed".into()))
        }
        Some(IndexingEvent::Idle { .. }) | None => {}
    }
    if let Some(hvc_core::embedding_service::ModelPullEvent::Progress {
        completed, total, ..
    }) = model.model_pull_status()
        && *total > 0
    {
        lines.push((
            Icon::Download,
            theme::accent(),
            format!("PULL {}%", completed.saturating_mul(100) / total),
        ));
    }
    lines
}

fn health_card(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let lines = health_lines(model, cx);
    card("rail-card-health", "RUNTIME")
        .cursor_pointer()
        .hover(|style| style.border_color(theme::text_muted()))
        .on_click(cx.listener(|model, _, _, cx| model.select_view(AppView::Settings, cx)))
        .children(lines.into_iter().map(|(glyph, color, text)| {
            div()
                .min_w_0()
                .flex()
                .items_center()
                .gap_2()
                .font_family(theme::FONT_MONO)
                .text_xs()
                .text_color(color)
                .child(icon(glyph, px(12.0), color))
                .child(div().flex_1().min_w_0().text_ellipsis().child(text))
        }))
        .into_any_element()
}

/// Token totals of the active pulsation, or nothing when no summary has been read yet.
fn usage_card(model: &AppModel, cx: &mut Context<AppModel>) -> Option<AnyElement> {
    let totals = model
        .pulsations
        .read(cx)
        .usage
        .summary
        .as_ref()
        .map(|summary| summary.totals.clone())?;
    let input = totals.input_tokens.unwrap_or(0).max(0) as u64;
    let output = totals.output_tokens.unwrap_or(0).max(0) as u64;
    Some(
        card("rail-card-usage", "PULSATION TOKENS")
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .font_family(theme::FONT_MONO)
                    .text_xs()
                    .child(icon(Icon::Zap, px(12.0), theme::text_muted()))
                    .child(format!(
                        "in {} · out {}",
                        crate::shell::compact_count(input),
                        crate::shell::compact_count(output)
                    )),
            )
            .into_any_element(),
    )
}

/// The folded rail: one icon per card, jumping to the view that owns it.
fn strip(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let badges: TabBadges = model.sidebar_badges(cx);
    let waiting = badges.engine_awaiting_hitl
        + model.pulsations.read(cx).awaiting_failure
        + badges.clarifications
        + badges.agent_fallbacks;
    let tone = active_status(model, cx).map(|(_, status)| status.tone.color());
    let runtime_attention = health_lines(model, cx)
        .iter()
        .any(|(_, color, _)| *color == theme::warning() || *color == theme::danger());
    let primary = crate::flow::next_steps(model, cx).into_iter().next();
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap_1()
        .child(
            div()
                .relative()
                .child(icon_button(
                    "rail-strip-pipeline",
                    Icon::Activity,
                    "Pipeline — open Pulsations",
                    false,
                    None,
                    true,
                    cx,
                    |model, _, cx| model.select_view(AppView::Pulsations, cx),
                ))
                .when_some(tone, |slot, tone| {
                    slot.child(
                        div()
                            .absolute()
                            .bottom_1()
                            .right_1()
                            .child(status_dot(tone)),
                    )
                }),
        )
        .child(icon_button(
            "rail-strip-waiting",
            Icon::TriangleAlert,
            if waiting > 0 {
                "Waiting on you — open Pulsations"
            } else {
                "Nothing waiting on you"
            },
            false,
            Some(waiting),
            true,
            cx,
            |model, _, cx| model.select_view(AppView::Pulsations, cx),
        ))
        .when_some(primary, |strip, step| {
            let target = step.target.clone();
            let tooltip =
                crate::widgets::Tooltip::new(step.label.clone()).with_shortcut("ctrl-enter");
            strip.child(icon_button(
                "rail-strip-actions",
                step.icon,
                tooltip,
                false,
                None,
                true,
                cx,
                move |model, window, cx| model.run_next_step(target.clone(), window, cx),
            ))
        })
        .child(
            div()
                .relative()
                .child(icon_button(
                    "rail-strip-health",
                    Icon::Cpu,
                    "Runtime — open Settings",
                    false,
                    None,
                    true,
                    cx,
                    |model, _, cx| model.select_view(AppView::Settings, cx),
                ))
                .when(runtime_attention, |slot| {
                    slot.child(
                        div()
                            .absolute()
                            .bottom_1()
                            .right_1()
                            .child(status_dot(theme::warning())),
                    )
                }),
        )
        .into_any_element()
}
