use gpui::{AppContext as _, Context, Entity, SharedString};

use crate::{AppView, NativeEditor};

pub struct NavigationModel {
    pub active: AppView,
    pub sidebar_collapsed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceViewMode {
    Editor,
    Hitl,
}

impl WorkspaceViewMode {
    pub const fn id(self) -> &'static str {
        match self {
            Self::Editor => "editor",
            Self::Hitl => "hitl",
        }
    }

    pub fn from_id(id: &str) -> Self {
        match id {
            "hitl" => Self::Hitl,
            _ => Self::Editor,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceTabModel {
    pub relative_path: SharedString,
    pub content: String,
    pub base_content: String,
    pub revision: String,
    pub dirty: bool,
    pub cursor_line: Option<usize>,
    pub cursor_column: Option<usize>,
    pub scroll_top: Option<f64>,
    pub load_error: Option<String>,
}

pub struct WorkspaceEditorModel {
    pub project_id: Option<SharedString>,
    pub root_path: Option<SharedString>,
    pub active_relative_path: Option<SharedString>,
    pub editor: Entity<NativeEditor>,
    pub tabs: Vec<WorkspaceTabModel>,
    pub view_mode: WorkspaceViewMode,
    pub context_drawer_open: bool,
    pub diff_expanded: bool,
}

impl WorkspaceEditorModel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            project_id: None,
            root_path: None,
            active_relative_path: None,
            editor: cx.new(NativeEditor::new),
            tabs: Vec::new(),
            view_mode: WorkspaceViewMode::Editor,
            context_drawer_open: true,
            diff_expanded: false,
        }
    }

    pub fn open_tab_count(&self) -> usize {
        self.tabs.len()
    }
}

#[derive(Default)]
pub struct PulsationsModel {
    pub active_pulsation_id: Option<SharedString>,
    pub running_jobs: usize,
    pub awaiting_hitl: usize,
}

#[derive(Default)]
pub struct VaultModel {
    pub selected_path: Option<SharedString>,
    pub indexed_notes: usize,
    pub stale: bool,
}

#[derive(Default)]
pub struct ChatModel {
    pub active_conversation: Option<SharedString>,
    pub streaming: bool,
}

#[derive(Default)]
pub struct SettingsModel {
    pub configured_providers: usize,
}

#[derive(Default)]
pub struct LogsModel {
    pub visible_entries: usize,
}
