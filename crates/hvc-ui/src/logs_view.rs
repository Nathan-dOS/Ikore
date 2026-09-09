use gpui::{AnyElement, Context, IntoElement, SharedString, div, prelude::*, px};

use crate::app_model::AppModel;
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
                .w(px(320.0))
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
                                        .child("Runs"),
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
                            .child("Select a pulsation; agent runs will appear here."),
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
                        .children(snapshot.runs.into_iter().map(|run| {
                            let id = run.id.clone();
                            let selected = selected_run_id
                                .as_ref()
                                .is_some_and(|selected| selected.as_ref() == id);
                            let provider = run.provider.as_deref().unwrap_or("pending provider");
                            let model_name = run.model.as_deref().unwrap_or("pending model");
                            div()
                                .id(SharedString::from(format!("log-run:{id}")))
                                .p_3()
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
                                    theme::surface_raised()
                                })
                                .cursor_pointer()
                                .on_click(cx.listener(move |model, _, _, cx| {
                                    model.select_log_run(id.clone(), cx)
                                }))
                                .child(
                                    div()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child(run.agent_name),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme::text_muted())
                                        .child(format!("{} · {provider}/{model_name}", run.status)),
                                )
                        })),
                ),
        )
        .child(
            div()
                .id("logs-detail-scroll")
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
                    panel.child(
                        section("Selected run")
                            .child(fact("STATUS", run.status))
                            .child(fact(
                                "PROVIDER / MODEL",
                                format!(
                                    "{} / {}",
                                    run.provider.unwrap_or_else(|| "—".into()),
                                    run.model.unwrap_or_else(|| "—".into())
                                ),
                            ))
                            .child(fact(
                                "TOKENS",
                                format!(
                                    "{} / {}",
                                    run.input_tokens
                                        .map_or_else(|| "—".into(), |value| value.to_string()),
                                    run.output_tokens
                                        .map_or_else(|| "—".into(), |value| value.to_string())
                                ),
                            ))
                            .child(fact(
                                "COST",
                                run.estimated_cost_usd
                                    .map_or_else(|| "—".into(), |value| format!("US$ {value:.6}")),
                            ))
                            .child(fact(
                                "TIME",
                                crate::time::format_time_range(
                                    run.started_at.as_deref().or(Some(&run.created_at)),
                                    run.finished_at.as_deref(),
                                    "—",
                                ),
                            ))
                            .when_some(run.last_error, |section, value| {
                                section.child(fact("ERROR", value))
                            })
                            .when_some(run.raw_output, |section, value| {
                                section.child(fact("RAW OUTPUT", value))
                            })
                            .when_some(run.structured_output, |section, value| {
                                section.child(fact("STRUCTURED OUTPUT", value.to_string()))
                            }),
                    )
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
