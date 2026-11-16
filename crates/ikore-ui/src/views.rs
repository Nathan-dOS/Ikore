use crate::AppView;

/// Shell navigation state mirrored from `AppModel` for observers.
pub struct NavigationModel {
    pub active: AppView,
    /// The right-hand context rail is folded to its icon strip. Persisted through the session's
    /// legacy `sidebar_collapsed` column (the left sidebar it named no longer exists).
    pub context_rail_collapsed: bool,
}
