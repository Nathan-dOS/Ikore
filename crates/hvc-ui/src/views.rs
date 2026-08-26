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
