use crate::AppView;
use crate::app_model::AppTab;

pub struct NavigationModel {
    pub active: AppView,
    pub active_tab: AppTab,
    pub sidebar_collapsed: bool,
}
