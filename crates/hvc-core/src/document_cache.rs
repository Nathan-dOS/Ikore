//! AppCore/runtime share parsed documents by canonical workspace, never by relative path alone.
use crate::workspace_service::{VaultIndexCache, WorkspaceServiceError};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

type RootCaches = HashMap<PathBuf, Arc<Mutex<VaultIndexCache>>>;

#[derive(Clone, Default)]
pub struct SharedDocumentCache {
    roots: Arc<Mutex<RootCaches>>,
}

impl SharedDocumentCache {
    pub(crate) fn with<T>(
        &self,
        root: &Path,
        action: impl FnOnce(&mut VaultIndexCache) -> Result<T, WorkspaceServiceError>,
    ) -> Result<T, WorkspaceServiceError> {
        let root = root.canonicalize()?;
        let cache = {
            let mut roots = self
                .roots
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if roots.len() >= 8 && !roots.contains_key(&root) {
                // Do not evict active users and create competing caches for the same root.
                if let Some(idle) = roots
                    .iter()
                    .find(|(_, cache)| Arc::strong_count(cache) == 1)
                    .map(|(path, _)| path.clone())
                {
                    roots.remove(&idle);
                }
            }
            roots.entry(root).or_default().clone()
        };
        let mut cache = cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        action(&mut cache)
    }
}
