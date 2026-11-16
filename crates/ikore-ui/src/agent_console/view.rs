//! The agent console's rendering (ADR-040, M2; specification 8.3 and 8.4):
//! the run header, the blocks in a virtual list that follows the newest one,
//! the `{n} new ↓` pill and the footer.

use std::rc::Rc;

use gpui::{
    AnyElement, App, ClipboardItem, Context, FocusHandle, FontWeight, SharedString, WeakEntity,
    div, prelude::*, px,
};
use ikore_core::terminals::{ActionStatus, TerminalRun, TerminalRunStatus, ToolKind};

use super::model::{ActionGroup, ConsoleRow, ConsoleView};
use super::projection::{
    ActionBlock, ActionState, BlockBody, ConsoleBlock, ConsoleFilter, NoticeLevel,
    OUTPUT_PREVIEW_LINES, OutputStream, RESULT_PREVIEW_LINES,
};
use crate::app_model::AppModel;
use crate::icons::Icon;
use crate::theme;

/// What a console shows and where.
pub(crate) struct ConsoleSurface<'a> {
    /// The surface key: the agent role, `free-form:{slot}`.
    pub(crate) key: String,
    pub(crate) agent_label: &'a str,
    pub(crate) run: Option<&'a TerminalRun>,
    pub(crate) filter: ConsoleFilter,
    /// The panel fills its column instead of the card's fixed height.
    pub(crate) expanded: bool,
}

pub(crate) fn tool_icon(kind: ToolKind) -> Icon {
    match kind {
        ToolKind::Read => Icon::FileText,
        ToolKind::Write | ToolKind::Edit => Icon::Pencil,
        ToolKind::Search => Icon::Search,
        ToolKind::List => Icon::Folder,
        ToolKind::Shell => Icon::ScrollText,
        ToolKind::Web => Icon::Route,
        ToolKind::Mcp => Icon::Plug,
        ToolKind::Task => Icon::Bot,
        ToolKind::Plan => Icon::ListTodo,
        ToolKind::Other => Icon::Hammer,
    }
}

/// `0.2 s`, `16 s`, `2:14`.
pub(crate) fn duration_label(ms: u64) -> String {
    if ms < 10_000 {
        format!("{:.1} s", ms as f64 / 1000.0)
    } else if ms < 60_000 {
        format!("{} s", ms / 1000)
    } else {
        let seconds = ms / 1000;
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
}

/// The milliseconds a run took, or has taken so far.
pub(crate) fn run_duration_ms(run: &TerminalRun) -> Option<u64> {
    let start = run.started_at.as_deref().unwrap_or(&run.created_at);
    let start = chrono::DateTime::parse_from_rfc3339(start).ok()?;
    let end = match run.finished_at.as_deref() {
        Some(end) => chrono::DateTime::parse_from_rfc3339(end).ok()?,
        None if run.status.is_active() => chrono::Utc::now().fixed_offset(),
        None => return None,
    };
    u64::try_from((end - start).num_milliseconds()).ok()
}

/// The run header (PD4): `Completed · 1:12 · 14 actions · 1 failed`.
pub(crate) fn run_summary(run: &TerminalRun, actions: usize, failed: usize) -> String {
    let mut parts = vec![crate::agents_view::run_status(run.status).to_owned()];
    if let Some(ms) = run_duration_ms(run) {
        parts.push(duration_label(ms));
    }
    parts.push(if actions == 1 {
        "1 action".to_owned()
    } else {
        format!("{actions} actions")
    });
    if failed > 0 {
        parts.push(format!("{failed} failed"));
    }
    if let Some(code) = run.exit_code.filter(|_| run.status.is_terminal()) {
        parts.push(format!("exit {code}"));
    }
    parts.join(" · ")
}

/// The state's icon, colour and words: never colour alone (ADR-012).
fn state_badge(action: &ActionBlock) -> (Icon, gpui::Rgba, String) {
    match &action.state {
        ActionState::Running => (Icon::Timer, theme::running(), "running".into()),
        ActionState::Done {
            exit_code: Some(code),
            ..
        } if *code != 0 => (Icon::X, theme::danger(), format!("exit {code}")),
        ActionState::Done {
            status: ActionStatus::Error,
            ..
        } => (Icon::X, theme::danger(), "error".into()),
        ActionState::Done {
            status: ActionStatus::Denied,
            ..
        } => (Icon::Ban, theme::warning(), "denied".into()),
        ActionState::Done {
            status: ActionStatus::Unknown,
            ..
        } => (Icon::CircleHelp, theme::text_muted(), "unknown".into()),
        ActionState::Done {
            status: ActionStatus::Ok,
            duration_ms,
            ..
        } => (
            Icon::Check,
            theme::success(),
            duration_ms.map_or_else(|| "ok".into(), duration_label),
        ),
    }
}

/// Lines shown of `text`, and how many stay hidden.
fn preview(text: &str, limit: usize) -> (String, usize) {
    let mut lines = text.lines();
    let shown = lines.by_ref().take(limit).collect::<Vec<_>>().join("\n");
    (shown, lines.count())
}

/// What one row needs besides its data.
struct RowView {
    index: usize,
    open: bool,
    selected: bool,
    current_match: bool,
    copied: bool,
}

/// `Live` at the end of the console, or `Jump to latest` that brings the
/// newest row back (specification 8.4). Not the dock's `Following`, which
/// is about the agent. Shared with the API viewer.
pub(crate) fn follow_control(
    id: String,
    following: bool,
    cx: &mut Context<AppModel>,
    resume: impl Fn(&mut AppModel, &mut Context<AppModel>) + 'static,
) -> AnyElement {
    let selector = id.clone();
    // It reports a state, and explains itself once the pointer rests on it.
    let explanation = if following {
        "Live: the console shows the newest output as it arrives. Scroll up to read earlier lines."
    } else {
        "Paused where you scrolled while new output keeps arriving. Click, or press End, to jump to the latest."
    };
    div()
        .id(SharedString::from(id))
        .debug_selector(move || selector.clone())
        .tooltip(move |_, cx| {
            cx.new(|_| {
                crate::widgets::Tooltip::new(explanation).after_rest(crate::widgets::EXPLAIN_REST)
            })
            .into()
        })
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
                .on_click(cx.listener(move |model, _, _, cx| resume(model, cx)))
        })
        .child(crate::widgets::status_dot(if following {
            theme::success()
        } else {
            theme::text_subtle()
        }))
        .child(if following { "Live" } else { "Jump to latest" })
        .into_any_element()
}

/// `{n} new ↓` over the bottom of a paused console.
pub(crate) fn new_pill(
    id: String,
    unseen: usize,
    cx: &mut Context<AppModel>,
    resume: impl Fn(&mut AppModel, &mut Context<AppModel>) + 'static,
) -> AnyElement {
    div()
        .id(SharedString::from(id))
        .debug_selector(|| "agent-console-new-pill".into())
        .absolute()
        .bottom_3()
        .right_3()
        .px_2()
        .py_1()
        .rounded_full()
        .bg(theme::accent())
        .text_xs()
        .text_color(theme::surface())
        .cursor_pointer()
        .on_click(cx.listener(move |model, _, _, cx| resume(model, cx)))
        .child(format!("{unseen} new ↓"))
        .into_any_element()
}

/// The frame every console shares: canvas, border, and the wheel kept inside.
pub(crate) fn frame(id: String, selector: String, expanded: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(SharedString::from(id))
        .debug_selector(move || selector.clone())
        .relative()
        .flex()
        .flex_col()
        .min_h_0()
        .when(expanded, |panel| panel.flex_1())
        .when(!expanded, |panel| {
            panel.h(crate::sizing::AGENT_CONSOLE_HEIGHT)
        })
        .rounded_md()
        .bg(theme::canvas())
        .border_1()
        .border_color(theme::border())
        // Own the wheel: the page around the console scrolls too.
        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
}

/// The console of one agent's selected run.
pub(crate) fn console(
    model: &AppModel,
    surface: ConsoleSurface<'_>,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let ConsoleSurface {
        key,
        agent_label,
        run,
        filter,
        expanded,
    } = surface;
    let (view, focus, now_line, reasoning, changes) = {
        let agents = model.agents.read(cx);
        // PD6: what a finished stage run published.
        let changes = run.and_then(|run| agents.run_changes.get(&run.id).cloned());
        let events = run
            .and_then(|run| agents.events_by_run.get(&run.id))
            .map_or(&[][..], Vec::as_slice);
        let live = run.and_then(|run| agents.live.get(&run.id));
        let view: ConsoleView = agents.consoles.view_live(
            &key,
            run.map(|run| run.id.as_str()),
            run.is_some_and(|run| run.status.is_terminal()),
            events,
            filter,
            live,
        );
        // M6: how much a finished run's model reasoned, never what.
        let reasoning = run
            .and_then(|run| agents.live.finished(&run.id))
            .filter(|counters| counters.reasoning_blocks > 0 || counters.reasoning_tokens.is_some())
            .map(|counters| {
                super::live::reasoning_label(counters.reasoning_blocks, counters.reasoning_tokens)
            });
        (
            view,
            agents.consoles.focus(&key, cx),
            live.map(|live| live.line(std::time::Instant::now())),
            reasoning,
            changes,
        )
    };
    // Read once per finished stage run, after this frame.
    if let Some(run) = run.filter(|run| {
        changes.is_none()
            && run.job_id.is_some()
            && run.status.is_terminal()
            && model.agents_backend.is_some()
    }) {
        let app = cx.entity().downgrade();
        let run = run.clone();
        cx.defer(move |cx| {
            let _ = app.update(cx, |model, cx| model.load_run_changes(&run, cx));
        });
    }
    let search = model.console_search_query(&key, cx);
    let matches = super::model::search_matches(&view.rows.rows, search.as_deref());
    let current_match = model
        .console_search_position(&key, matches.len())
        .and_then(|position| matches.get(position).copied());
    let app = cx.entity().downgrade();
    let empty = match run {
        None => Some(vec![format!("No runs yet for {agent_label}.")]),
        Some(run) if view.rows.rows.is_empty() && run.status.is_active() => {
            Some(vec!["Waiting for the first event…".to_owned()])
        }
        Some(run)
            if view.rows.rows.is_empty()
                && filter == ConsoleFilter::All
                && run.status == TerminalRunStatus::Failed =>
        {
            let mut lines = vec!["Run failed before producing output.".to_owned()];
            if let Some(message) = run.error_message.as_deref() {
                lines.push(format!("Error: {message}"));
            }
            if let Some(reason) = run
                .termination_reason
                .as_deref()
                .filter(|reason| Some(*reason) != run.error_message.as_deref())
            {
                lines.push(format!("Reason: {reason}"));
            }
            Some(lines)
        }
        Some(_) if view.rows.rows.is_empty() => Some(vec![match filter {
            ConsoleFilter::All => "No activity recorded for the selected run.".to_owned(),
            other => format!("Nothing under {} in this run.", other.label()),
        }]),
        Some(_) => None,
    };
    let key_for_keys = key.clone();
    let mut panel = frame(
        format!("agent-console:{key}"),
        format!("agent-console:{key}"),
        expanded,
    )
    .track_focus(&focus)
    .on_key_down(
        cx.listener(move |model, event: &gpui::KeyDownEvent, window, cx| {
            if model.console_key(&key_for_keys, event, window, cx) {
                cx.stop_propagation();
            }
        }),
    )
    .child(run_header(
        &key,
        run,
        filter,
        &view,
        reasoning.as_deref(),
        expanded,
        cx,
    ));
    if let Some(query) = search.as_ref() {
        panel = panel.child(search_bar(model, &key, query, &matches, current_match, cx));
    }
    match empty {
        Some(lines) => {
            let failed = run.is_some_and(|run| run.status == TerminalRunStatus::Failed);
            panel = panel.child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_2()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .children(lines.into_iter().enumerate().map(|(index, line)| {
                        div()
                            .when(failed && index == 0, |line| {
                                line.text_color(theme::danger())
                            })
                            .child(line)
                    })),
            );
        }
        None => {
            let rows = Rc::clone(&view.rows);
            let list_key = key.clone();
            let list_app = app.clone();
            let open = Rc::clone(&view.expanded);
            let selected = view.selected;
            let copied = view.copied.clone();
            let focus_for_rows = focus.clone();
            panel = panel.child(
                gpui::list(view.list.clone(), move |index, _, _| {
                    let Some(row) = rows.rows.get(index) else {
                        return div().into_any_element();
                    };
                    let row_view = RowView {
                        index,
                        open: open.contains(row.key()),
                        selected: selected == Some(index),
                        current_match: current_match == Some(index),
                        copied: copied.as_deref() == Some(row.key()),
                    };
                    render_row(row, row_view, &list_key, &list_app, &focus_for_rows)
                })
                .flex_1()
                .px_1()
                .py_1(),
            );
        }
    }
    if let (Some(run), Some(crate::run_changes::RunChanges::Loaded(files))) = (run, &changes) {
        panel = panel.children(crate::run_changes::changes_card(
            &key,
            agent_label,
            run,
            files,
            cx,
        ));
    }
    if !view.following && view.unseen > 0 {
        let pill_key = key.clone();
        panel = panel.child(new_pill(
            format!("agent-console-new-pill:{key}"),
            view.unseen,
            cx,
            move |model, cx| model.console_follow(&pill_key, cx),
        ));
    }
    if run.is_some_and(|run| run.status.is_active()) {
        // What the run does now (M5), or why its text has not shown yet.
        let selector = format!("agent-console-now:{key}");
        panel =
            panel.child(
                div()
                    .debug_selector(move || selector.clone())
                    .px_2()
                    .py_1()
                    .border_t_1()
                    .border_color(theme::border())
                    .text_xs()
                    .text_color(theme::text_muted())
                    .truncate()
                    .child(now_line.unwrap_or_else(|| {
                        "Response text appears when the run completes.".to_owned()
                    })),
            );
    }
    panel.into_any_element()
}

/// Status, duration and actions of the run (PD4), the display of a finished
/// run's actions and the console's own controls.
fn run_header(
    key: &str,
    run: Option<&TerminalRun>,
    filter: ConsoleFilter,
    view: &ConsoleView,
    reasoning: Option<&str>,
    expanded: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let color = match run.map(|run| run.status) {
        Some(TerminalRunStatus::Failed | TerminalRunStatus::Interrupted) => theme::danger(),
        Some(TerminalRunStatus::Completed) => theme::success(),
        Some(TerminalRunStatus::Cancelled) => theme::warning(),
        Some(_) => theme::running(),
        None => theme::text_subtle(),
    };
    let summary = run.map_or_else(
        || "No run".to_owned(),
        |run| {
            let mut summary = run_summary(run, view.rows.actions, view.rows.failed);
            if let Some(reasoning) = reasoning {
                summary.push_str(" · ");
                summary.push_str(reasoning);
            }
            summary
        },
    );
    let search_key = key.to_owned();
    let copy_key = key.to_owned();
    let follow_key = key.to_owned();
    div()
        .id(SharedString::from(format!("agent-console-header:{key}")))
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .py_1()
        .border_b_1()
        .border_color(theme::border())
        .text_xs()
        .child(crate::widgets::status_dot(color))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(theme::text())
                .child(summary),
        )
        .when(
            run.is_some_and(|run| run.status.is_terminal()) && view.rows.actions > 1,
            |header| {
                header.child(crate::select_menu::compact_control(
                    format!("agent-console-display:{key}"),
                    "Actions",
                    view.display.label(),
                    true,
                    cx,
                ))
            },
        )
        .child(crate::select_menu::compact_control(
            format!("agent-console-filter:{key}"),
            "Filter",
            filter.label(),
            run.is_some(),
            cx,
        ))
        .child(crate::widgets::icon_button(
            SharedString::from(format!("agent-console-search:{search_key}")),
            Icon::Search,
            crate::widgets::Tooltip::new("Search the console").with_shortcut("Ctrl+F"),
            false,
            None,
            !view.rows.rows.is_empty(),
            cx,
            move |model, window, cx| model.console_open_search(&search_key, window, cx),
        ))
        .child(crate::widgets::icon_button(
            SharedString::from(format!("agent-console-copy-all:{copy_key}")),
            if view.copied.as_deref() == Some("*") {
                Icon::Check
            } else {
                Icon::Copy
            },
            if view.copied.as_deref() == Some("*") {
                "Copied"
            } else {
                "Copy all"
            },
            false,
            None,
            !view.rows.rows.is_empty(),
            cx,
            move |model, _, cx| model.console_copy_all(&copy_key, cx),
        ))
        .child(follow_control(
            format!("agent-console-follow:{key}"),
            view.following,
            cx,
            move |model, cx| model.console_follow(&follow_key, cx),
        ))
        .when(!expanded, |header| {
            header.child(crate::widgets::icon_button(
                SharedString::from(format!("agent-expand:{key}")),
                Icon::ChevronUp,
                "Expand terminal",
                false,
                None,
                true,
                cx,
                |model, _, cx| model.toggle_agents_terminal_expanded(cx),
            ))
        })
        .into_any_element()
}

fn search_bar(
    model: &AppModel,
    key: &str,
    query: &str,
    matches: &[usize],
    current: Option<usize>,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let position = current
        .and_then(|row| matches.iter().position(|candidate| *candidate == row))
        .map_or(0, |position| position + 1);
    let status = if query.trim().is_empty() {
        String::new()
    } else if matches.is_empty() {
        "No matches".to_owned()
    } else {
        format!("{position} of {}", matches.len())
    };
    let (previous_key, next_key, close_key) = (key.to_owned(), key.to_owned(), key.to_owned());
    div()
        .id(SharedString::from(format!(
            "agent-console-search-bar:{key}"
        )))
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .py_1()
        .border_b_1()
        .border_color(theme::border())
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(model.agent_console_search_input.clone()),
        )
        .child(
            div()
                .flex_shrink_0()
                .text_xs()
                .text_color(theme::text_muted())
                .child(status),
        )
        .child(crate::widgets::icon_button(
            SharedString::from(format!("agent-console-search-previous:{key}")),
            Icon::ChevronUp,
            crate::widgets::Tooltip::new("Previous match").with_shortcut("Shift+Enter"),
            false,
            None,
            !matches.is_empty(),
            cx,
            move |model, _, cx| model.console_search_step(&previous_key, -1, cx),
        ))
        .child(crate::widgets::icon_button(
            SharedString::from(format!("agent-console-search-next:{key}")),
            Icon::ChevronDown,
            crate::widgets::Tooltip::new("Next match").with_shortcut("Enter"),
            false,
            None,
            !matches.is_empty(),
            cx,
            move |model, _, cx| model.console_search_step(&next_key, 1, cx),
        ))
        .child(crate::widgets::icon_button(
            SharedString::from(format!("agent-console-search-close:{key}")),
            Icon::X,
            crate::widgets::Tooltip::new("Close search").with_shortcut("Esc"),
            false,
            None,
            true,
            cx,
            move |model, window, cx| model.console_close_search(&close_key, window, cx),
        ))
        .into_any_element()
}

fn copy_row(app: &WeakEntity<AppModel>, key: &str, row_key: &str, text: String, cx: &mut App) {
    cx.write_to_clipboard(ClipboardItem::new_string(text));
    let (key, row_key) = (key.to_owned(), row_key.to_owned());
    let _ = app.update(cx, |model, cx| model.console_copied(&key, row_key, cx));
}

fn toggle_link(key: &str, row_key: &str, label: String, app: &WeakEntity<AppModel>) -> AnyElement {
    let app = app.clone();
    let (key, row_key) = (key.to_owned(), row_key.to_owned());
    div()
        .id(SharedString::from(format!(
            "agent-console-toggle:{row_key}"
        )))
        .text_xs()
        .text_color(theme::accent())
        .cursor_pointer()
        .on_click(move |_, _, cx| {
            cx.stop_propagation();
            let _ = app.update(cx, |model, cx| model.console_toggle_row(&key, &row_key, cx));
        })
        .child(label)
        .into_any_element()
}

fn render_row(
    row: &ConsoleRow,
    view: RowView,
    key: &str,
    app: &WeakEntity<AppModel>,
    focus: &FocusHandle,
) -> AnyElement {
    let selector = format!("agent-console-block:{}", view.index);
    let element_id = SharedString::from(format!("agent-console-block:{key}:{}", view.index));
    let text = row.text();
    let (copy_app, copy_key, copy_row_key) = (app.clone(), key.to_owned(), row.key().to_owned());
    let (select_app, select_key, index) = (app.clone(), key.to_owned(), view.index);
    let focus = focus.clone();
    let mut element = div()
        .id(element_id)
        .debug_selector(move || selector.clone())
        .relative()
        .w_full()
        .px_2()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(if view.current_match || view.selected {
            gpui::Hsla::from(theme::accent())
        } else {
            gpui::transparent_black()
        })
        .hover(|style| style.bg(theme::surface_hover()))
        .cursor_pointer()
        .on_click(move |event, window, cx| {
            window.focus(&focus);
            let _ = select_app.update(cx, |model, cx| model.console_select(&select_key, index, cx));
            // A double click copies the row: a single click only selects it.
            if event.click_count() >= 2 {
                copy_row(&copy_app, &copy_key, &copy_row_key, text.clone(), cx);
            }
        });
    element = match row {
        ConsoleRow::Block(block) => element.child(block_body(block, &view, key, app)),
        ConsoleRow::Group(group) => element.child(group_row(group, &view, key, app)),
    };
    if view.copied {
        element = element.child(
            div()
                .absolute()
                .top_1()
                .right_2()
                .px_1()
                .rounded_sm()
                .bg(theme::success_soft())
                .text_xs()
                .text_color(theme::success())
                .child("Copied"),
        );
    }
    element.into_any_element()
}

fn group_row(
    group: &ActionGroup,
    view: &RowView,
    key: &str,
    app: &WeakEntity<AppModel>,
) -> AnyElement {
    let color = if group.failed > 0 {
        theme::warning()
    } else {
        theme::text_muted()
    };
    div()
        .flex()
        .items_center()
        .gap_2()
        .text_xs()
        .text_color(color)
        .child(crate::widgets::icon(
            if view.open {
                Icon::ChevronDown
            } else {
                Icon::ChevronRight
            },
            px(12.0),
            color,
        ))
        .child(div().flex_1().min_w_0().truncate().child(group.label()))
        .child(toggle_link(
            key,
            &group.key,
            if view.open { "Hide" } else { "Show" }.into(),
            app,
        ))
        .into_any_element()
}

fn block_body(
    block: &ConsoleBlock,
    view: &RowView,
    key: &str,
    app: &WeakEntity<AppModel>,
) -> AnyElement {
    match &block.body {
        BlockBody::Action(action) => action_block(action, block, view, key, app),
        BlockBody::Prose(text) => div()
            .text_sm()
            .font_family(theme::FONT_UI)
            .text_color(theme::text())
            .child(text.clone())
            .into_any_element(),
        BlockBody::Output { stream, text } => {
            let (shown, hidden) = if view.open {
                (text.clone(), 0)
            } else {
                preview(text, OUTPUT_PREVIEW_LINES)
            };
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .font_family(theme::FONT_MONO)
                        .text_color(if *stream == OutputStream::Stderr {
                            theme::warning()
                        } else {
                            theme::text()
                        })
                        .child(shown),
                )
                .when(hidden > 0, |column| {
                    column.child(toggle_link(
                        key,
                        &block.key,
                        format!("Show {hidden} more lines"),
                        app,
                    ))
                })
                .into_any_element()
        }
        BlockBody::Notice { level, text } => {
            let (glyph, color) = match level {
                NoticeLevel::Warning => (Icon::TriangleAlert, theme::warning()),
                NoticeLevel::Error => (Icon::X, theme::danger()),
            };
            div()
                .flex()
                .items_start()
                .gap_2()
                .text_sm()
                .text_color(color)
                .child(crate::widgets::icon(glyph, px(14.0), color))
                .child(div().min_w_0().child(text.clone()))
                .into_any_element()
        }
        BlockBody::System(text) => div()
            .text_xs()
            .text_color(theme::text_muted())
            .child(text.clone())
            .into_any_element(),
        BlockBody::ProtocolCount(count) => div()
            .text_xs()
            .text_color(theme::text_subtle())
            .child(super::projection::protocol_label(*count))
            .into_any_element(),
    }
}

fn action_block(
    action: &ActionBlock,
    block: &ConsoleBlock,
    view: &RowView,
    key: &str,
    app: &WeakEntity<AppModel>,
) -> AnyElement {
    let (glyph, color, state) = state_badge(action);
    let mut column = div().flex().flex_col().gap_1().child(
        div()
            .flex()
            .items_center()
            .gap_2()
            .min_w_0()
            .child(crate::widgets::icon(
                tool_icon(action.kind),
                px(14.0),
                theme::text_muted(),
            ))
            .child(
                div()
                    .flex_shrink_0()
                    .text_sm()
                    .font_family(theme::FONT_UI)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text())
                    .child(action.name.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .font_family(theme::FONT_MONO)
                    .text_color(theme::text_muted())
                    .child(action.target.clone().unwrap_or_default()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(color)
                    .child(crate::widgets::icon(glyph, px(12.0), color))
                    .child(state),
            ),
    );
    if let Some(result) = action
        .result
        .as_ref()
        .filter(|result| !result.trim().is_empty())
    {
        let total = result.lines().count();
        let (shown, hidden) = if view.open {
            (result.clone(), 0)
        } else {
            preview(result, RESULT_PREVIEW_LINES)
        };
        column = column.child(
            div()
                .pl_6()
                .text_xs()
                .font_family(theme::FONT_MONO)
                .text_color(theme::text_muted())
                .child(shown),
        );
        if hidden > 0 || (view.open && total > RESULT_PREVIEW_LINES) {
            column = column.child(div().pl_6().child(toggle_link(
                key,
                &block.key,
                if view.open {
                    "Hide result".to_owned()
                } else {
                    format!("Show result ({total} lines)")
                },
                app,
            )));
        }
    }
    column.into_any_element()
}
