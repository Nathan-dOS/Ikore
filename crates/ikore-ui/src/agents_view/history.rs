//! An agent's runs as a table (ADR-040 M7, specification 12.2): Started,
//! Origin, Status, Duration, Tokens, Exit and Model. A row opens its run in
//! Activity.

use gpui::{AnyElement, Context, IntoElement, Rgba, SharedString, div, prelude::*, px};
use ikore_core::terminals::TerminalRun;

use crate::agents::AgentIdentifier;
use crate::app_model::AppModel;
use crate::theme;

use super::{empty_state, origin_label, run_status, status_color};

/// The columns and their widths; Model takes what is left.
const COLUMNS: [(&str, f32); 6] = [
    ("Started", 150.0),
    ("Origin", 96.0),
    ("Status", 104.0),
    ("Duration", 72.0),
    ("Tokens", 72.0),
    ("Exit", 48.0),
];

fn cell(text: impl Into<SharedString>, width: Option<f32>, color: Rgba) -> AnyElement {
    let cell = div().flex_shrink_0().truncate().text_xs().text_color(color);
    match width {
        Some(width) => cell.w(px(width)),
        None => cell.flex_1().min_w_0(),
    }
    .child(text.into())
    .into_any_element()
}

/// Output tokens of a run that ended in this session, compacted.
fn tokens(model: &AppModel, run: &TerminalRun, cx: &gpui::App) -> String {
    model
        .agents
        .read(cx)
        .live
        .finished(&run.id)
        .and_then(|counters| counters.output_tokens)
        .map_or_else(
            || "—".to_owned(),
            |tokens| {
                if tokens >= 1_000 {
                    format!("{:.1}k", tokens as f64 / 1_000.0)
                } else {
                    tokens.to_string()
                }
            },
        )
}

pub(super) fn history_tab(
    model: &AppModel,
    agent: &AgentIdentifier,
    runs: &[TerminalRun],
    selected: Option<&str>,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    if runs.is_empty() {
        return empty_state("No runs yet.");
    }
    let header = div()
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .pb_1()
        .border_b_1()
        .border_color(theme::border())
        .children(
            COLUMNS
                .iter()
                .map(|(label, width)| cell(*label, Some(*width), theme::text_muted())),
        )
        .child(cell("Model", None, theme::text_muted()));
    let rows = runs
        .iter()
        .enumerate()
        .map(|(index, run)| {
            let duration = crate::agent_console::view::run_duration_ms(run).map_or_else(
                || "—".to_owned(),
                crate::agent_console::view::duration_label,
            );
            let values = [
                crate::time::format_timestamp(&run.created_at),
                origin_label(run.origin).to_owned(),
                run_status(run.status).to_owned(),
                duration,
                tokens(model, run, cx),
                run.exit_code
                    .map_or_else(|| "—".to_owned(), |code| code.to_string()),
            ];
            let model_name = run
                .observed_model
                .as_deref()
                .or(run.model.as_deref())
                .unwrap_or("default")
                .to_owned();
            let selector = format!("agent-history-row:{index}");
            let open = (agent.clone(), run.id.clone());
            div()
                .id(SharedString::from(selector.clone()))
                .debug_selector(move || selector.clone())
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .when(selected == Some(run.id.as_str()), |row| {
                    row.bg(theme::accent_soft())
                })
                .hover(|style| style.bg(theme::surface_hover()))
                .on_click(cx.listener(move |model, _, _, cx| {
                    model.open_agent_run(open.0.clone(), open.1.clone(), cx);
                }))
                .children(values.into_iter().zip(COLUMNS).enumerate().map(
                    |(column, (value, (_, width)))| {
                        let color = if column == 2 {
                            status_color(Some(run.status))
                        } else {
                            theme::text()
                        };
                        cell(value, Some(width), color)
                    },
                ))
                .child(cell(model_name, None, theme::text_muted()))
        })
        .collect::<Vec<_>>();
    div()
        .id("agent-history")
        .debug_selector(|| "agent-history".into())
        .flex()
        .flex_col()
        .gap_1()
        .child(header)
        .children(rows)
        .into_any_element()
}
