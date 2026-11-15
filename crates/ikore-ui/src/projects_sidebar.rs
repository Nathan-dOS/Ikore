//! The projects sidebar of the shell (ADR-042): every project and its recent durable work.
//!
//! For the projects that are not open it only reads: what runs there, what waits for the
//! developer. Opening a project is the only way to act on its work.

use std::collections::HashSet;

use gpui::{
    AnyElement, Context, FocusHandle, IntoElement, Pixels, SharedString, Subscription, Task,
    Window, div, prelude::*, px,
};
use ikore_core::persistence_service::{ProjectSessions, SessionEntry, SessionKind, SessionState};

use crate::app_model::{AppModel, AppView};
use crate::icons::Icon;
use crate::theme;

pub(crate) const PROJECTS_SIDEBAR_WIDTH: Pixels = px(240.0);
/// Window width under which the sidebar stays closed: beside the nav rail and the folded context
/// rail, the centre keeps its 660 px.
pub(crate) const PROJECTS_SIDEBAR_MIN_WINDOW: Pixels = px(1000.0);
/// Sessions of each kind listed under a project.
const SESSIONS_PER_KIND: usize = 5;

#[derive(Default)]
pub(crate) struct ProjectsSidebar {
    pub(crate) open: bool,
    pub(crate) projects: Vec<ProjectSessions>,
    pub(crate) error: Option<SharedString>,
    /// Projects the developer folded or unfolded by hand. The open project starts unfolded and
    /// the others folded.
    toggled: HashSet<String>,
    loading: bool,
    task: Option<Task<()>>,
    focus: Option<SidebarFocus>,
}

/// Where the focus of the sidebar lives, and where it goes when the sidebar loses it.
struct SidebarFocus {
    panel: FocusHandle,
    /// The rail button that opens the sidebar.
    trigger: FocusHandle,
    /// The focus was inside the sidebar when the shell was last drawn.
    owned: bool,
    _lost: Subscription,
}

impl ProjectsSidebar {
    pub(crate) fn unfolded(&self, project_id: &str, open_project: bool) -> bool {
        open_project != self.toggled.contains(project_id)
    }
}

/// The sidebar is open and the window has room for it.
pub(crate) fn sidebar_shown(model: &AppModel, window: &Window) -> bool {
    model.projects_sidebar.open && window.viewport_size().width >= PROJECTS_SIDEBAR_MIN_WINDOW
}

fn state_color(state: SessionState) -> gpui::Rgba {
    match state {
        SessionState::Waiting => theme::warning(),
        SessionState::Running => theme::running(),
        SessionState::Failed => theme::danger(),
        SessionState::Done => theme::success(),
        SessionState::Idle => theme::text_subtle(),
    }
}

fn state_label(state: SessionState) -> &'static str {
    match state {
        SessionState::Waiting => "waiting on you",
        SessionState::Running => "running",
        SessionState::Failed => "failed",
        SessionState::Done => "done",
        SessionState::Idle => "idle",
    }
}

fn kind_label(kind: SessionKind) -> &'static str {
    match kind {
        SessionKind::Pulsation => "Pulsation",
        SessionKind::Feature => "Feature",
        SessionKind::Conversation => "Conversation",
    }
}

/// What resting on a session says: its kind, where it stands and when it last changed.
fn session_summary(session: &SessionEntry, open_project: bool) -> String {
    let mut summary = format!(
        "{} · {} · {} · updated {}",
        kind_label(session.kind),
        session.detail,
        state_label(session.state),
        crate::time::format_timestamp(&session.updated_at)
    );
    if !open_project {
        summary.push_str(". Opens its project.");
    }
    summary
}

/// The sidebar element, or `None` while it is closed or the window is too narrow for it.
pub(crate) fn projects_sidebar(
    model: &AppModel,
    window: &Window,
    cx: &mut Context<AppModel>,
) -> Option<AnyElement> {
    if !sidebar_shown(model, window) {
        return None;
    }
    let sidebar = &model.projects_sidebar;
    let open_project = model.workspace.read(cx).project_id.clone();
    // What each view of the open project has selected: its row is marked.
    let selected = [
        model.pulsations.read(cx).active_pulsation_id.clone(),
        model.forge.selected_feature.clone().map(SharedString::from),
        model.chat.read(cx).active_conversation.clone(),
    ];
    let is_selected = |session: &SessionEntry| {
        let selected = match session.kind {
            SessionKind::Pulsation => &selected[0],
            SessionKind::Feature => &selected[1],
            SessionKind::Conversation => &selected[2],
        };
        selected.as_ref().map(AsRef::as_ref) == Some(session.id.as_str())
    };
    let mut list = div()
        .id("projects-sidebar-list")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_1();
    if sidebar.projects.is_empty() {
        list = list.child(
            div()
                .px_2()
                .text_sm()
                .text_color(theme::text_muted())
                .child(if sidebar.loading {
                    "Reading projects…"
                } else {
                    "No project yet. Open a workspace to start one."
                }),
        );
    }
    for entry in &sidebar.projects {
        let is_open = open_project.as_ref().map(AsRef::as_ref) == Some(entry.project.id.as_str());
        let unfolded = sidebar.unfolded(&entry.project.id, is_open);
        list = list.child(project_row(entry, is_open, unfolded, cx));
        if !unfolded {
            continue;
        }
        if entry.sessions.is_empty() {
            list = list.child(
                div()
                    .pl(px(28.0))
                    .py_1()
                    .text_xs()
                    .text_color(theme::text_subtle())
                    .child("No work yet."),
            );
        }
        for session in &entry.sessions {
            let selected = is_open && is_selected(session);
            list = list.child(session_row(entry, session, is_open, selected, cx));
        }
    }
    Some(
        div()
            .debug_selector(|| "projects-sidebar".into())
            .when_some(sidebar.focus.as_ref(), |panel, focus| {
                panel.track_focus(&focus.panel)
            })
            .w(PROJECTS_SIDEBAR_WIDTH)
            .flex_shrink_0()
            .min_h_0()
            .pb_2()
            .pr_2()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .px_2()
                    .pt_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme::text_subtle())
                            .child("PROJECTS"),
                    )
                    .child(crate::widgets::info_tip(
                        "projects-sidebar",
                        "Every project and its recent work: pulsations, Forge features and Companion conversations. A dot marks what is running or waiting on you. Only the open project can be changed; opening another one switches to it.",
                    )),
            )
            .when_some(sidebar.error.clone(), |panel, error| {
                panel.child(
                    div()
                        .px_2()
                        .text_xs()
                        .text_color(theme::danger())
                        .child(error),
                )
            })
            .child(list)
            .into_any_element(),
    )
}

fn project_row(
    entry: &ProjectSessions,
    open_project: bool,
    unfolded: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let id = entry.project.id.clone();
    let selector = format!("sidebar-project:{id}");
    let toggle_id = id.clone();
    let live = matches!(entry.state, SessionState::Waiting | SessionState::Running);
    let path = entry.project.workspace_path.clone();
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .tab_index(0)
        .w_full()
        .min_w_0()
        .px_2()
        .py_1()
        .rounded_md()
        .flex()
        .items_center()
        .gap_2()
        .cursor_pointer()
        .text_sm()
        .when(open_project, |row| {
            row.font_weight(gpui::FontWeight::MEDIUM)
        })
        .hover(|style| style.bg(theme::hover_bg()))
        .focus_visible(|style| style.bg(theme::accent_soft()))
        .tooltip(crate::widgets::text_tooltip(crate::widgets::display_path(
            &entry.project.workspace_path,
        )))
        .on_click(cx.listener(move |model, _, _, cx| {
            model.toggle_sidebar_project(toggle_id.clone(), cx);
        }))
        .child(crate::widgets::tree_row_label(
            Some(unfolded),
            entry.project.name.clone(),
            if open_project {
                theme::text()
            } else {
                theme::text_muted()
            },
        ))
        .when(live, |row| {
            let state_id = format!("sidebar-project-state:{id}");
            row.child(
                div()
                    .debug_selector(move || state_id.clone())
                    .child(crate::widgets::status_dot(state_color(entry.state))),
            )
        })
        .when(!open_project, |row| {
            let open_id = format!("sidebar-open:{id}");
            let selector = open_id.clone();
            row.child(
                div()
                    .id(SharedString::from(open_id))
                    .debug_selector(move || selector.clone())
                    .tab_index(0)
                    .flex_shrink_0()
                    .size(px(22.0))
                    .rounded_md()
                    .flex()
                    .items_center()
                    .justify_center()
                    .hover(|style| style.bg(theme::surface_hover()))
                    .focus_visible(|style| style.bg(theme::accent_soft()))
                    .tooltip(crate::widgets::text_tooltip("Open this project"))
                    .on_click(cx.listener(move |model, _, _, cx| {
                        model.open_sidebar_project(path.clone(), cx);
                        cx.stop_propagation();
                    }))
                    .child(crate::widgets::icon(
                        Icon::FolderOpen,
                        px(13.0),
                        theme::text_muted(),
                    )),
            )
        })
        .into_any_element()
}

fn session_row(
    entry: &ProjectSessions,
    session: &SessionEntry,
    open_project: bool,
    selected: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let selector = format!("sidebar-session:{}", session.id);
    let glyph = match session.kind {
        SessionKind::Pulsation => AppView::Pulsations.icon(),
        SessionKind::Feature => AppView::Forge.icon(),
        SessionKind::Conversation => AppView::Chat.icon(),
    };
    let project_id = entry.project.id.clone();
    let (kind, id) = (session.kind, session.id.clone());
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .tab_index(0)
        .w_full()
        .min_w_0()
        .pl(px(28.0))
        .pr_2()
        .py_1()
        .rounded_md()
        .flex()
        .items_center()
        .gap_2()
        .cursor_pointer()
        .text_sm()
        .when(selected, |row| row.bg(theme::surface_hover()))
        .hover(|style| style.bg(theme::hover_bg()))
        .focus_visible(|style| style.bg(theme::accent_soft()))
        .tooltip(crate::widgets::text_tooltip(session_summary(
            session,
            open_project,
        )))
        .on_click(cx.listener(move |model, _, _, cx| {
            model.open_sidebar_session(&project_id, kind, id.clone(), cx);
        }))
        .child(crate::widgets::icon(glyph, px(13.0), theme::text_subtle()))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_color(if open_project {
                    theme::text()
                } else {
                    theme::text_muted()
                })
                .child(session.title.clone()),
        )
        .when(session.state != SessionState::Idle, |row| {
            row.child(crate::widgets::status_dot(state_color(session.state)))
        })
        .into_any_element()
}

impl AppModel {
    /// Keeps the focus in the shell when the sidebar, or a row of it, goes away with the focus
    /// inside: the shell shortcuts only reach a focused control, so the one that reopens the
    /// sidebar would stop working. A sidebar that closed gives its focus to the rail button that
    /// opens it, whose handle this returns.
    pub(crate) fn observe_projects_sidebar_focus(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> FocusHandle {
        let focus = self
            .projects_sidebar
            .focus
            .get_or_insert_with(|| SidebarFocus {
                panel: cx.focus_handle(),
                trigger: cx.focus_handle().tab_index(0).tab_stop(true),
                owned: false,
                _lost: cx.on_focus_lost(window, |model, window, _| {
                    let shown = sidebar_shown(model, window);
                    let Some(focus) = model.projects_sidebar.focus.as_mut() else {
                        return;
                    };
                    if !std::mem::take(&mut focus.owned) {
                        return;
                    }
                    if shown {
                        focus.panel.focus(window);
                    } else {
                        focus.trigger.focus(window);
                    }
                }),
            });
        focus.owned = focus.panel.contains_focused(window, cx);
        focus.trigger.clone()
    }

    pub(crate) fn toggle_projects_sidebar(&mut self, cx: &mut Context<Self>) {
        self.projects_sidebar.open = !self.projects_sidebar.open;
        self.refresh_projects_sidebar(cx);
        cx.notify();
    }

    /// Reads the recent sessions of every project. It runs only while the sidebar is open, on
    /// the poll the shell already has, and draws again only when something changed.
    pub(crate) fn refresh_projects_sidebar(&mut self, cx: &mut Context<Self>) {
        if !self.projects_sidebar.open || self.projects_sidebar.loading {
            return;
        }
        let Some(core) = self.core.clone() else {
            return;
        };
        self.projects_sidebar.loading = true;
        let runtime = core.runtime_handle().clone();
        self.projects_sidebar.task = Some(cx.spawn(async move |this, cx| {
            let read = runtime
                .spawn_blocking(move || {
                    core.persistence()
                        .project_sessions(SESSIONS_PER_KIND)
                        .map_err(|error| error.to_string())
                })
                .await;
            let _ = this.update(cx, |model, cx| {
                let sidebar = &mut model.projects_sidebar;
                sidebar.loading = false;
                let (projects, error) = match read {
                    Ok(Ok(projects)) => (Some(projects), None),
                    Ok(Err(error)) => (None, Some(SharedString::from(error))),
                    Err(error) => (None, Some(SharedString::from(error.to_string()))),
                };
                let changed = sidebar.error != error
                    || projects
                        .as_ref()
                        .is_some_and(|projects| *projects != sidebar.projects);
                sidebar.error = error;
                if let Some(projects) = projects {
                    sidebar.projects = projects;
                }
                if changed {
                    cx.notify();
                }
            });
        }));
    }

    pub(crate) fn toggle_sidebar_project(&mut self, project_id: String, cx: &mut Context<Self>) {
        let toggled = &mut self.projects_sidebar.toggled;
        if !toggled.remove(&project_id) {
            toggled.insert(project_id);
        }
        cx.notify();
    }

    /// Switches to another project, through the same path as choosing its folder.
    pub(crate) fn open_sidebar_project(&mut self, workspace_path: String, cx: &mut Context<Self>) {
        self.open_selected_workspace(workspace_path, cx);
    }

    /// Goes to a session of the open project. A session of another project opens that project:
    /// its work is not reachable from this one.
    pub(crate) fn open_sidebar_session(
        &mut self,
        project_id: &str,
        kind: SessionKind,
        id: String,
        cx: &mut Context<Self>,
    ) {
        let open_project = self
            .workspace
            .read(cx)
            .project_id
            .as_ref()
            .map(AsRef::as_ref)
            == Some(project_id);
        if !open_project {
            let path = self
                .projects_sidebar
                .projects
                .iter()
                .find(|entry| entry.project.id == project_id)
                .map(|entry| entry.project.workspace_path.clone());
            if let Some(path) = path {
                self.open_sidebar_project(path, cx);
            }
            return;
        }
        match kind {
            SessionKind::Pulsation => {
                self.select_pulsation(id, cx);
                self.select_view(AppView::Pulsations, cx);
            }
            SessionKind::Feature => {
                self.select_view(AppView::Forge, cx);
                self.forge_action(crate::forge::ForgeAction::SelectFeature(id), cx);
            }
            SessionKind::Conversation => {
                self.select_chat_conversation(id, cx);
                self.select_view(AppView::Chat, cx);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_open_project_starts_unfolded_and_the_others_folded() {
        let mut sidebar = ProjectsSidebar::default();
        assert!(sidebar.unfolded("open", true));
        assert!(!sidebar.unfolded("other", false));
        // A fold by hand is remembered on both sides.
        sidebar.toggled.insert("open".into());
        sidebar.toggled.insert("other".into());
        assert!(!sidebar.unfolded("open", true));
        assert!(sidebar.unfolded("other", false));
    }

    #[test]
    fn a_session_says_where_it_stands_and_that_another_project_opens() {
        let session = SessionEntry {
            kind: SessionKind::Pulsation,
            id: "pulse".into(),
            title: "Add the report".into(),
            detail: "test".into(),
            state: SessionState::Waiting,
            updated_at: "now".into(),
        };
        assert_eq!(
            session_summary(&session, true),
            "Pulsation · test · waiting on you · updated now"
        );
        assert_eq!(
            session_summary(&session, false),
            "Pulsation · test · waiting on you · updated now. Opens its project."
        );
    }
}
