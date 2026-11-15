//! The work panel (ADR-042, item 6): the right-hand column of the shell, in tabs. General keeps
//! the context cards; Files and Search reach the open project's files from any view. The panel
//! never opens or changes tab by itself (item 7): only the developer does.

use gpui::{
    AnyElement, Context, Entity, FocusHandle, Focusable, IntoElement, KeyDownEvent, SharedString,
    Subscription, Task, Window, div, prelude::*, px,
};
use ikore_core::workspace_service::{SearchMatch, SearchMode, SearchWorkspaceTextRequest};

use crate::app_model::{AppModel, AppView};
use crate::file_tree::TreeRow;
use crate::text_input::NativeTextInput;
use crate::theme;

/// Matches the panel keeps: a narrow column reads a short list, and a longer one says so.
const MAX_RESULTS: usize = 100;
pub(crate) const SEARCH_PLACEHOLDER: &str = "Search the project";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum WorkPanelTab {
    #[default]
    General,
    Files,
    Search,
}

impl WorkPanelTab {
    pub(crate) const ALL: [Self; 3] = [Self::General, Self::Files, Self::Search];

    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Files => "files",
            Self::Search => "search",
        }
    }

    /// The tab a saved session names; `None` for one this build does not know.
    pub(crate) fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tab| tab.id() == id)
    }

    fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Files => "Files",
            Self::Search => "Search",
        }
    }

    fn tip(self) -> &'static str {
        match self {
            Self::General => "The pipeline, what waits on you, the next steps and runtime health",
            Self::Files => "The files of the open project. A file opens in Workspace.",
            Self::Search => "Text search over every file the editor opens",
        }
    }
}

/// The panel's own state. It lives in memory: the session keeps its fold, not its tab.
#[derive(Default)]
pub(crate) struct WorkPanel {
    pub(crate) tab: WorkPanelTab,
    search: Option<WorkSearch>,
    focus: Option<PanelFocus>,
}

impl WorkPanel {
    pub(crate) fn search_input(&self) -> Option<Entity<NativeTextInput>> {
        self.search.as_ref().map(|search| search.input.clone())
    }

    /// The panel's own focus, which takes over from a control of the panel that goes away.
    pub(crate) fn focus_handle(&self) -> Option<&FocusHandle> {
        self.focus.as_ref().map(|focus| &focus.panel)
    }
}

struct PanelFocus {
    panel: FocusHandle,
    /// The focus was inside the panel when the shell was last drawn.
    owned: bool,
    _lost: Subscription,
}

struct WorkSearch {
    input: Entity<NativeTextInput>,
    /// The project the results belong to: after a project switch they are not shown.
    root: Option<SharedString>,
    /// The query of the results shown; the input may have moved past it.
    query: String,
    matches: Vec<SearchMatch>,
    scanned_files: usize,
    truncated: bool,
    searching: bool,
    error: Option<SharedString>,
    task: Option<Task<()>>,
}

impl WorkSearch {
    /// The results were found in the project open now.
    fn belongs_to(&self, root: Option<&SharedString>) -> bool {
        root.is_some() && self.root.as_ref() == root
    }
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

/// The tabs, in the panel's header.
pub(crate) fn tab_strip(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let active = model.work_panel.tab;
    div()
        .flex()
        .items_center()
        .gap(px(2.0))
        .children(WorkPanelTab::ALL.into_iter().map(|tab| {
            let selector = format!("work-tab:{}", tab.id());
            let selected = tab == active;
            div()
                .id(SharedString::from(selector.clone()))
                .debug_selector(move || selector.clone())
                .tab_index(0)
                .px_2()
                .py(px(3.0))
                .rounded_md()
                .text_xs()
                .font_weight(gpui::FontWeight::MEDIUM)
                .cursor_pointer()
                // Backgrounds only in the refinements: one that sets the text colour replaces the
                // whole text style in GPUI and the label would lose its size.
                .text_color(if selected {
                    theme::text()
                } else {
                    theme::text_subtle()
                })
                .when(selected, |tab| tab.bg(theme::surface_hover()))
                .when(!selected, |tab| {
                    tab.hover(|style| style.bg(theme::hover_bg()))
                })
                .focus_visible(|style| style.bg(theme::accent_soft()))
                .tooltip(crate::widgets::text_tooltip(tab.tip()))
                .on_click(cx.listener(move |model, _, window, cx| {
                    model.select_work_panel_tab(tab, window, cx);
                }))
                .child(tab.label())
        }))
        .into_any_element()
}

/// The project's tree. It shares the folders' open state with the Workspace explorer: one tree.
pub(crate) fn files_tab(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let (rows, capped, active, has_root) = {
        let workspace = model.workspace.read(cx);
        (
            crate::file_tree::flatten(&workspace.tree, &workspace.folders),
            crate::file_tree::is_capped(&workspace.tree),
            workspace
                .active_relative_path
                .as_ref()
                .map(ToString::to_string),
            workspace.root_path.is_some(),
        )
    };
    let list = div()
        .id("work-files")
        .debug_selector(|| "work-files".into())
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px_2()
        .pb_2()
        .flex()
        .flex_col();
    if !has_root {
        return list.child(note("No project is open.")).into_any_element();
    }
    if rows.is_empty() {
        return list
            .child(note("This project has no file the editor opens."))
            .into_any_element();
    }
    list.children(
        rows.into_iter()
            .map(|row| file_row(row, active.as_deref(), cx)),
    )
    .when(capped, |list| {
        list.child(crate::widgets::tree_capped_notice())
    })
    .into_any_element()
}

fn file_row(row: TreeRow, active: Option<&str>, cx: &mut Context<AppModel>) -> AnyElement {
    let TreeRow {
        depth,
        name,
        relative_path: path,
        is_dir,
        expanded,
    } = row;
    let selector = format!("work-file:{path}");
    let is_active = !is_dir && active == Some(path.as_str());
    let tip = path.clone();
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .tab_index(0)
        .w_full()
        .min_w_0()
        .flex_shrink_0()
        .pl(px(4.0 + depth as f32 * 12.0))
        .pr_1()
        .py(px(3.0))
        .rounded_md()
        .flex()
        .items_center()
        .text_sm()
        .cursor_pointer()
        .when(is_active, |row| row.bg(theme::accent_soft()))
        .when(!is_active, |row| {
            row.hover(|style| style.bg(theme::hover_bg()))
        })
        .focus_visible(|style| style.bg(theme::accent_soft()))
        .tooltip(crate::widgets::text_tooltip(tip))
        .on_click(cx.listener(move |model, _, _, cx| {
            if is_dir {
                model.toggle_workspace_folder(&path, cx);
            } else {
                model.open_workspace_file(path.clone(), cx);
            }
        }))
        .child(crate::widgets::tree_row_label(
            is_dir.then_some(expanded),
            name,
            if is_active {
                theme::accent()
            } else {
                theme::text_muted()
            },
        ))
        .into_any_element()
}

/// The search field and its results. Enter searches; a result opens its file at the match.
pub(crate) fn search_tab(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let Some(search) = model.work_panel.search.as_ref() else {
        return note("Choose the Search tab to search the project.");
    };
    let shown = search.belongs_to(model.workspace.read(cx).root_path.as_ref());
    let status = if search.searching {
        Some(note("Searching…"))
    } else if let Some(error) = search.error.clone().filter(|_| shown) {
        Some(
            div()
                .px_2()
                .text_xs()
                .text_color(theme::danger())
                .child(error)
                .into_any_element(),
        )
    } else if shown && !search.query.is_empty() {
        Some(note(format!(
            "{} result(s) in {} file(s){}",
            search.matches.len(),
            search.scanned_files,
            if search.truncated {
                format!(" · the first {MAX_RESULTS} are shown")
            } else {
                String::new()
            }
        )))
    } else {
        Some(note(
            "Enter searches the text of every file the editor opens.",
        ))
    };
    let results = if shown && !search.searching {
        search
            .matches
            .iter()
            .enumerate()
            .map(|(index, matched)| result_row(index, matched, cx))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .id("work-search-field")
                .debug_selector(|| "work-search-field".into())
                .px_2()
                .w_full()
                .min_w_0()
                .on_key_down(cx.listener(|model, event: &KeyDownEvent, _, cx| {
                    if event.keystroke.key == "enter" && !event.keystroke.modifiers.modified() {
                        model.search_work_panel(cx);
                        cx.stop_propagation();
                    }
                }))
                .child(search.input.clone()),
        )
        .children(status)
        .child(
            div()
                .id("work-search-results")
                .debug_selector(|| "work-search-results".into())
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .px_2()
                .pb_2()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .children(results),
        )
        .into_any_element()
}

fn result_row(index: usize, matched: &SearchMatch, cx: &mut Context<AppModel>) -> AnyElement {
    let selector = format!("work-result:{index}");
    let path = matched.relative_path.clone();
    let name = path.rsplit('/').next().unwrap_or(&path).to_string();
    let cursor = ikore_editor::TextPosition {
        line: matched.line.saturating_sub(1),
        column: matched.column.saturating_sub(1),
    };
    let tip = format!("{path} · line {}, column {}", matched.line, matched.column);
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .tab_index(0)
        .w_full()
        .min_w_0()
        .flex_shrink_0()
        .px_2()
        .py_1()
        .rounded_md()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .cursor_pointer()
        .hover(|style| style.bg(theme::hover_bg()))
        .focus_visible(|style| style.bg(theme::accent_soft()))
        .tooltip(crate::widgets::text_tooltip(tip))
        .on_click(cx.listener(move |model, _, _, cx| {
            model.open_workspace_file_at(path.clone(), Some(cursor), cx);
        }))
        .child(
            div()
                .min_w_0()
                .flex()
                .gap_1()
                .text_xs()
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_color(theme::text())
                        .child(name),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_color(theme::text_subtle())
                        .child(format!(":{}", matched.line)),
                ),
        )
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .font_family(theme::FONT_MONO)
                .text_xs()
                .text_color(theme::text_muted())
                .child(matched.line_text.trim().to_string()),
        )
        .into_any_element()
}

impl AppModel {
    /// Keeps the focus in the shell when the control that had it inside the panel goes away: a
    /// fold, a tab change, a strip button replaced by the open panel. The shell shortcuts only
    /// reach a focused control, so the one that unfolds the panel again would stop working.
    pub(crate) fn observe_work_panel_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focus = self.work_panel.focus.get_or_insert_with(|| PanelFocus {
            panel: cx.focus_handle(),
            owned: false,
            _lost: cx.on_focus_lost(window, |model, window, _| {
                let home = model.active_view() == AppView::Home;
                let Some(focus) = model.work_panel.focus.as_mut() else {
                    return;
                };
                // Home has no panel to hold the focus.
                if std::mem::take(&mut focus.owned) && !home {
                    focus.panel.focus(window);
                }
            }),
        });
        focus.owned = focus.panel.contains_focused(window, cx);
    }

    /// The developer picks a tab. Search takes the focus, so typing goes to its field.
    pub(crate) fn select_work_panel_tab(
        &mut self,
        tab: WorkPanelTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let changed = self.work_panel.tab != tab;
        self.work_panel.tab = tab;
        if tab == WorkPanelTab::Search {
            let input = self.ensure_work_search(cx);
            input.read(cx).focus_handle(cx).focus(window);
        }
        if changed {
            // The project's session remembers the tab for the next launch.
            self.schedule_session_persistence(cx);
        }
        cx.notify();
    }

    /// Opens the panel on `tab` from its folded strip or a shortcut. Home has no panel, so the
    /// shortcut goes to Workspace first; a window too narrow for the panel says so instead.
    pub(crate) fn open_work_panel_tab(
        &mut self,
        tab: WorkPanelTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_view() == AppView::Home {
            self.select_view(AppView::Workspace, cx);
        }
        let (_, forced) = crate::context_rail::effective_collapsed(self, window, cx);
        if forced {
            self.push_header_notice(
                crate::session::HeaderNoticeKind::Notice,
                format!(
                    "Widen the window to at least {} px to open the work panel.",
                    f32::from(crate::sizing::CONTEXT_RAIL_AUTO_COLLAPSE_BELOW)
                ),
                cx,
            );
            return;
        }
        if self.context_rail_collapsed(cx) {
            self.set_context_rail_collapsed(false, cx);
        }
        self.select_work_panel_tab(tab, window, cx);
    }

    fn ensure_work_search(&mut self, cx: &mut Context<Self>) -> Entity<NativeTextInput> {
        if let Some(input) = self.work_panel.search_input() {
            return input;
        }
        let input = cx.new(|cx| NativeTextInput::new(SEARCH_PLACEHOLDER, cx));
        self.work_panel.search = Some(WorkSearch {
            input: input.clone(),
            root: None,
            query: String::new(),
            matches: Vec::new(),
            scanned_files: 0,
            truncated: false,
            searching: false,
            error: None,
            task: None,
        });
        input
    }

    /// Searches the open project for the field's text. An empty field clears the results.
    pub(crate) fn search_work_panel(&mut self, cx: &mut Context<Self>) {
        let root = self.workspace.read(cx).root_path.clone();
        let backend = self.vault_backend.clone();
        let Some(search) = self.work_panel.search.as_mut() else {
            return;
        };
        let query = search.input.read(cx).value().trim().to_owned();
        search.task = None;
        search.searching = false;
        search.error = None;
        search.matches.clear();
        search.scanned_files = 0;
        search.truncated = false;
        search.query.clone_from(&query);
        search.root = root;
        if query.is_empty() {
            cx.notify();
            return;
        }
        let Some(backend) = backend else {
            search.error = Some("Search is not available without a project.".into());
            cx.notify();
            return;
        };
        search.searching = true;
        let request = SearchWorkspaceTextRequest {
            query,
            mode: SearchMode::Substring,
            case_sensitive: false,
            max_results: MAX_RESULTS,
        };
        search.task = Some(cx.spawn(async move |this, cx| {
            let result = backend.search(request).await;
            let _ = this.update(cx, |model, cx| {
                if let Some(search) = model.work_panel.search.as_mut() {
                    search.searching = false;
                    match result {
                        Ok(response) => {
                            search.matches = response.matches;
                            search.scanned_files = response.scanned_files;
                            search.truncated = response.truncated;
                        }
                        Err(error) => search.error = Some(error.into()),
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::{Arc, Mutex};

    use gpui::TestAppContext;
    use ikore_core::workspace_service::{
        FileNode, GetWorkspaceVaultIndexResponse, RenameWorkspaceNoteRequest,
        RenameWorkspaceNoteResponse, SearchWorkspaceTextResponse,
    };

    use super::*;
    use crate::vault::{VaultBackend, VaultFuture};

    /// Answers every search with one match per query word, in `src/{word}.rs`, and records the
    /// queries.
    #[derive(Default)]
    pub(crate) struct SearchOnly {
        pub(crate) queries: Mutex<Vec<(String, usize)>>,
    }

    impl VaultBackend for SearchOnly {
        fn tree(&self) -> VaultFuture<Vec<FileNode>> {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn index(&self, _: bool) -> VaultFuture<GetWorkspaceVaultIndexResponse> {
            Box::pin(async { Err("not used".to_string()) })
        }

        fn search(
            &self,
            request: SearchWorkspaceTextRequest,
        ) -> VaultFuture<SearchWorkspaceTextResponse> {
            self.queries
                .lock()
                .unwrap()
                .push((request.query.clone(), request.max_results));
            let matches = request
                .query
                .split_whitespace()
                .enumerate()
                .map(|(index, word)| SearchMatch {
                    relative_path: format!("src/{word}.rs"),
                    line: index + 3,
                    column: 5,
                    line_text: format!("    let {word} = 1;"),
                })
                .collect();
            Box::pin(async move {
                Ok(SearchWorkspaceTextResponse {
                    matches,
                    truncated: false,
                    scanned_files: 7,
                })
            })
        }

        fn read(&self, _: String) -> VaultFuture<String> {
            Box::pin(async { Err("not used".to_string()) })
        }

        fn rename(
            &self,
            _: RenameWorkspaceNoteRequest,
        ) -> VaultFuture<RenameWorkspaceNoteResponse> {
            Box::pin(async { Err("not used".to_string()) })
        }
    }

    #[gpui::test]
    fn the_search_tab_searches_the_open_project_and_not_another_one(cx: &mut TestAppContext) {
        let backend = Arc::new(SearchOnly::default());
        let model = cx.new(|cx| {
            let mut model = AppModel::new_for_test(cx);
            model.vault_backend = Some(backend.clone());
            model.workspace.update(cx, |workspace, _| {
                workspace.root_path = Some("C:/work/one".into());
            });
            model
        });
        model.update(cx, |model, cx| {
            let input = model.ensure_work_search(cx);
            input.update(cx, |input, cx| input.set_value("  alpha beta ", cx));
            model.search_work_panel(cx);
            assert!(model.work_panel.search.as_ref().unwrap().searching);
        });
        cx.run_until_parked();
        assert_eq!(
            *backend.queries.lock().unwrap(),
            vec![("alpha beta".to_string(), MAX_RESULTS)]
        );
        model.read_with(cx, |model, _| {
            let search = model.work_panel.search.as_ref().unwrap();
            assert!(!search.searching);
            assert_eq!(search.query, "alpha beta");
            assert_eq!(search.scanned_files, 7);
            let paths: Vec<_> = search
                .matches
                .iter()
                .map(|matched| matched.relative_path.as_str())
                .collect();
            assert_eq!(paths, ["src/alpha.rs", "src/beta.rs"]);
            // The results belong to the project they were found in, not to the next one.
            assert!(search.belongs_to(Some(&"C:/work/one".into())));
            assert!(!search.belongs_to(Some(&"C:/work/two".into())));
            assert!(!search.belongs_to(None));
        });

        // An empty field clears the results without asking the backend.
        model.update(cx, |model, cx| {
            let input = model.work_panel.search_input().unwrap();
            input.update(cx, |input, cx| input.set_value("   ", cx));
            model.search_work_panel(cx);
        });
        cx.run_until_parked();
        assert_eq!(backend.queries.lock().unwrap().len(), 1);
        model.read_with(cx, |model, _| {
            let search = model.work_panel.search.as_ref().unwrap();
            assert!(search.matches.is_empty() && search.query.is_empty());
        });
    }
}
