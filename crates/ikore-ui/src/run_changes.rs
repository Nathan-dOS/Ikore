//! What a stage run published (PI-Desktop PD6 with Reticle P1): the card at
//! the end of a finished run's console and the work panel's Diff tab, which
//! compares each file with the text the publication replaced.

use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyElement, Context, IntoElement, SharedString, Window, div, prelude::*, px, uniform_list,
};
use ikore_core::persistence_service::{PublishedBefore, PublishedChange};
use ikore_core::terminals::TerminalRun;
use ikore_editor::{DiffMark, DiffRow, build_diff_rows, diff_stats};

use crate::app_model::AppModel;
use crate::theme;
use crate::work_panel::WorkPanelTab;

/// Unchanged lines kept around each change in the Diff tab.
const CONTEXT_LINES: usize = 3;
/// Files the card lists before pointing at the Diff tab.
const CARD_FILES: usize = 3;
/// While a run's publications are still landing, the card reads them again
/// this often, at most this many times.
const SETTLE_INTERVAL: Duration = Duration::from_millis(1500);
const SETTLE_ATTEMPTS: u8 = 20;
/// The height of one line in the Diff tab.
const LINE_HEIGHT: f32 = 18.0;

/// One file a run's job published, as the card lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunFileChange {
    pub(crate) artifact_id: String,
    pub(crate) relative_path: String,
    pub(crate) added: usize,
    pub(crate) removed: usize,
    /// Why the file cannot be compared, when it cannot.
    pub(crate) note: Option<&'static str>,
    /// The publication has not landed yet.
    pub(crate) pending: bool,
}

/// What the UI holds of a run's changes.
#[derive(Debug, Clone)]
pub(crate) enum RunChanges {
    Loading,
    Loaded(Arc<Vec<RunFileChange>>),
    Failed,
}

/// One line of a file's change, unified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DiffLine {
    Context(String),
    Removed(String),
    Added(String),
    /// Unchanged lines left out.
    Skipped(usize),
}

/// One published file against the text it replaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileChangeDiff {
    pub(crate) relative_path: String,
    pub(crate) lines: Vec<DiffLine>,
    pub(crate) added: usize,
    pub(crate) removed: usize,
    /// Why there is nothing to compare, when there is not.
    pub(crate) note: Option<&'static str>,
}

/// The Diff tab: one run's files and the one compared.
pub(crate) struct RunDiff {
    pub(crate) project_id: String,
    pub(crate) title: String,
    pub(crate) files: Arc<Vec<RunFileChange>>,
    pub(crate) selected: String,
    pub(crate) view: DiffView,
}

pub(crate) enum DiffView {
    Loading,
    Loaded(Arc<FileChangeDiff>),
    Failed(String),
}

/// The text a change replaced, or why it cannot be compared.
fn replaced(change: &PublishedChange) -> Result<&str, &'static str> {
    match &change.before {
        PublishedBefore::Absent => Ok(""),
        PublishedBefore::Text(text) => Ok(text),
        PublishedBefore::NotApplied => Err(match change.publication_state.as_str() {
            "conflict" => "Not published: the file changed outside the run",
            "cancelled" => "Not published: cancelled",
            _ => "Not published yet",
        }),
        PublishedBefore::Unavailable(reason) => Err(match reason.as_str() {
            "too_large" => "The replaced text was over 1 MiB",
            "over_budget" => "The project's 64 MiB of replaced texts was full",
            "replayed" => "The replaced text was lost in a restart",
            "purged" => "The replaced text was removed",
            "not_recorded" => "Published before replaced texts were kept",
            _ => "The replaced text could not be read",
        }),
    }
}

/// The card's line for one published file.
pub(crate) fn run_file_change(change: &PublishedChange) -> RunFileChange {
    let pending = matches!(change.publication_state.as_str(), "pending" | "applying");
    let (added, removed, note) = match replaced(change) {
        Ok(before) => {
            let stats = diff_stats(before, &change.after);
            (stats.added, stats.removed, None)
        }
        Err(note) => (0, 0, Some(note)),
    };
    RunFileChange {
        artifact_id: change.artifact_id.clone(),
        relative_path: change.relative_path.clone(),
        added,
        removed,
        note,
        pending,
    }
}

/// One published file as the Diff tab shows it.
pub(crate) fn file_change_diff(change: &PublishedChange) -> FileChangeDiff {
    match replaced(change) {
        Ok(before) => {
            let stats = diff_stats(before, &change.after);
            FileChangeDiff {
                relative_path: change.relative_path.clone(),
                lines: unified(&build_diff_rows(before, &change.after), CONTEXT_LINES),
                added: stats.added,
                removed: stats.removed,
                note: None,
            }
        }
        Err(note) => FileChangeDiff {
            relative_path: change.relative_path.clone(),
            lines: Vec::new(),
            added: 0,
            removed: 0,
            note: Some(note),
        },
    }
}

/// Side-by-side rows as unified lines: in each changed run, the removed lines
/// then the added ones; unchanged lines only within `context` of a change.
pub(crate) fn unified(rows: &[DiffRow], context: usize) -> Vec<DiffLine> {
    let mut flat = Vec::with_capacity(rows.len());
    let mut removed = Vec::new();
    let mut added = Vec::new();
    for row in rows {
        if row.left.mark == DiffMark::Neutral {
            flat.append(&mut removed);
            flat.append(&mut added);
            flat.push(DiffLine::Context(row.right.text.clone()));
            continue;
        }
        if row.left.line.is_some() {
            removed.push(DiffLine::Removed(row.left.text.clone()));
        }
        if row.right.line.is_some() {
            added.push(DiffLine::Added(row.right.text.clone()));
        }
    }
    flat.append(&mut removed);
    flat.append(&mut added);
    let mut shown = vec![false; flat.len()];
    for (index, line) in flat.iter().enumerate() {
        if !matches!(line, DiffLine::Context(_)) {
            let end = (index + context).min(flat.len() - 1);
            shown[index.saturating_sub(context)..=end].fill(true);
        }
    }
    let mut lines = Vec::new();
    let mut skipped = 0;
    for (line, shown) in flat.into_iter().zip(shown) {
        if shown {
            if skipped > 0 {
                lines.push(DiffLine::Skipped(skipped));
                skipped = 0;
            }
            lines.push(line);
        } else {
            skipped += 1;
        }
    }
    if skipped > 0 {
        lines.push(DiffLine::Skipped(skipped));
    }
    lines
}

impl AppModel {
    /// Reads what a finished stage run published, once per run; while its
    /// publications are still landing, reads them again a few times.
    pub(crate) fn load_run_changes(&mut self, run: &TerminalRun, cx: &mut Context<Self>) {
        let Some(job_id) = run.job_id.clone() else {
            return;
        };
        if !run.status.is_terminal() || self.agents.read(cx).run_changes.contains_key(&run.id) {
            return;
        }
        self.fetch_run_changes(
            run.project_id.clone(),
            run.id.clone(),
            job_id,
            SETTLE_ATTEMPTS,
            cx,
        );
    }

    fn fetch_run_changes(
        &mut self,
        project_id: String,
        run_id: String,
        job_id: String,
        attempts: u8,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.agents_backend.clone() else {
            return;
        };
        // A run read again keeps its files on screen meanwhile.
        self.agents.update(cx, |agents, _| {
            agents
                .run_changes
                .entry(run_id.clone())
                .or_insert(RunChanges::Loading);
        });
        cx.spawn(async move |this, cx| {
            let result = backend
                .run_changes(project_id.clone(), job_id.clone())
                .await;
            let settling = matches!(&result, Ok(files) if files.iter().any(|file| file.pending));
            let _ = this.update(cx, |model, cx| {
                model.agents.update(cx, |agents, _| {
                    agents.run_changes.insert(
                        run_id.clone(),
                        match result {
                            Ok(files) => RunChanges::Loaded(Arc::new(files)),
                            Err(_) => RunChanges::Failed,
                        },
                    );
                });
                cx.notify();
            });
            if settling && attempts > 1 {
                cx.background_executor().timer(SETTLE_INTERVAL).await;
                let _ = this.update(cx, |model, cx| {
                    model.fetch_run_changes(project_id, run_id, job_id, attempts - 1, cx);
                });
            }
        })
        .detach();
    }

    /// Opens the Diff tab on one file of a run's changes.
    pub(crate) fn open_run_change(
        &mut self,
        project_id: String,
        title: String,
        files: Arc<Vec<RunFileChange>>,
        artifact_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.work_panel.diff = Some(RunDiff {
            project_id,
            title,
            files,
            selected: artifact_id.clone(),
            view: DiffView::Loading,
        });
        self.open_work_panel_tab(WorkPanelTab::Diff, window, cx);
        self.load_file_change(artifact_id, cx);
    }

    /// Compares another file of the Diff tab's run.
    pub(crate) fn select_run_change(&mut self, artifact_id: String, cx: &mut Context<Self>) {
        let Some(diff) = self.work_panel.diff.as_mut() else {
            return;
        };
        if diff.selected == artifact_id {
            return;
        }
        diff.selected = artifact_id.clone();
        diff.view = DiffView::Loading;
        self.load_file_change(artifact_id, cx);
        cx.notify();
    }

    fn load_file_change(&mut self, artifact_id: String, cx: &mut Context<Self>) {
        let (Some(backend), Some(diff)) =
            (self.agents_backend.clone(), self.work_panel.diff.as_ref())
        else {
            return;
        };
        let project_id = diff.project_id.clone();
        cx.spawn(async move |this, cx| {
            let result = backend.file_change(project_id, artifact_id.clone()).await;
            let _ = this.update(cx, |model, cx| {
                // A file chosen meanwhile wins over this answer.
                let Some(diff) = model
                    .work_panel
                    .diff
                    .as_mut()
                    .filter(|diff| diff.selected == artifact_id)
                else {
                    return;
                };
                diff.view = match result {
                    Ok(Some(file)) => DiffView::Loaded(Arc::new(file)),
                    Ok(None) => DiffView::Failed("This change is no longer recorded.".into()),
                    Err(error) => DiffView::Failed(error.message),
                };
                cx.notify();
            });
        })
        .detach();
    }
}

/// What a click on the card opens: one file of the run in the Diff tab.
struct OpenChange {
    project_id: String,
    title: String,
    files: Arc<Vec<RunFileChange>>,
    artifact_id: String,
}

impl OpenChange {
    fn listener(
        self,
        cx: &mut Context<AppModel>,
    ) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static {
        cx.listener(move |model, _, window, cx| {
            model.open_run_change(
                self.project_id.clone(),
                self.title.clone(),
                Arc::clone(&self.files),
                self.artifact_id.clone(),
                window,
                cx,
            );
        })
    }
}

fn stats(added: usize, removed: usize) -> AnyElement {
    div()
        .flex_shrink_0()
        .flex()
        .gap_1()
        .font_family(theme::FONT_MONO)
        .child(
            div()
                .text_color(theme::success())
                .child(format!("+{added}")),
        )
        .child(
            div()
                .text_color(theme::danger())
                .child(format!("−{removed}")),
        )
        .into_any_element()
}

/// The card at the end of a finished stage run's console: the files its job
/// published, with the lines each added and removed. A file opens in the
/// work panel's Diff tab.
pub(crate) fn changes_card(
    key: &str,
    agent_label: &str,
    run: &TerminalRun,
    files: &Arc<Vec<RunFileChange>>,
    cx: &mut Context<AppModel>,
) -> Option<AnyElement> {
    if files.is_empty() {
        return None;
    }
    let added: usize = files.iter().map(|file| file.added).sum();
    let removed: usize = files.iter().map(|file| file.removed).sum();
    let mut summary = format!(
        "Changes this run · {} file{}",
        files.len(),
        if files.len() == 1 { "" } else { "s" }
    );
    if files.iter().any(|file| file.pending) {
        summary.push_str(" · publishing…");
    }
    let title = match crate::agent_dock_view::run_time(run) {
        Some(time) => format!("{agent_label} · {time}"),
        None => agent_label.to_owned(),
    };
    let open = |artifact_id: String| OpenChange {
        project_id: run.project_id.clone(),
        title: title.clone(),
        files: Arc::clone(files),
        artifact_id,
    };
    let rows = files
        .iter()
        .take(CARD_FILES)
        .enumerate()
        .map(|(index, file)| {
            let selector = format!("agent-console-change:{key}:{index}");
            let tip = match file.note {
                Some(note) => format!("{} · {note}", file.relative_path),
                None => file.relative_path.clone(),
            };
            div()
                .id(SharedString::from(selector.clone()))
                .debug_selector(move || selector.clone())
                .flex()
                .items_center()
                .gap_2()
                .px_1()
                .rounded_sm()
                .cursor_pointer()
                .hover(|style| style.bg(theme::hover_bg()))
                .tooltip(crate::widgets::text_tooltip(tip))
                .on_click(open(file.artifact_id.clone()).listener(cx))
                .child(match file.note {
                    Some(_) => div()
                        .flex_shrink_0()
                        .text_color(theme::text_subtle())
                        .child("—")
                        .into_any_element(),
                    None => stats(file.added, file.removed),
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(theme::text())
                        .child(file.relative_path.clone()),
                )
                .into_any_element()
        })
        .collect::<Vec<_>>();
    let more = (files.len() > CARD_FILES).then(|| {
        let hidden = files.len() - CARD_FILES;
        let selector = format!("agent-console-changes-more:{key}");
        div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .px_1()
            .text_color(theme::accent())
            .cursor_pointer()
            .hover(|style| style.bg(theme::hover_bg()))
            .on_click(open(files[CARD_FILES].artifact_id.clone()).listener(cx))
            .child(format!("{hidden} more in the Diff tab"))
    });
    let selector = format!("agent-console-changes:{key}");
    Some(
        div()
            .debug_selector(move || selector.clone())
            .flex_shrink_0()
            .px_2()
            .py_1()
            .border_t_1()
            .border_color(theme::border())
            .flex()
            .flex_col()
            .gap(px(2.0))
            .text_xs()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme::text_muted())
                            .child(summary),
                    )
                    .child(stats(added, removed)),
            )
            .children(rows)
            .children(more)
            .into_any_element(),
    )
}

fn note(text: impl Into<SharedString>) -> AnyElement {
    div()
        .px_2()
        .py_1()
        .text_xs()
        .text_color(theme::text_muted())
        .child(text.into())
        .into_any_element()
}

/// The work panel's Diff tab: the run's files, then the chosen one as
/// unified lines against what it replaced.
pub(crate) fn diff_tab(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let Some(diff) = model.work_panel.diff.as_ref() else {
        return note(
            "Choose a file under “Changes this run”, at the end of an agent's console, to compare it with what it replaced.",
        );
    };
    let files = diff.files.iter().enumerate().map(|(index, file)| {
        let selector = format!("work-diff-file:{index}");
        let selected = file.artifact_id == diff.selected;
        let artifact_id = file.artifact_id.clone();
        div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .tab_index(0)
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .py(px(2.0))
            .rounded_md()
            .text_xs()
            .cursor_pointer()
            .when(selected, |row| row.bg(theme::accent_soft()))
            .when(!selected, |row| {
                row.hover(|style| style.bg(theme::hover_bg()))
            })
            .focus_visible(|style| style.bg(theme::accent_soft()))
            .tooltip(crate::widgets::text_tooltip(file.relative_path.clone()))
            .on_click(cx.listener(move |model, _, _, cx| {
                model.select_run_change(artifact_id.clone(), cx);
            }))
            .child(match file.note {
                Some(_) => div()
                    .flex_shrink_0()
                    .text_color(theme::text_subtle())
                    .child("—")
                    .into_any_element(),
                None => stats(file.added, file.removed),
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme::text())
                    .child(file.relative_path.clone()),
            )
    });
    let body = match &diff.view {
        DiffView::Loading => note("Reading the change…"),
        DiffView::Failed(message) => div()
            .px_2()
            .text_xs()
            .text_color(theme::danger())
            .child(message.clone())
            .into_any_element(),
        DiffView::Loaded(file) => match file.note {
            Some(reason) => note(reason),
            None if file.added == 0 && file.removed == 0 => note("No line changed."),
            None => {
                let file = Arc::clone(file);
                uniform_list(
                    "work-diff-lines",
                    file.lines.len(),
                    cx.processor(move |_, range: std::ops::Range<usize>, _, _| {
                        range
                            .filter_map(|index| file.lines.get(index).map(diff_line))
                            .collect::<Vec<_>>()
                    }),
                )
                .flex_1()
                .min_h_0()
                .px_2()
                .into_any_element()
            }
        },
    };
    div()
        .id("work-diff")
        .debug_selector(|| "work-diff".into())
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .gap_1()
        .child(note(diff.title.clone()))
        .child(
            div()
                .id("work-diff-files")
                .flex_shrink_0()
                .max_h(px(140.0))
                .overflow_y_scroll()
                .px_1()
                .flex()
                .flex_col()
                .children(files),
        )
        .child(div().mx_2().h(px(1.0)).flex_shrink_0().bg(theme::border()))
        .child(body)
        .into_any_element()
}

fn diff_line(line: &DiffLine) -> AnyElement {
    let (marker, text, color, background) = match line {
        DiffLine::Context(text) => (" ", text.clone(), theme::text_muted(), None),
        DiffLine::Removed(text) => ("-", text.clone(), theme::text(), Some(theme::danger_soft())),
        DiffLine::Added(text) => (
            "+",
            text.clone(),
            theme::text(),
            Some(theme::success_soft()),
        ),
        DiffLine::Skipped(count) => (
            "⋯",
            format!(
                "{count} unchanged line{}",
                if *count == 1 { "" } else { "s" }
            ),
            theme::text_subtle(),
            None,
        ),
    };
    div()
        .h(px(LINE_HEIGHT))
        .flex()
        .items_center()
        .gap_1()
        .px_1()
        .font_family(theme::FONT_MONO)
        .text_xs()
        .text_color(color)
        .when_some(background, |line, background| line.bg(background))
        .child(div().flex_shrink_0().w(px(10.0)).child(marker))
        .child(div().flex_1().min_w_0().truncate().child(text))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(before: PublishedBefore, after: &str, state: &str) -> PublishedChange {
        PublishedChange {
            artifact_id: "artifact".into(),
            relative_path: "src/lib.rs".into(),
            publication_state: state.into(),
            before,
            after: after.into(),
        }
    }

    /// PD6: the card counts the lines a file added and removed; a new file
    /// adds all of its lines; a change that cannot be compared says why.
    #[test]
    fn the_card_counts_lines_or_says_why_it_cannot() {
        let edited = run_file_change(&change(
            PublishedBefore::Text("a\nb\nc\n".into()),
            "a\nB\nc\nd\n",
            "applied",
        ));
        assert_eq!((edited.added, edited.removed, edited.note), (2, 1, None));
        let created = run_file_change(&change(PublishedBefore::Absent, "x\ny\n", "applied"));
        assert_eq!((created.added, created.removed), (2, 0));
        let pending = run_file_change(&change(PublishedBefore::NotApplied, "x", "pending"));
        assert!(pending.pending);
        assert_eq!(pending.note, Some("Not published yet"));
        let conflict = run_file_change(&change(PublishedBefore::NotApplied, "x", "conflict"));
        assert!(!conflict.pending);
        assert_eq!(
            conflict.note,
            Some("Not published: the file changed outside the run")
        );
        let old = run_file_change(&change(
            PublishedBefore::Unavailable("not_recorded".into()),
            "x",
            "applied",
        ));
        assert_eq!(old.note, Some("Published before replaced texts were kept"));
    }

    /// The Diff tab shows removed lines before added ones in a changed run,
    /// keeps three unchanged lines around changes and folds the rest.
    #[test]
    fn unified_lines_keep_context_and_fold_the_rest() {
        let before: String = (1..=20).map(|index| format!("line {index}\n")).collect();
        let after = before.replace("line 10\n", "line ten\n");
        let diff = file_change_diff(&change(PublishedBefore::Text(before), &after, "applied"));
        assert_eq!((diff.added, diff.removed), (1, 1));
        let expected = [
            DiffLine::Skipped(6),
            DiffLine::Context("line 7".into()),
            DiffLine::Context("line 8".into()),
            DiffLine::Context("line 9".into()),
            DiffLine::Removed("line 10".into()),
            DiffLine::Added("line ten".into()),
            DiffLine::Context("line 11".into()),
            DiffLine::Context("line 12".into()),
            DiffLine::Context("line 13".into()),
            DiffLine::Skipped(7),
        ];
        assert_eq!(diff.lines, expected);
        let missing = file_change_diff(&change(
            PublishedBefore::Unavailable("too_large".into()),
            "x",
            "applied",
        ));
        assert!(missing.lines.is_empty());
        assert_eq!(missing.note, Some("The replaced text was over 1 MiB"));
    }
}
