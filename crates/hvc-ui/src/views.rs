use gpui::SharedString;

use crate::AppView;
use crate::hitl::HitlReviewState;

pub struct NavigationModel {
    pub active: AppView,
    pub sidebar_collapsed: bool,
}

#[derive(Default)]
pub struct PulsationsModel {
    pub active_pulsation_id: Option<SharedString>,
    pub running_jobs: usize,
    pub awaiting_hitl: usize,
    pub(crate) hitl: HitlReviewState,
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
