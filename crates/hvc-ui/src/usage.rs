use crate::{AppModel, AppView, select_menu::Choice, settings::SettingsSection, theme};
use gpui::{AnyElement, App, Context, IntoElement, SharedString, div, prelude::*, px};
use hvc_core::usage::{UsagePeriod, UsageQuery, UsageSummary, UsageTotals, UsageTransport};
use std::time::Duration;

#[derive(Debug, Clone, Default)]
pub(crate) struct UsageState {
    pub query: UsageQuery,
    pub summary: Option<UsageSummary>,
    projects: Vec<hvc_core::usage::UsageProject>,
    pub loading: bool,
    pub error: Option<String>,
    generation: u64,
    refresh_pending: bool,
}

impl UsageState {
    fn begin(&mut self) -> u64 {
        self.generation = self.generation.saturating_add(1);
        self.loading = true;
        self.error = None;
        self.generation
    }
    fn apply(&mut self, generation: u64, result: Result<UsageSummary, String>) {
        if self.generation != generation {
            return;
        }
        self.loading = false;
        match result {
            Ok(summary) => {
                self.projects.clone_from(&summary.projects);
                self.summary = Some(summary);
            }
            Err(error) => self.error = Some(error),
        }
    }
}

impl AppModel {
    pub(crate) fn refresh_usage(&mut self, cx: &mut Context<Self>) {
        if self.is_disposed() {
            return;
        }
        let Some(core) = self.core.clone() else {
            return;
        };
        let (generation, query) = self.settings.update(cx, |settings, _| {
            (settings.usage.begin(), settings.usage.query.clone())
        });
        let runtime = core.runtime_handle().clone();
        cx.spawn(async move |this, cx| {
            let result = runtime
                .spawn_blocking(move || {
                    hvc_core::usage::query_usage(core.persistence().database_path(), &query)
                })
                .await
                .map_err(|e| format!("Usage worker failed: {e}"))
                .and_then(|result| result.map_err(|e| format!("Unable to load token usage: {e}")));
            let _ = this.update(cx, |model, cx| {
                if model.is_disposed() {
                    return;
                }
                model
                    .settings
                    .update(cx, |settings, _| settings.usage.apply(generation, result));
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn schedule_usage_refresh(&mut self, cx: &mut Context<Self>) {
        if self.active_view() != AppView::Settings
            || self.settings.read(cx).settings_section != SettingsSection::Usage
        {
            return;
        }
        if self.settings.read(cx).usage.refresh_pending {
            return;
        }
        self.settings
            .update(cx, |settings, _| settings.usage.refresh_pending = true);
        let timer = cx.background_executor().timer(Duration::from_millis(350));
        cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |model, cx| {
                model
                    .settings
                    .update(cx, |settings, _| settings.usage.refresh_pending = false);
                if model.active_view() == AppView::Settings
                    && model.settings.read(cx).settings_section == SettingsSection::Usage
                {
                    model.refresh_usage(cx);
                }
            });
        })
        .detach();
    }

    pub(crate) fn usage_choices(&self, id: &str, cx: &App) -> Vec<Choice> {
        let state = &self.settings.read(cx).usage;
        match id {
            "usage-project" => {
                let mut options = vec![Choice::new(
                    "all",
                    "All projects",
                    state.query.project_id.is_none(),
                )];
                options.extend(state.projects.iter().map(|p| {
                    Choice::new(
                        &p.id,
                        &p.name,
                        state.query.project_id.as_deref() == Some(&p.id),
                    )
                }));
                options
            }
            "usage-transport" => [
                ("all", "API + CLI", None),
                ("api", "API", Some(UsageTransport::Api)),
                ("cli", "CLI", Some(UsageTransport::Cli)),
            ]
            .into_iter()
            .map(|(value, label, target)| {
                Choice::new(value, label, state.query.transport == target)
            })
            .collect(),
            "usage-period" => [
                ("all", "All time", UsagePeriod::AllTime),
                ("7", "Last 7 days", UsagePeriod::Last7Days),
                ("30", "Last 30 days", UsagePeriod::Last30Days),
            ]
            .into_iter()
            .map(|(value, label, target)| Choice::new(value, label, state.query.period == target))
            .collect(),
            _ => Vec::new(),
        }
    }

    pub(crate) fn apply_usage_choice(&mut self, id: &str, value: &str, cx: &mut Context<Self>) {
        self.settings.update(cx, |settings, _| {
            match id {
                "usage-project" => {
                    settings.usage.query.project_id = (value != "all").then(|| value.to_string())
                }
                "usage-transport" => {
                    settings.usage.query.transport = match value {
                        "api" => Some(UsageTransport::Api),
                        "cli" => Some(UsageTransport::Cli),
                        _ => None,
                    }
                }
                "usage-period" => {
                    settings.usage.query.period = match value {
                        "7" => UsagePeriod::Last7Days,
                        "30" => UsagePeriod::Last30Days,
                        _ => UsagePeriod::AllTime,
                    }
                }
                _ => {}
            }
            // Never display old totals under newly selected filters.
            settings.usage.summary = None;
        });
        self.refresh_usage(cx);
    }
}

fn count(value: Option<i64>) -> String {
    let Some(value) = value else {
        return "Unavailable".into();
    };
    let digits = value.to_string();
    let mut result = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            result.push(',');
        }
        result.push(ch);
    }
    result
}

fn coverage(totals: &UsageTotals) -> String {
    if totals.records == 0 {
        return "No recorded calls".into();
    }
    if totals.unavailable_records == totals.records {
        return "Usage unavailable".into();
    }
    if totals.partial_records + totals.unavailable_records > 0 {
        "Partial coverage".into()
    } else {
        "Reported usage".into()
    }
}

fn card(
    id: &'static str,
    label: &'static str,
    value: Option<i64>,
    totals: &UsageTotals,
) -> AnyElement {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .flex_1()
        .min_w_0()
        .p_4()
        .rounded_lg()
        .border_1()
        .border_color(theme::border())
        .bg(theme::surface())
        .child(div().text_xs().text_color(theme::text_muted()).child(label))
        .child(
            div()
                .mt_2()
                .text_2xl()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(if totals.records == 0 {
                    "0".into()
                } else {
                    count(value)
                }),
        )
        .child(
            div()
                .mt_1()
                .text_xs()
                .text_color(theme::text_muted())
                .child(coverage(totals)),
        )
        .into_any_element()
}

pub(crate) fn usage_panel(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let state = model.settings.read(cx).usage.clone();
    let summary = state.summary.clone().unwrap_or_default();
    let loading = state.loading;
    let error = state.error.clone();
    let period = state.query.period;
    let project = state
        .query
        .project_id
        .as_deref()
        .and_then(|id| state.projects.iter().find(|p| p.id == id))
        .map(|p| p.name.clone())
        .unwrap_or_else(|| {
            if state.query.project_id.is_some() {
                "Selected project".into()
            } else {
                "All projects".into()
            }
        });
    let transport = match state.query.transport {
        Some(UsageTransport::Api) => "API",
        Some(UsageTransport::Cli) => "CLI",
        None => "API + CLI",
    };
    let period_label = match period {
        UsagePeriod::AllTime => "All time",
        UsagePeriod::Last7Days => "Last 7 days",
        UsagePeriod::Last30Days => "Last 30 days",
    };
    div().id("usage-panel").debug_selector(||"usage-panel".into()).flex_1().min_h_0().min_w_0()
        .overflow_y_scroll().p_4().flex().flex_col().gap_4()
        .child(div().flex().justify_between().items_center()
            .child(div().child(div().text_xl().font_weight(gpui::FontWeight::SEMIBOLD).child("Token usage"))
                .child(div().mt_1().text_sm().text_color(theme::text_muted()).child("Generation usage recorded by HVC, retained when conversations are deleted.")))
            .child(div().id("usage-refresh").debug_selector(||"usage-refresh".into()).px_3().py_2().rounded_md()
                .bg(theme::accent_soft()).text_color(theme::accent()).cursor_pointer()
                .on_click(cx.listener(|model,_,_,cx|model.refresh_usage(cx)))
                .child(if loading {"Refreshing…"}else{"Refresh"})))
        .child(div().flex().gap_3()
            .child(div().flex_1().min_w_0().child(crate::select_menu::control("usage-project","PROJECT",project,true,cx)))
            .child(div().w(px(180.0)).child(crate::select_menu::control("usage-transport","TRANSPORT",transport,true,cx)))
            .child(div().w(px(180.0)).child(crate::select_menu::control("usage-period","PERIOD",period_label,true,cx))))
        .when_some(error,|panel,error|panel.child(div().text_sm().text_color(theme::danger()).child(error)))
        .when(loading && state.summary.is_none(),|panel|panel.child(div().text_sm().child("Loading token usage…")))
        .when(state.summary.is_some(),|panel|panel
            .child(div().flex().gap_3()
                .child(card("usage-total","TOTAL TOKENS",summary.totals.total_tokens,&summary.totals))
                .child(card("usage-input","INPUT TOKENS",summary.totals.input_tokens,&summary.totals))
                .child(card("usage-output","OUTPUT TOKENS",summary.totals.output_tokens,&summary.totals)))
            .child(div().text_xs().text_color(theme::text_muted()).child(format!(
                "{} accounting units · {} partial · {} unavailable. Cache is included in input; reasoning is included in output when reported. Historical records may be incomplete. CLI collection begins with this feature.",
                summary.totals.records,summary.totals.partial_records,summary.totals.unavailable_records)))
            .child(usage_chart(&summary,period))
            .child(model_table(&summary)))
        .into_any_element()
}

fn usage_chart(summary: &UsageSummary, period: UsagePeriod) -> AnyElement {
    let maximum = summary
        .series
        .iter()
        .filter_map(|b| b.totals.total_tokens)
        .max()
        .unwrap_or(1)
        .max(1) as f64;
    let length = summary.series.len();
    div()
        .p_4()
        .rounded_lg()
        .bg(theme::surface())
        .border_1()
        .border_color(theme::border())
        .flex_shrink_0()
        .child(
            div()
                .text_sm()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(if period == UsagePeriod::AllTime {
                    "Monthly usage · UTC"
                } else {
                    "Daily usage · UTC"
                }),
        )
        .child(
            div()
                .mt_1()
                .text_xs()
                .text_color(theme::text_muted())
                .child("Gray bars indicate partial coverage. Empty dates have no recorded calls."),
        )
        .when(length == 0, |chart| {
            chart.child(
                div()
                    .mt_3()
                    .text_sm()
                    .text_color(theme::text_muted())
                    .child("No usage recorded in this period."),
            )
        })
        .when(length > 0, |chart| {
            chart.child(
                div()
                    .id("usage-chart")
                    .debug_selector(|| "usage-chart".into())
                    .mt_3()
                    .h(px(165.0))
                    .overflow_x_scroll()
                    .flex()
                    .items_end()
                    .gap_1()
                    .children(summary.series.iter().enumerate().map(|(index, bucket)| {
                        let value = bucket.totals.total_tokens.unwrap_or(0);
                        let height = (value as f64 / maximum * 100.0) as f32;
                        let label = if length <= 12 || index % length.div_ceil(10) == 0 {
                            bucket.period.clone()
                        } else {
                            String::new()
                        };
                        div()
                            .flex_1()
                            .min_w(px(24.0))
                            .h_full()
                            .flex()
                            .flex_col()
                            .justify_end()
                            .gap_1()
                            .child(div().text_xs().text_color(theme::text_muted()).child(
                                if length <= 12 {
                                    if bucket.totals.records == 0 {
                                        "—".into()
                                    } else {
                                        count(bucket.totals.total_tokens)
                                    }
                                } else {
                                    String::new()
                                },
                            ))
                            .child(
                                div()
                                    .h(px(if bucket.totals.records == 0 {
                                        0.0
                                    } else {
                                        height.max(2.0)
                                    }))
                                    .w_full()
                                    .rounded_sm()
                                    .bg(
                                        if bucket.totals.unavailable_records > 0
                                            || bucket.totals.partial_records > 0
                                        {
                                            theme::text_muted()
                                        } else {
                                            theme::accent()
                                        },
                                    ),
                            )
                            .child(
                                div()
                                    .h(px(30.0))
                                    .text_xs()
                                    .text_color(theme::text_muted())
                                    .child(label),
                            )
                    })),
            )
        })
        .into_any_element()
}

fn cell(text: impl Into<SharedString>, width: f32) -> AnyElement {
    div()
        .w(px(width))
        .flex_shrink_0()
        .text_sm()
        .child(text.into())
        .into_any_element()
}

fn model_table(summary: &UsageSummary) -> AnyElement {
    div()
        .id("usage-model-table")
        .debug_selector(|| "usage-model-table".into())
        .p_4()
        .rounded_lg()
        .bg(theme::surface())
        .border_1()
        .border_color(theme::border())
        .flex_shrink_0()
        .child(
            div()
                .text_sm()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child("Usage by model"),
        )
        .child(
            div()
                .mt_3()
                .flex()
                .gap_3()
                .text_color(theme::text_muted())
                .child(div().flex_1().child("Model / provider"))
                .child(cell("Transport", 70.0))
                .child(cell("Input", 105.0))
                .child(cell("Output", 105.0))
                .child(cell("Total", 105.0)),
        )
        .when(summary.models.is_empty(), |table| {
            table.child(
                div()
                    .mt_3()
                    .text_sm()
                    .text_color(theme::text_muted())
                    .child("Run an agent or send a Companion message to record usage."),
            )
        })
        .children(summary.models.iter().map(|entry| {
            div()
                .mt_3()
                .pt_3()
                .border_t_1()
                .border_color(theme::border())
                .child(
                    div()
                        .flex()
                        .gap_3()
                        .items_center()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(div().text_sm().child(entry.model_id.clone()))
                                .child(div().text_xs().text_color(theme::text_muted()).child(
                                    format!("{} · {} model", entry.provider_id, entry.model_source),
                                )),
                        )
                        .child(cell(entry.transport.as_str().to_ascii_uppercase(), 70.0))
                        .child(cell(count(entry.totals.input_tokens), 105.0))
                        .child(cell(count(entry.totals.output_tokens), 105.0))
                        .child(cell(count(entry.totals.total_tokens), 105.0)),
                )
                .child(
                    div()
                        .mt_1()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(format!(
                            "{} · {} units · Cache read: {} · Cache write: {} · Reasoning: {}",
                            coverage(&entry.totals),
                            entry.totals.records,
                            count(entry.totals.cache_read_tokens),
                            count(entry.totals.cache_write_tokens),
                            count(entry.totals.reasoning_tokens)
                        )),
                )
        }))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stale_queries_cannot_replace_new_filters_and_unknown_usage_is_not_zero() {
        let mut state = UsageState::default();
        let old = state.begin();
        state.query.transport = Some(UsageTransport::Cli);
        let current = state.begin();
        state.apply(old, Ok(UsageSummary::default()));
        assert!(state.summary.is_none());
        assert!(state.loading);
        state.apply(current, Ok(UsageSummary::default()));
        assert!(!state.loading);
        assert_eq!(count(None), "Unavailable");
        assert_eq!(count(Some(0)), "0");
        assert_eq!(count(Some(1234567)), "1,234,567");
    }

    #[gpui::test]
    fn usage_panel_mounts_without_a_provider_connection(cx: &mut gpui::TestAppContext) {
        let (model, visual) = cx.add_window_view(|_, cx| {
            let mut model = crate::layout_tests::fixture(cx);
            model.select_view(AppView::Settings, cx);
            model.settings.update(cx, |settings, _| {
                settings.settings_section = SettingsSection::Usage;
                settings.usage.summary = Some(UsageSummary::default());
                settings.snapshot = None;
            });
            model
        });
        visual.run_until_parked();
        assert!(visual.debug_bounds("usage-panel").unwrap().size.width > px(0.0));
        assert!(visual.debug_bounds("usage-total").unwrap().size.height > px(0.0));
        model.read_with(visual, |model, cx| {
            assert!(model.choice_options("usage-project", cx)[0].selected)
        });
        model.update(visual, |model, cx| {
            model.settings.update(cx, |settings, _| {
                let totals = UsageTotals {
                    input_tokens: Some(2000),
                    output_tokens: Some(500),
                    total_tokens: Some(2500),
                    records: 1,
                    ..Default::default()
                };
                let summary = UsageSummary {
                    totals: totals.clone(),
                    models: vec![hvc_core::usage::ModelUsage {
                        provider_id: "openai".into(),
                        model_id: "observed-model".into(),
                        model_source: "observed".into(),
                        transport: UsageTransport::Api,
                        totals: totals.clone(),
                    }],
                    series: vec![hvc_core::usage::UsageBucket {
                        period: "2026-09".into(),
                        totals,
                    }],
                    projects: vec![hvc_core::usage::UsageProject {
                        id: "project-a".into(),
                        name: "Project A".into(),
                    }],
                };
                let generation = settings.usage.begin();
                settings.usage.apply(generation, Ok(summary));
            });
            cx.notify();
        });
        visual.simulate_resize(gpui::size(px(960.0), px(640.0)));
        visual.run_until_parked();
        let card_before = visual.debug_bounds("usage-total").unwrap();
        let panel = visual.debug_bounds("usage-panel").unwrap();
        let table = visual.debug_bounds("usage-model-table").unwrap();
        assert!(table.size.width <= panel.size.width);
        assert!(visual.debug_bounds("usage-chart").unwrap().size.width > px(0.0));
        model.update(visual, |model, cx| {
            model.settings.update(cx, |settings, _| {
                settings.usage.begin();
            });
            cx.notify();
        });
        visual.run_until_parked();
        assert_eq!(visual.debug_bounds("usage-total").unwrap(), card_before);
        model.update(visual, |model, cx| {
            model.apply_usage_choice("usage-project", "project-a", cx);
            assert!(model.settings.read(cx).usage.summary.is_none());
            assert!(
                model
                    .usage_choices("usage-project", cx)
                    .iter()
                    .any(|c| c.selected && c.value == "project-a")
            );
        });
    }
}
