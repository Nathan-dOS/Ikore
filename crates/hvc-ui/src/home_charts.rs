//! Home's dashboard: four headline numbers and four charts — tokens per day, tokens by model,
//! agent run times and pulsation durations — plus the active pulsation's time per agent in the
//! hero. Bars are plain elements sized in pixels, so a chart is a few dozen elements and needs
//! no canvas; every bar explains itself in a tooltip.

use chrono::{Datelike, Utc};
use gpui::{
    AnyElement, App, Context, FontWeight, IntoElement, Rgba, SharedString, div, prelude::*, px,
    relative, rgb,
};
use hvc_core::persistence_service::PulsationStatus;

use crate::app_model::{AppModel, AppView};
use crate::home_stats::{self, AgentTiming, DayTokens, ModelShare, PulsationTiming, format_secs};
use crate::icons::Icon;
use crate::shell::compact_count;
use crate::theme;
use crate::widgets::{icon, info_tip, status_dot, text_tooltip};

/// Height of the bar area in the two column charts.
const BAR_AREA: f32 = 128.0;
/// Widest a column bar gets, however wide the card is.
const BAR_MAX_WIDTH: f32 = 34.0;

/// The headline numbers (shown right under the next step) and the charts two by two (shown
/// after the waiting/runtime/activity cards; stacked one per row on narrow windows).
pub(crate) fn insights(
    model: &AppModel,
    stacked: bool,
    cx: &mut Context<AppModel>,
) -> (AnyElement, AnyElement) {
    let now = Utc::now();
    let usage = model.home_stats.usage.as_ref();
    let usage_loading = model.home_stats.loading && usage.is_none();
    let days = home_stats::token_days(usage, now.date_naive());
    let models = home_stats::model_shares(usage);
    let (agent_rows, active_running) = {
        let agents = model.agents.read(cx);
        let rows = home_stats::agent_timings(agents.snapshot.as_ref(), now);
        let pulsations = model.pulsations.read(cx);
        let active = pulsations.active().and_then(|active| {
            let running = !matches!(
                active.status,
                PulsationStatus::Done | PulsationStatus::Failed | PulsationStatus::Idle
            );
            let created = home_stats::parse_utc(&active.created_at)?;
            running.then(|| {
                (
                    (now - created).num_seconds().max(0) as u64,
                    format!(
                        "{} · {}",
                        crate::pulsations::status_label(
                            active.status,
                            active.terminal_reason.as_deref()
                        ),
                        crate::pulsations::stage_label(active.stage)
                    ),
                )
            })
        });
        (rows, active)
    };
    let pulse_rows = home_stats::pulsation_timings(&model.pulsations.read(cx).pulsations, now);
    let typical = home_stats::typical_pulsation_secs(&pulse_rows);
    let finished = pulse_rows
        .iter()
        .filter(|row| row.status == PulsationStatus::Done)
        .count();

    let week_tokens: i64 = days.iter().map(|day| day.total).sum();
    let week_input: i64 = days.iter().map(|day| day.input).sum();
    let week_output: i64 = days.iter().map(|day| day.output).sum();
    let runs: usize = agent_rows.iter().map(|row| row.runs).sum();
    let failed: usize = agent_rows.iter().map(|row| row.failed).sum();

    let tiles = div()
        .id("home-kpis")
        .debug_selector(|| "home-kpis".into())
        .w_full()
        .flex()
        .flex_wrap()
        .gap_4()
        .child(kpi(
            "home-kpi-tokens",
            Icon::Zap,
            theme::accent(),
            theme::accent_soft(),
            "Tokens · 7 days",
            if usage_loading {
                "…".to_string()
            } else {
                compact(week_tokens)
            },
            if week_tokens > 0 {
                format!("in {} · out {}", compact(week_input), compact(week_output))
            } else {
                "No usage recorded this week".to_string()
            },
        ))
        .child(kpi(
            "home-kpi-runs",
            Icon::Bot,
            theme::purple(),
            theme::purple_soft(),
            "Agent runs · 7 days",
            runs.to_string(),
            match (runs, failed) {
                (0, _) => "No finished runs this week".to_string(),
                (_, 0) => "All completed".to_string(),
                (_, failed) => format!("{failed} failed or interrupted"),
            },
        ))
        .child(kpi(
            "home-kpi-pulsation",
            Icon::Timer,
            theme::warning(),
            theme::warning_soft(),
            "Typical pulsation",
            typical.map(format_secs).unwrap_or_else(|| "—".into()),
            match finished {
                0 => "No finished pulsation yet".to_string(),
                1 => "The last finished one".to_string(),
                count => format!("Median of the last {count} finished"),
            },
        ))
        .child(kpi(
            "home-kpi-active",
            Icon::Activity,
            theme::running(),
            theme::running_soft(),
            "Active pulsation · open for",
            active_running
                .as_ref()
                .map(|(secs, _)| format_secs(*secs))
                .unwrap_or_else(|| "—".into()),
            active_running
                .map(|(_, state)| state)
                .unwrap_or_else(|| "Nothing running".to_string()),
        ));

    let row = |id: &'static str| {
        div()
            .id(id)
            .debug_selector(move || id.into())
            .w_full()
            .flex()
            // Side by side the two cards stretch to the taller one (the default alignment).
            .when(stacked, |row| row.flex_col())
            .gap_4()
    };
    let charts = div()
        .id("home-insights")
        .debug_selector(|| "home-insights".into())
        .w_full()
        .flex()
        .flex_col()
        .gap_4()
        .child(
            row("home-charts-usage")
                .child(tokens_per_day(&days, now.date_naive(), usage_loading))
                .child(tokens_by_model(&models, usage_loading)),
        )
        .child(
            row("home-charts-timing")
                .child(agent_run_times(&agent_rows))
                .child(pulsation_durations(&pulse_rows, cx)),
        )
        .into_any_element();
    (tiles.into_any_element(), charts)
}

/// The hero's time-per-agent bar for the active pulsation, once an agent has run for it.
pub(crate) fn active_timeline(model: &AppModel, cx: &App) -> Option<AnyElement> {
    let now = Utc::now();
    let pulsations = model.pulsations.read(cx);
    let active = pulsations.active()?;
    let stages = home_stats::stage_times(model.agents.read(cx).snapshot.as_ref(), &active.id, now);
    let total: u64 = stages.iter().map(|(_, secs)| secs).sum();
    if stages.is_empty() || total == 0 {
        return None;
    }
    let elapsed = home_stats::parse_utc(&active.created_at)
        .map(|created| (now - created).num_seconds().max(0) as u64);
    Some(
        div()
            .id("home-active-timeline")
            .debug_selector(|| "home-active-timeline".into())
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child("TIME PER AGENT")
                            .child(info_tip(
                                "home-active-timeline",
                                "Wall-clock time of each agent's runs for this pulsation; a run still going counts until now.",
                            )),
                    )
                    .when_some(elapsed, |line, elapsed| {
                        line.child(
                            div()
                                .font_family(theme::FONT_MONO)
                                .child(format!("{} since it started", format_secs(elapsed))),
                        )
                    }),
            )
            .child(
                div()
                    .w_full()
                    .h(px(10.0))
                    .rounded_full()
                    .overflow_hidden()
                    .flex()
                    .gap(px(2.0))
                    .children(stages.iter().enumerate().map(|(index, (role, secs))| {
                        let name = hvc_core::terminals::canonical_agent(*role).name;
                        div()
                            .id(SharedString::from(format!("home-active-stage-{index}")))
                            .h_full()
                            .w(relative(*secs as f32 / total as f32))
                            .min_w(px(3.0))
                            .bg(theme::agent_color(*role))
                            .tooltip(text_tooltip(format!("{name} · {}", format_secs(*secs))))
                    })),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_x_4()
                    .gap_y_1()
                    .children(stages.iter().map(|(role, secs)| {
                        legend_item(
                            theme::agent_color(*role),
                            format!(
                                "{} {}",
                                hvc_core::terminals::canonical_agent(*role).name,
                                format_secs(*secs)
                            ),
                        )
                    })),
            )
            .into_any_element(),
    )
}

/// A headline number: tinted glyph, label, value and one line of context.
fn kpi(
    id: &'static str,
    glyph: Icon,
    ink: Rgba,
    tint: Rgba,
    label: &'static str,
    value: String,
    detail: String,
) -> AnyElement {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .flex_1()
        .min_w(px(200.0))
        .p_4()
        .rounded_lg()
        .bg(theme::surface_raised())
        .border_1()
        .border_color(theme::border())
        .flex()
        .items_start()
        .gap_3()
        .child(
            div()
                .flex_shrink_0()
                .size(px(36.0))
                .rounded_lg()
                .bg(tint)
                .flex()
                .items_center()
                .justify_center()
                .child(icon(glyph, px(18.0), ink)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(div().text_xs().text_color(theme::text_muted()).child(label))
                .child(
                    div()
                        .text_2xl()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::text())
                        .child(value),
                )
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_xs()
                        .text_color(ink)
                        .child(detail),
                ),
        )
        .into_any_element()
}

/// A chart's frame: title with its "i", an optional right-hand note, then the body.
fn chart_card(
    id: &'static str,
    title: &'static str,
    info: &'static str,
    note: Option<AnyElement>,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .flex_1()
        .min_w_0()
        .p_4()
        .rounded_lg()
        .bg(theme::surface_raised())
        .border_1()
        .border_color(theme::border())
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(title),
                        )
                        .child(info_tip(id, info)),
                )
                .when_some(note, |header, note| header.child(note)),
        )
}

fn tokens_per_day(days: &[DayTokens], today: chrono::NaiveDate, loading: bool) -> AnyElement {
    let peak = days.iter().map(|day| day.total).max().unwrap_or(0);
    let card = chart_card(
        "home-chart-tokens",
        "Tokens per day",
        "The last seven UTC days of this project's token ledger, input and output stacked. Cache counts as input and reasoning as output, as on the Usage page.",
        Some(
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(legend_item(theme::accent(), "Input"))
                .child(legend_item(theme::purple(), "Output"))
                .into_any_element(),
        ),
    );
    if peak == 0 {
        return card
            .child(empty_chart(if loading {
                "Loading the week's usage…"
            } else {
                "No token usage in the last 7 days."
            }))
            .into_any_element();
    }
    card.child(scale_caption(format!("{} peak", compact(peak))))
        .child(
            div().relative().w_full().child(gridlines()).child(
                div()
                    .w_full()
                    .flex()
                    .items_end()
                    .gap_2()
                    .children(days.iter().enumerate().map(|(index, day)| {
                        let height = BAR_AREA * day.total as f32 / peak as f32;
                        let bar = if day.total > 0 { height.max(3.0) } else { 0.0 };
                        let output = if day.total > 0 {
                            bar * day.output as f32 / day.total as f32
                        } else {
                            0.0
                        };
                        let is_today = day.date == today;
                        let tooltip = format!(
                            "{} · {} tokens (input {} · output {}){}",
                            day.date.format("%a %d %b"),
                            compact(day.total),
                            compact(day.input),
                            compact(day.output),
                            if day.incomplete {
                                " · some records partial"
                            } else {
                                ""
                            }
                        );
                        div()
                            .id(SharedString::from(format!("home-day-{index}")))
                            .debug_selector(move || format!("home-day-{index}"))
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap_1()
                            .tooltip(text_tooltip(tooltip))
                            .child(
                                div()
                                    .w_full()
                                    .h(px(BAR_AREA))
                                    .flex()
                                    .flex_col()
                                    .justify_end()
                                    .items_center()
                                    .child(
                                        div()
                                            .w_full()
                                            .max_w(px(BAR_MAX_WIDTH))
                                            .h(px(bar))
                                            .rounded_t_md()
                                            .overflow_hidden()
                                            .flex()
                                            .flex_col()
                                            .opacity(if day.incomplete { 0.6 } else { 1.0 })
                                            .child(div().w_full().h(px(output)).bg(theme::purple()))
                                            .child(div().w_full().flex_1().bg(theme::accent())),
                                    ),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .when(is_today, |label| {
                                        label
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(theme::text())
                                    })
                                    .when(!is_today, |label| label.text_color(theme::text_muted()))
                                    .child(if is_today {
                                        "Today".to_string()
                                    } else {
                                        weekday(day.date)
                                    }),
                            )
                    })),
            ),
        )
        .into_any_element()
}

fn tokens_by_model(models: &[ModelShare], loading: bool) -> AnyElement {
    let total: i64 = models.iter().map(|share| share.tokens).sum();
    let card = chart_card(
        "home-chart-models",
        "Tokens by model",
        "Each model's share of the week's tokens; the same model over API and CLI counts once. Models past the fifth are grouped as Other.",
        (total > 0).then(|| {
            div()
                .font_family(theme::FONT_MONO)
                .text_xs()
                .text_color(theme::text_muted())
                .child(format!("{} total", compact(total)))
                .into_any_element()
        }),
    );
    if total == 0 {
        return card
            .child(empty_chart(if loading {
                "Loading the week's usage…"
            } else {
                "No model has been used this week."
            }))
            .into_any_element();
    }
    let color = |index: usize, share: &ModelShare| {
        if share.other {
            theme::text_subtle()
        } else {
            MODEL_PALETTE[index % MODEL_PALETTE.len()]()
        }
    };
    card.child(
        div()
            .w_full()
            .h(px(12.0))
            .rounded_full()
            .overflow_hidden()
            .flex()
            .gap(px(2.0))
            .children(models.iter().enumerate().map(|(index, share)| {
                div()
                    .id(SharedString::from(format!("home-model-{index}")))
                    .h_full()
                    .w(relative(share.tokens as f32 / total as f32))
                    .min_w(px(3.0))
                    .bg(color(index, share))
                    .tooltip(text_tooltip(format!(
                        "{} · {} tokens",
                        share.label,
                        compact(share.tokens)
                    )))
            })),
    )
    .child(
        div()
            .flex()
            .flex_col()
            .gap_2()
            .children(models.iter().enumerate().map(|(index, share)| {
                let percent = share.tokens as f64 * 100.0 / total as f64;
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .child(status_dot(color(index, share)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(share.label.clone()),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .font_family(theme::FONT_MONO)
                            .text_xs()
                            .text_color(theme::text_muted())
                            .child(compact(share.tokens)),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .w(px(44.0))
                            .text_right()
                            .font_family(theme::FONT_MONO)
                            .text_xs()
                            .child(if percent >= 10.0 {
                                format!("{percent:.0}%")
                            } else {
                                format!("{percent:.1}%")
                            }),
                    )
            })),
    )
    .into_any_element()
}

fn agent_run_times(rows: &[AgentTiming]) -> AnyElement {
    let card = chart_card(
        "home-chart-agents",
        "Agent run time",
        "Finished runs of each agent that started in the last seven days (up to its last 20). The solid bar is the median run, the faint one the longest.",
        (!rows.is_empty()).then(|| {
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(legend_item(theme::text(), "Median"))
                .child(legend_item(theme::text_subtle(), "Longest"))
                .into_any_element()
        }),
    );
    let longest = rows.iter().map(|row| row.longest_secs).max().unwrap_or(0);
    if rows.is_empty() || longest == 0 {
        return card
            .child(empty_chart("No agent has finished a run this week."))
            .into_any_element();
    }
    card.child(
        div()
            .flex()
            .flex_col()
            .gap_3()
            .children(rows.iter().enumerate().map(|(index, row)| {
                let ink = theme::agent_color(row.role);
                let tooltip = format!(
                    "{} · {} run{} in 7 days · median {} · longest {}{}",
                    row.name,
                    row.runs,
                    if row.runs == 1 { "" } else { "s" },
                    format_secs(row.median_secs),
                    format_secs(row.longest_secs),
                    if row.failed > 0 {
                        format!(" · {} failed or interrupted", row.failed)
                    } else {
                        String::new()
                    }
                );
                div()
                    .id(SharedString::from(format!("home-agent-{index}")))
                    .debug_selector(move || format!("home-agent-{index}"))
                    .flex()
                    .items_center()
                    .gap_3()
                    .tooltip(text_tooltip(tooltip))
                    .child(
                        div()
                            .w(px(132.0))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(status_dot(ink))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .text_sm()
                                    .child(row.name),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h(px(10.0))
                            .relative()
                            .rounded_full()
                            .bg(theme::canvas())
                            .child(
                                div()
                                    .absolute()
                                    .left_0()
                                    .top_0()
                                    .bottom_0()
                                    .w(relative(row.longest_secs as f32 / longest as f32))
                                    .rounded_full()
                                    .bg(theme::agent_soft_color(row.role)),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .left_0()
                                    .top_0()
                                    .bottom_0()
                                    .w(relative(row.median_secs as f32 / longest as f32))
                                    .min_w(px(4.0))
                                    .rounded_full()
                                    .bg(ink),
                            ),
                    )
                    .child(
                        div()
                            .w(px(112.0))
                            .flex_shrink_0()
                            .flex()
                            .justify_end()
                            .items_center()
                            .gap_2()
                            .font_family(theme::FONT_MONO)
                            .text_xs()
                            .child(format_secs(row.median_secs))
                            .child(
                                div()
                                    .text_color(if row.failed > 0 {
                                        theme::danger()
                                    } else {
                                        theme::text_muted()
                                    })
                                    .child(format!("{}×", row.runs)),
                            ),
                    )
            })),
    )
    .into_any_element()
}

fn pulsation_durations(rows: &[PulsationTiming], cx: &mut Context<AppModel>) -> AnyElement {
    let card = chart_card(
        "home-chart-pulsations",
        "Pulsation duration",
        "Wall-clock time of the last eight pulsations, from creation to their last update once finished; a pulsation still running counts until now. Click a bar to open it.",
        (!rows.is_empty()).then(|| {
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(legend_item(theme::success(), "Done"))
                .child(legend_item(theme::danger(), "Failed"))
                .child(legend_item(theme::running(), "Running"))
                .into_any_element()
        }),
    );
    // Finished pulsations set the scale: one left open for a day would flatten every other
    // bar. An open one beyond the scale is drawn at full height, faded, and says "so far".
    let finished_peak = rows
        .iter()
        .filter(|row| row.finished)
        .map(|row| row.secs)
        .max()
        .unwrap_or(0);
    let peak = if finished_peak > 0 {
        finished_peak
    } else {
        rows.iter().map(|row| row.secs).max().unwrap_or(0)
    };
    if rows.is_empty() || peak == 0 {
        return card
            .child(empty_chart("No pulsation has run yet."))
            .into_any_element();
    }
    card.child(scale_caption(format!(
        "{} longest{}",
        format_secs(peak),
        if finished_peak > 0 { " finished" } else { "" }
    )))
    .child(
        div().relative().w_full().child(gridlines()).child(
            div()
                .w_full()
                .flex()
                .items_end()
                .gap_2()
                .children(rows.iter().enumerate().map(|(index, row)| {
                    let bar = (BAR_AREA * (row.secs.min(peak)) as f32 / peak as f32).max(3.0);
                    let ink = match row.status {
                        PulsationStatus::Done => theme::success(),
                        PulsationStatus::Failed => theme::danger(),
                        _ => theme::running(),
                    };
                    let name = if row.name.is_empty() {
                        "Untitled pulsation".to_string()
                    } else {
                        row.name.clone()
                    };
                    let tooltip = format!(
                        "{name} · {} · {}{}",
                        crate::pulsations::status_label(row.status, None),
                        format_secs(row.secs),
                        if row.finished { "" } else { " so far" }
                    );
                    let id = row.id.clone();
                    div()
                        .id(SharedString::from(format!("home-pulsation-{index}")))
                        .debug_selector(move || format!("home-pulsation-{index}"))
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_1()
                        .cursor_pointer()
                        .tooltip(text_tooltip(tooltip))
                        .on_click(cx.listener(move |model, _, _, cx| {
                            model.select_pulsation(id.clone(), cx);
                            model.select_view(AppView::Pulsations, cx);
                        }))
                        .child(
                            div()
                                .w_full()
                                .h(px(BAR_AREA))
                                .flex()
                                .flex_col()
                                .justify_end()
                                .items_center()
                                .child(
                                    div()
                                        .w_full()
                                        .max_w(px(BAR_MAX_WIDTH))
                                        .h(px(bar))
                                        .rounded_t_md()
                                        .bg(ink)
                                        .opacity(if row.finished { 1.0 } else { 0.65 })
                                        .hover(|style| style.opacity(0.8)),
                                ),
                        )
                        .child(
                            div()
                                .w_full()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_center()
                                .text_xs()
                                .text_color(theme::text_muted())
                                .child(name),
                        )
                })),
        ),
    )
    .into_any_element()
}

/// The value of a column chart's top gridline, on its own line above the bars so a tall bar
/// never covers it.
fn scale_caption(label: String) -> AnyElement {
    div()
        .flex()
        .justify_end()
        .font_family(theme::FONT_MONO)
        .text_xs()
        .text_color(theme::text_subtle())
        .child(label)
        .into_any_element()
}

/// Two faint horizontal lines behind a column chart: its peak and half of it.
fn gridlines() -> AnyElement {
    div()
        .absolute()
        .left_0()
        .right_0()
        .top_0()
        .h(px(BAR_AREA))
        .child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .top_0()
                .h(px(1.0))
                .bg(theme::border()),
        )
        .child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .top(px(BAR_AREA / 2.0))
                .h(px(1.0))
                .bg(theme::border()),
        )
        .into_any_element()
}

fn legend_item(color: Rgba, label: impl Into<SharedString>) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap_1p5()
        .text_xs()
        .text_color(theme::text_muted())
        .child(status_dot(color))
        .child(label.into())
        .into_any_element()
}

fn empty_chart(message: &'static str) -> AnyElement {
    div()
        .h(px(BAR_AREA))
        .flex()
        .items_center()
        .justify_center()
        .text_sm()
        .text_color(theme::text_muted())
        .child(message)
        .into_any_element()
}

fn weekday(date: chrono::NaiveDate) -> String {
    format!("{} {}", date.format("%a"), date.day())
}

fn compact(value: i64) -> String {
    compact_count(value.max(0) as u64)
}

/// Model colours in share order; "Other" is always the quiet grey.
const MODEL_PALETTE: [fn() -> Rgba; 6] = [
    theme::accent,
    theme::purple,
    theme::warning,
    theme::success,
    pink,
    theme::running,
];

fn pink() -> Rgba {
    rgb(0xf08bd5)
}
