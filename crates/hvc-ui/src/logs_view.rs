use gpui::{AnyElement, Context, IntoElement, SharedString, div, prelude::*, px};

use crate::app_model::AppModel;
use crate::sizing::StyledExt;
use crate::theme;

pub(crate) fn logs_panel(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let (
        snapshot,
        selected_run_id,
        selected_run,
        events,
        visible_output,
        output_complete,
        loading,
        poll_in_flight,
        grouped_runs,
    ) = {
        let logs = model.logs.read(cx);
        (
            logs.snapshot.clone(),
            logs.selected_run_id.clone(),
            logs.selected_run().cloned(),
            logs.events.clone(),
            logs.visible_output.clone(),
            logs.visible_output_complete,
            logs.loading,
            logs.poll_in_flight,
            logs.grouped_runs(),
        )
    };
    let logs = model.logs.read(cx);
    let collapsed_routing = logs.collapsed_routing;
    let collapsed_events = logs.collapsed_events;
    let visible_events = logs.visible_events().cloned().collect::<Vec<_>>();
    let has_more_events = visible_events.len() < events.len();
    let run_count = snapshot.runs.len();
    let metric_count = snapshot.metrics.len();

    div()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .p_3()
        .flex()
        .gap_3()
        .child(
            div()
                .w(px(360.0))
                .flex_shrink_0()
                .min_h_0()
                .p_3()
                .rounded_lg()
                .border_1()
                .border_color(theme::border())
                .bg(theme::surface())
                .flex()
                .flex_col()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme::accent())
                                        .child("AGENT RUNTIME"),
                                )
                                .child(
                                    div()
                                        .text_lg()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child("Observability Logs"),
                                ),
                        )
                        .child(button(
                            "logs-refresh",
                            if poll_in_flight { "…" } else { "Refresh" },
                            !poll_in_flight,
                            cx,
                            |model, cx| model.refresh_logs(cx),
                        )),
                )
                .when(loading, |panel| {
                    panel.child(
                        div()
                            .text_sm()
                            .text_color(theme::text_muted())
                            .child("Loading observability…"),
                    )
                })
                .when(!loading && snapshot.runs.is_empty(), |panel| {
                    panel.child(
                        div()
                            .p_3()
                            .text_sm()
                            .text_color(theme::text_muted())
                            .child("No execution runs registered for this project yet."),
                    )
                })
                .child(
                    div()
                        .id("logs-runs-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .children(grouped_runs.into_iter().map(|group| {
                            let key = group.key.clone();
                            let is_expanded = group.is_expanded;
                            let pulsation_id = group.pulsation_id.clone();
                            let runs_len = group.runs.len();

                            div()
                                .flex_shrink_0()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .id(SharedString::from(format!("pulse-group:{key}")))
                                        .flex_shrink_0()
                                        .px(px(10.0))
                                        .py(px(8.0))
                                        .rounded_md()
                                        .bg(theme::surface_raised())
                                        .border_1()
                                        .border_color(theme::border())
                                        .cursor_pointer()
                                        .on_click(cx.listener({
                                            let key = key.clone();
                                            move |model, _, _, cx| {
                                                model.toggle_log_pulsation_group(key.clone(), cx);
                                            }
                                        }))
                                        .flex()
                                        .flex_col()
                                        .gap(px(5.0))
                                        .child(
                                            div()
                                                .flex()
                                                .items_start()
                                                .gap_2()
                                                .child(
                                                    div()
                                                        .flex_shrink_0()
                                                        .mt(px(2.0))
                                                        .text_xs()
                                                        .text_color(theme::accent())
                                                        .child(if is_expanded { "▼" } else { "▶" }),
                                                )
                                                .when_some(group.order_label, |row, order| {
                                                    row.child(
                                                        div()
                                                            .flex_shrink_0()
                                                            .px(px(6.0))
                                                            .py(px(1.5))
                                                            .rounded_sm()
                                                            .bg(theme::accent_soft())
                                                            .text_color(theme::accent())
                                                            .text_xs()
                                                            .font_weight(gpui::FontWeight::BOLD)
                                                            .child(order),
                                                    )
                                                })
                                                .child(
                                                    div()
                                                        .flex_1()
                                                        .min_w_0()
                                                        .text_sm()
                                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                                        .text_color(theme::text())
                                                        .child(group.title),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .justify_between()
                                                .gap_2()
                                                .child(
                                                    div()
                                                        .flex()
                                                        .items_center()
                                                        .gap_1_5()
                                                        .text_xs()
                                                        .text_color(theme::text_muted())
                                                        .child(format!(
                                                            "{runs_len} run{}",
                                                            if runs_len == 1 { "" } else { "s" }
                                                        ))
                                                        .when_some(pulsation_id, |row, pid| {
                                                            row.child(
                                                                div()
                                                                    .text_color(theme::text_muted())
                                                                    .child("·"),
                                                            )
                                                            .child(
                                                                div()
                                                                    .id(SharedString::from(format!("jump-pulse:{pid}")))
                                                                    .text_color(theme::accent())
                                                                    .cursor_pointer()
                                                                    .hover(|s| s.underline())
                                                                    .on_click(cx.listener({
                                                                        let pid = pid.clone();
                                                                        move |model, _, _, cx| {
                                                                            model.jump_to_pulsation_from_logs(pid.clone(), cx);
                                                                        }
                                                                    }))
                                                                    .child("↗ Pulsations"),
                                                            )
                                                        }),
                                                )
                                                .when_some(group.status, |row, status| {
                                                    let is_done = status.eq_ignore_ascii_case("done")
                                                        || status.eq_ignore_ascii_case("completed");
                                                    let is_fail = status.eq_ignore_ascii_case("failed");
                                                    let (status_bg, status_fg) = if is_done {
                                                        (theme::success_soft(), theme::success())
                                                    } else if is_fail {
                                                        (theme::danger_soft(), theme::danger())
                                                    } else {
                                                        (theme::surface(), theme::text_muted())
                                                    };
                                                    row.child(
                                                        div()
                                                            .flex_shrink_0()
                                                            .px(px(6.0))
                                                            .py(px(1.5))
                                                            .rounded_sm()
                                                            .text_xs()
                                                            .font_weight(gpui::FontWeight::BOLD)
                                                            .bg(status_bg)
                                                            .text_color(status_fg)
                                                            .child(status.to_uppercase()),
                                                    )
                                                }),
                                        ),
                                )
                                .when(is_expanded, |section| {
                                    section.child(
                                        div()
                                            .flex_shrink_0()
                                            .pl_2()
                                            .flex()
                                            .flex_col()
                                            .gap(px(3.0))
                                            .children(group.runs.into_iter().enumerate().map(|(idx, run)| {
                                                let id = run.id.clone();
                                                let selected = selected_run_id
                                                    .as_ref()
                                                    .is_some_and(|sel| sel.as_ref() == id);
                                                let has_error = run.last_error.is_some()
                                                    || run.validation_error.is_some();
                                                let model_name = run.model.clone().unwrap_or_else(|| "—".into());
                                                let duration_str = run
                                                    .duration_ms
                                                    .map(format_duration_ms)
                                                    .unwrap_or_else(|| "—".into());

                                                div()
                                                    .id(SharedString::from(format!("log-run:{id}")))
                                                    .flex_shrink_0()
                                                    .px(px(8.0))
                                                    .py(px(5.0))
                                                    .rounded_md()
                                                    .border_1()
                                                    .border_color(if selected {
                                                        theme::accent()
                                                    } else {
                                                        theme::border()
                                                    })
                                                    .bg(if selected {
                                                        theme::accent_soft()
                                                    } else {
                                                        theme::surface()
                                                    })
                                                    .cursor_pointer()
                                                    .on_click(cx.listener(move |model, _, _, cx| {
                                                        model.select_log_run(id.clone(), cx)
                                                    }))
                                                    .flex()
                                                    .flex_col()
                                                    .gap(px(2.0))
                                                    .child(
                                                        div()
                                                            .flex()
                                                            .items_center()
                                                            .justify_between()
                                                            .gap_1()
                                                            .child(
                                                                div()
                                                                    .flex_1()
                                                                    .min_w_0()
                                                                    .flex()
                                                                    .items_center()
                                                                    .gap_1_5()
                                                                    .child(
                                                                        div()
                                                                            .flex_shrink_0()
                                                                            .text_xs()
                                                                            .text_color(theme::text_muted())
                                                                            .child(format!("{}.", idx + 1)),
                                                                    )
                                                                    .child(
                                                                        div()
                                                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                                                            .text_sm()
                                                                            .child(run.agent_name),
                                                                    ),
                                                            )
                                                            .child(status_badge(&run.status, has_error)),
                                                    )
                                                    .child(
                                                        div()
                                                            .flex()
                                                            .items_center()
                                                            .justify_between()
                                                            .gap_1()
                                                            .text_xs()
                                                            .text_color(theme::text_muted())
                                                            .child(
                                                                div()
                                                                    .min_w_0()
                                                                    .truncate()
                                                                    .child(model_name),
                                                            )
                                                            .child(
                                                                div()
                                                                    .flex_shrink_0()
                                                                    .child(duration_str),
                                                            ),
                                                    )
                                            })),
                                    )
                                })
                        })),
                ),
        )
        .child(
            div()
                .id("logs-detail-scroll")
                .debug_selector(|| "logs-detail-scroll".into())
                .flex_1()
                .min_w_0()
                .min_h_0()
                .p_4()
                .rounded_lg()
                .border_1()
                .border_color(theme::border())
                .bg(theme::surface())
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_4()
                .child(
                    div()
                        .text_xl()
                        .font_weight(gpui::FontWeight::BOLD)
                        .child("Logs & Observability"),
                )
                .child(
                    div()
                        .grid()
                        .grid_cols(3)
                        .gap_2()
                        .child(metric("RUNS", run_count.to_string()))
                        .child(metric("METRICS", metric_count.to_string()))
                        .child(metric("EVENTS", events.len().to_string())),
                )
                .when_some(selected_run, |panel, run| {
                    let has_error = run.last_error.is_some() || run.validation_error.is_some();
                    let input_tok = run.input_tokens.unwrap_or(0);
                    let output_tok = run.output_tokens.unwrap_or(0);
                    let total_tok = input_tok + output_tok;
                    let speed = format_throughput(run.output_tokens, run.duration_ms);

                    panel
                        // Context & Provenance Card
                        .child(
                            div()
                                .p_3()
                                .rounded_lg()
                                .border_1()
                                .border_color(theme::border())
                                .bg(theme::surface_raised())
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap_2()
                                                .child(
                                                    div()
                                                        .text_lg()
                                                        .font_weight(gpui::FontWeight::BOLD)
                                                        .child(run.agent_name.clone()),
                                                )
                                                .child(status_badge(&run.status, has_error)),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(theme::text_muted())
                                                .child(format!(
                                                    "{} / {}",
                                                    run.provider.as_deref().unwrap_or("—"),
                                                    run.model.as_deref().unwrap_or("—")
                                                )),
                                        ),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .flex_wrap()
                                        .gap_x_4()
                                        .gap_y_1()
                                        .text_xs()
                                        .text_color(theme::text_muted())
                                        .when_some(run.pulsation_name.clone(), |row, pname| {
                                            row.child(format!("Pulsation: {pname}"))
                                        })
                                        .when_some(run.job_kind.clone(), |row, kind| {
                                            row.child(format!(
                                                "Job: {kind} ({})",
                                                &run.job_id[..run.job_id.len().min(8)]
                                            ))
                                        })
                                        .when_some(run.pulsation_status.clone(), |row, pstatus| {
                                            row.child(format!("Pulsation Status: {pstatus}"))
                                        }),
                                 ),
                        )
                        // Failure Diagnostic Banner
                        .when(has_error || run.cancellation_reason.is_some(), |panel| {
                            panel.child(
                                div()
                                    .p_3()
                                    .rounded_lg()
                                    .border_1()
                                    .border_color(theme::danger())
                                    .bg(theme::danger_soft())
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .child(
                                        div()
                                            .font_weight(gpui::FontWeight::BOLD)
                                            .text_color(theme::danger())
                                            .text_xs()
                                            .child("FAILURE DIAGNOSTIC"),
                                    )
                                    .when_some(run.last_error.clone(), |diag, err| {
                                        diag.child(
                                            div()
                                                .font_family(theme::FONT_MONO)
                                                .text_xs()
                                                .text_color(theme::text())
                                                .child(err),
                                        )
                                    })
                                    .when_some(run.validation_error.clone(), |diag, val_err| {
                                        diag.child(
                                            div()
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(theme::warning())
                                                        .child("Schema Validation Error:"),
                                                )
                                                .child(
                                                    div()
                                                        .font_family(theme::FONT_MONO)
                                                        .text_xs()
                                                        .text_color(theme::text())
                                                        .child(val_err),
                                                ),
                                        )
                                    })
                                    .when_some(run.cancellation_reason.clone(), |diag, reason| {
                                        diag.child(
                                            div()
                                                .text_xs()
                                                .text_color(theme::warning())
                                                .child(format!("Cancellation reason: {reason}")),
                                        )
                                    }),
                            )
                        })
                        // Quick Metrics
                        .child(
                            div()
                                .grid()
                                .grid_cols(4)
                                .gap_2()
                                .child(metric(
                                    "DURATION",
                                    run.duration_ms
                                        .map(format_duration_ms)
                                        .unwrap_or_else(|| "—".into()),
                                ))
                                .child(metric(
                                    "TOKENS",
                                    format!("{total_tok} ({input_tok} in / {output_tok} out)"),
                                ))
                                .child(metric("SPEED", speed.unwrap_or_else(|| "—".into())))
                                .child(metric(
                                    "ESTIMATED COST",
                                    run.estimated_cost_usd
                                        .map_or_else(|| "—".into(), |v| format!("US$ {v:.6}")),
                                ))
                        )
                        // Timing & Lifecycle
                        .child(
                            section("Timing & Lifecycle")
                                .child(fact(
                                    "TIMING",
                                    crate::time::format_time_range(
                                        run.started_at.as_deref().or(Some(&run.created_at)),
                                        run.finished_at.as_deref(),
                                        "—",
                                    ),
                                ))
                                .child(fact("CREATED AT", run.created_at.clone()))
                                .when_some(run.finished_at.clone(), |sec, finished| {
                                    sec.child(fact("FINISHED AT", finished))
                                }),
                        )
                        // Tool Trace
                        .when_some(run.tool_trace.clone(), |panel, trace| {
                            panel.child(
                                section("Tool Trace")
                                    .child(
                                        div()
                                            .flex()
                                            .justify_end()
                                            .child(copy_button(
                                                "copy-tool-trace".into(),
                                                trace.clone(),
                                                "Copy Trace",
                                                cx,
                                            )),
                                    )
                                    .child(
                                        div()
                                            .mt_2()
                                            .p_3()
                                            .rounded_md()
                                            .bg(theme::surface())
                                            .font_family(theme::FONT_MONO)
                                            .text_xs()
                                            .child(trace),
                                    ),
                            )
                        })
                        // Structured Output
                        .when_some(run.structured_output.clone(), |panel, structured| {
                            let pretty_json = serde_json::to_string_pretty(&structured)
                                .unwrap_or_else(|_| structured.to_string());
                            panel.child(
                                section("Structured Output")
                                    .child(
                                        div()
                                            .flex()
                                            .justify_end()
                                            .child(copy_button(
                                                "copy-structured-out".into(),
                                                pretty_json.clone(),
                                                "Copy JSON",
                                                cx,
                                            )),
                                    )
                                    .child(
                                        div()
                                            .mt_2()
                                            .p_3()
                                            .rounded_md()
                                            .bg(theme::surface())
                                            .font_family(theme::FONT_MONO)
                                            .text_xs()
                                            .child(pretty_json),
                                    ),
                            )
                        })
                        // Raw Output
                        .when_some(run.raw_output.clone(), |panel, raw| {
                            panel.child(
                                section("Raw Output")
                                    .child(
                                        div()
                                            .flex()
                                            .justify_end()
                                            .child(copy_button(
                                                "copy-raw-out".into(),
                                                raw.clone(),
                                                "Copy Output",
                                                cx,
                                            )),
                                    )
                                    .child(
                                        div()
                                            .mt_2()
                                            .p_3()
                                            .rounded_md()
                                            .bg(theme::surface())
                                            .font_family(theme::FONT_MONO)
                                            .text_xs()
                                            .child(raw),
                                    ),
                            )
                        })
                })
                .child(button(
                    "logs-toggle-routing",
                    if collapsed_routing {
                        "Show routing metrics"
                    } else {
                        "Hide routing metrics"
                    },
                    true,
                    cx,
                    |model, cx| {
                        model.logs.update(cx, |logs, _| {
                            logs.collapsed_routing = !logs.collapsed_routing
                        });
                        cx.notify();
                    },
                ))
                .when(!collapsed_routing, |panel| {
                    panel.child(section("Routing metrics").children(
                        snapshot.metrics.into_iter().map(|metric_item| {
                            div()
                                .mt_2()
                                .p_2()
                                .rounded_md()
                                .bg(theme::surface())
                                .child(format!(
                                    "{}/{} · {} attempt(s), {} success(es)",
                                    metric_item.provider_id,
                                    metric_item.model_id,
                                    metric_item.attempts,
                                    metric_item.successes
                                ))
                                .child(div().text_xs().text_color(theme::text_muted()).child(
                                    format!(
                                        "average latency {} · tokens {}/{} · cost {}",
                                        metric_item
                                            .average_latency_ms
                                            .map(|value| format!("{value:.1} ms"))
                                            .unwrap_or_else(|| "—".into()),
                                        metric_item.input_tokens,
                                        metric_item.output_tokens,
                                        metric_item
                                            .estimated_cost_usd
                                            .map(|value| format!("US$ {value:.6}"))
                                            .unwrap_or_else(|| "—".into())
                                    ),
                                ))
                        }),
                    ))
                })
                .child(
                    section("Visible LLM output")
                        .child(
                            div()
                                .text_xs()
                                .text_color(if output_complete {
                                    theme::accent()
                                } else {
                                    theme::warning()
                                })
                                .child(if output_complete {
                                    "complete"
                                } else {
                                    "incomplete / streaming"
                                }),
                        )
                        .child(
                            div()
                                .mt_2()
                                .p_3()
                                .rounded_md()
                                .bg(theme::surface())
                                .font_family(theme::FONT_MONO)
                                .text_sm()
                                .child(if visible_output.is_empty() {
                                    SharedString::from("No visible output in this session.")
                                } else {
                                    visible_output
                                }),
                        ),
                )
                .child(button(
                    "logs-toggle-events",
                    if collapsed_events {
                        "Show events"
                    } else {
                        "Hide events"
                    },
                    true,
                    cx,
                    |model, cx| {
                        model
                            .logs
                            .update(cx, |logs, _| logs.collapsed_events = !logs.collapsed_events);
                        cx.notify();
                    },
                ))
                .when(!collapsed_events, |panel| {
                    panel.child(
                        section("Watcher / LLM events").child(
                            div()
                                .id("logs-events-scroll")
                                .flex()
                                .flex_col()
                                .gap_1()
                                .children(visible_events.into_iter().map(|event| {
                                    div()
                                        .p_2()
                                        .rounded_md()
                                        .bg(theme::surface())
                                        .child(
                                            div()
                                                .flex()
                                                .justify_between()
                                                .child(format!(
                                                    "{} · {}",
                                                    event.category, event.name
                                                ))
                                                .child(
                                                    div()
                                                        .flex()
                                                        .items_center()
                                                        .gap_2()
                                                        .when(!event.timestamp.is_empty(), |row| {
                                                            row.child(
                                                                div()
                                                                    .text_xs()
                                                                    .text_color(theme::text_muted())
                                                                    .child(event.timestamp.clone()),
                                                            )
                                                        })
                                                        .when_some(
                                                            event.sequence,
                                                            |row, sequence| {
                                                                row.child(
                                                                    div()
                                                                        .text_xs()
                                                                        .text_color(
                                                                            theme::text_muted(),
                                                                        )
                                                                        .child(format!(
                                                                            "#{sequence}"
                                                                        )),
                                                                )
                                                            },
                                                        ),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(theme::text_muted())
                                                .child(event.summary),
                                        )
                                })),
                        ),
                    )
                })
                .when(!collapsed_events && has_more_events, |panel| {
                    panel.child(button(
                        "logs-more-events",
                        "Show 50 more events",
                        true,
                        cx,
                        |model, cx| {
                            model.logs.update(cx, |logs, _| logs.show_more_events());
                            cx.notify();
                        },
                    ))
                }),
        )
        .into_any_element()
}

fn button(
    id: &'static str,
    label: &'static str,
    enabled: bool,
    cx: &mut Context<AppModel>,
    handler: impl Fn(&mut AppModel, &mut Context<AppModel>) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .self_start()
        .flex_shrink_0()
        .px_3()
        .py_2()
        .rounded_md()
        .bg(if enabled {
            theme::accent()
        } else {
            theme::surface_raised()
        })
        .text_color(if enabled {
            theme::canvas()
        } else {
            theme::text_muted()
        })
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .on_click(cx.listener(move |model, _, _, cx| handler(model, cx)))
        })
        .child(label)
}

fn section(title: &'static str) -> gpui::Div {
    div()
        .p_3()
        .rounded_lg()
        .border_1()
        .border_color(theme::border())
        .bg(theme::surface_raised())
        .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(title))
}

fn metric(label: &'static str, value: String) -> impl IntoElement {
    div()
        .p_3()
        .rounded_md()
        .bg(theme::surface_raised())
        .child(div().text_xs().text_color(theme::text_muted()).child(label))
        .child(
            div()
                .text_lg()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(value),
        )
}

fn fact(label: &'static str, value: String) -> impl IntoElement {
    div()
        .mt_2()
        .child(div().text_xs().text_color(theme::text_muted()).child(label))
        .child(div().font_family(theme::FONT_MONO).text_sm().child(value))
}

fn format_duration_ms(ms: i64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{}m {}s", ms / 60_000, (ms % 60_000) / 1000)
    }
}

fn format_throughput(tokens: Option<i64>, duration_ms: Option<i64>) -> Option<String> {
    let tokens = tokens?;
    let duration_ms = duration_ms?;
    if duration_ms > 0 && tokens > 0 {
        let tok_per_sec = (tokens as f64) / (duration_ms as f64 / 1000.0);
        Some(format!("{tok_per_sec:.1} tok/s"))
    } else {
        None
    }
}

pub(crate) fn status_badge_info(
    status: &str,
    has_error: bool,
) -> (&'static str, gpui::Rgba, gpui::Rgba) {
    let is_complete = (status.eq_ignore_ascii_case("completed")
        || status.eq_ignore_ascii_case("done"))
        && !has_error;
    let is_fail = status.eq_ignore_ascii_case("failed")
        || status.eq_ignore_ascii_case("interrupted")
        || has_error;
    let is_running = status.eq_ignore_ascii_case("running")
        || status.eq_ignore_ascii_case("starting")
        || status.eq_ignore_ascii_case("queued");
    let is_retrying = status.eq_ignore_ascii_case("retrying");
    let is_waiting = status.eq_ignore_ascii_case("awaiting_fallback")
        || status.eq_ignore_ascii_case("awaiting_decision");

    if is_complete {
        ("COMPLETE", theme::success_soft(), theme::success())
    } else if is_fail {
        ("FAIL", theme::danger_soft(), theme::danger())
    } else if is_running {
        ("RUNNING", theme::accent_soft(), theme::accent())
    } else if is_retrying {
        ("RETRY", theme::warning_soft(), theme::warning())
    } else if is_waiting {
        ("WAITING", theme::warning_soft(), theme::warning())
    } else {
        ("IDLE", theme::surface(), theme::text_muted())
    }
}

fn status_badge(status: &str, has_error: bool) -> impl IntoElement {
    let (text, bg, fg) = status_badge_info(status, has_error);

    div()
        .flex_shrink_0()
        .px(px(6.0))
        .py(px(1.5))
        .rounded_sm()
        .text_xs()
        .font_weight(gpui::FontWeight::BOLD)
        .bg(bg)
        .text_color(fg)
        .child(text)
}

fn copy_button(
    id: SharedString,
    text: String,
    label: &'static str,
    cx: &mut Context<AppModel>,
) -> impl IntoElement {
    div()
        .id(id)
        .px(px(8.0))
        .py(px(4.0))
        .rounded_sm()
        .bg(theme::surface())
        .border_1()
        .border_color(theme::border())
        .text_xs()
        .text_color(theme::accent())
        .cursor_pointer()
        .hover(|s| s.bg(theme::surface_raised()))
        .on_click(cx.listener({
            let text = text.clone();
            move |_, _, _, cx| {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.clone()));
            }
        }))
        .child(label)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_status_badge_info_mappings() {
        assert_eq!(status_badge_info("completed", false).0, "COMPLETE");
        assert_eq!(status_badge_info("done", false).0, "COMPLETE");
        assert_eq!(status_badge_info("done", true).0, "FAIL");
        assert_eq!(status_badge_info("completed", true).0, "FAIL");
        assert_eq!(status_badge_info("failed", false).0, "FAIL");
        assert_eq!(status_badge_info("interrupted", false).0, "FAIL");
        assert_eq!(status_badge_info("running", false).0, "RUNNING");
        assert_eq!(status_badge_info("starting", false).0, "RUNNING");
        assert_eq!(status_badge_info("queued", false).0, "RUNNING");
        assert_eq!(status_badge_info("retrying", false).0, "RETRY");
        assert_eq!(status_badge_info("awaiting_fallback", false).0, "WAITING");
        assert_eq!(status_badge_info("awaiting_decision", false).0, "WAITING");
        assert_eq!(status_badge_info("unknown_status", false).0, "IDLE");
    }
}
