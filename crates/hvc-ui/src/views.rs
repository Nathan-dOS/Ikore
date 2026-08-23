use gpui::{AppContext as _, Context, Entity, SharedString};

use crate::{AppView, NativeEditor};

pub struct NavigationModel {
    pub active: AppView,
}

pub struct WorkspaceEditorModel {
    pub project_id: Option<SharedString>,
    pub root_path: Option<SharedString>,
    pub active_relative_path: Option<SharedString>,
    pub editor: Entity<NativeEditor>,
    pub open_tabs: usize,
}

impl WorkspaceEditorModel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            project_id: None,
            root_path: None,
            active_relative_path: None,
            editor: cx.new(NativeEditor::new),
            open_tabs: 0,
        }
    }
}

#[derive(Default)]
pub struct PulsationsModel {
    pub running_jobs: usize,
    pub awaiting_hitl: usize,
}

#[derive(Default)]
pub struct VaultModel {
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
