use gpui::SharedString;

use crate::AppView;

pub struct NavigationModel {
    pub active: AppView,
    pub sidebar_collapsed: bool,
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
