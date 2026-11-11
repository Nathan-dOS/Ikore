use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use gpui::{AppContext as _, Context, Entity, SharedString};
use ikore_core::AppCore;
use ikore_core::orchestration::brownfield::{
    BrownfieldConsentState, BrownfieldCoverageMetrics, BrownfieldInventory,
    BrownfieldOnboardingResult, BrownfieldOnboardingStatus, SetBrownfieldConsentRequest,
    SetBrownfieldIgnoredRequest,
};
use ikore_core::workspace_service::{
    FileNode, ReadWorkspaceFileRequest, WriteWorkspaceFileAtomicRequest,
};
use ikore_editor::{EditorSelection, EditorSnapshot, TextPosition};

use crate::NativeEditor;
use crate::errors::UiCommandError;

pub(crate) const AUTOSAVE_DEBOUNCE: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkspaceFileSnapshot {
    pub(crate) content: String,
    pub(crate) revision: String,
}

pub(crate) type WorkspaceOperationError = UiCommandError;

impl UiCommandError {
    pub(crate) fn is_conflict(&self) -> bool {
        self.is_code("conflict")
    }
}

pub(crate) type WorkspaceFuture<T> =
    Pin<Box<dyn Future<Output = Result<T, WorkspaceOperationError>> + Send + 'static>>;

pub(crate) trait WorkspaceBackend: Send + Sync {
    fn read(&self, relative_path: String) -> WorkspaceFuture<WorkspaceFileSnapshot>;

    fn write(
        &self,
        relative_path: String,
        content: String,
        expected_revision: String,
    ) -> WorkspaceFuture<String>;

    fn set_brownfield_consent(
        &self,
        project_id: String,
        consent: bool,
    ) -> WorkspaceFuture<BrownfieldOnboardingResult>;

    fn set_brownfield_ignored(
        &self,
        project_id: String,
        relative_path: String,
        ignored: bool,
    ) -> WorkspaceFuture<BrownfieldOnboardingStatus>;
}

pub(crate) struct CoreWorkspaceBackend {
    core: Arc<AppCore>,
}

impl CoreWorkspaceBackend {
    pub(crate) fn new(core: Arc<AppCore>) -> Self {
        Self { core }
    }
}

impl WorkspaceBackend for CoreWorkspaceBackend {
    fn read(&self, relative_path: String) -> WorkspaceFuture<WorkspaceFileSnapshot> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    core.read_workspace_file(ReadWorkspaceFileRequest { relative_path })
                        .map(|response| WorkspaceFileSnapshot {
                            content: response.content,
                            revision: response.revision,
                        })
                        .map_err(WorkspaceOperationError::from)
                })
                .await
                .map_err(|error| {
                    WorkspaceOperationError::worker(format!(
                        "workspace read worker interrupted: {error}"
                    ))
                })?
        })
    }

    fn write(
        &self,
        relative_path: String,
        content: String,
        expected_revision: String,
    ) -> WorkspaceFuture<String> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    core.write_workspace_file_atomic(WriteWorkspaceFileAtomicRequest {
                        relative_path,
                        content,
                        expected_revision,
                    })
                    .map(|response| response.revision)
                    .map_err(WorkspaceOperationError::from)
                })
                .await
                .map_err(|error| {
                    WorkspaceOperationError::worker(format!(
                        "workspace write worker interrupted: {error}"
                    ))
                })?
        })
    }

    fn set_brownfield_consent(
        &self,
        project_id: String,
        consent: bool,
    ) -> WorkspaceFuture<BrownfieldOnboardingResult> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    core.set_brownfield_onboarding_consent(SetBrownfieldConsentRequest {
                        project_id,
                        consent,
                    })
                    .map_err(WorkspaceOperationError::from)
                })
                .await
                .map_err(|error| {
                    WorkspaceOperationError::worker(format!(
                        "brownfield onboarding worker interrupted: {error}"
                    ))
                })?
        })
    }

    fn set_brownfield_ignored(
        &self,
        project_id: String,
        relative_path: String,
        ignored: bool,
    ) -> WorkspaceFuture<BrownfieldOnboardingStatus> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    core.set_brownfield_path_ignored(SetBrownfieldIgnoredRequest {
                        project_id,
                        relative_path,
                        ignored,
                    })
                    .map_err(WorkspaceOperationError::from)
                })
                .await
                .map_err(|error| {
                    WorkspaceOperationError::worker(format!(
                        "brownfield ignore worker interrupted: {error}"
                    ))
                })?
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum BrownfieldOnboardingState {
    #[default]
    NotApplicable,
    Pending,
    Disabled,
    Enabled,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct BrownfieldOnboardingModel {
    pub(crate) state: BrownfieldOnboardingState,
    pub(crate) busy: bool,
    pub(crate) error: Option<String>,
    pub(crate) inventory: Option<BrownfieldInventory>,
    pub(crate) ignored_paths: Vec<String>,
    pub(crate) coverage: Option<BrownfieldCoverageMetrics>,
}

impl BrownfieldOnboardingModel {
    pub(crate) fn from_status(status: Option<BrownfieldOnboardingStatus>) -> Self {
        let ignored_paths = status
            .as_ref()
            .map(|status| status.ignored_paths.clone())
            .unwrap_or_default();
        let coverage = status.as_ref().and_then(|status| status.coverage.clone());
        let state = status.map_or(
            BrownfieldOnboardingState::NotApplicable,
            |status| match status.consent {
                BrownfieldConsentState::NotDecided => BrownfieldOnboardingState::Pending,
                BrownfieldConsentState::Declined => BrownfieldOnboardingState::Disabled,
                BrownfieldConsentState::Granted => BrownfieldOnboardingState::Enabled,
            },
        );
        Self {
            state,
            ignored_paths,
            coverage,
            ..Self::default()
        }
    }

    pub(crate) fn apply_result(&mut self, result: BrownfieldOnboardingResult) {
        self.state = match result.status.consent {
            BrownfieldConsentState::NotDecided => BrownfieldOnboardingState::Pending,
            BrownfieldConsentState::Declined => BrownfieldOnboardingState::Disabled,
            BrownfieldConsentState::Granted => BrownfieldOnboardingState::Enabled,
        };
        self.busy = false;
        self.error = None;
        self.inventory = result.inventory;
        self.ignored_paths = result.status.ignored_paths;
        self.coverage = result.status.coverage;
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceTabModel {
    pub relative_path: SharedString,
    pub content: String,
    pub base_content: String,
    pub revision: String,
    pub dirty: bool,
    pub saving: bool,
    pub conflict: Option<String>,
    pub save_error: Option<String>,
    pub cursor_line: Option<usize>,
    pub cursor_column: Option<usize>,
    pub scroll_top: Option<f64>,
}

impl WorkspaceTabModel {
    pub(crate) fn new(
        relative_path: impl Into<SharedString>,
        content: String,
        revision: String,
    ) -> Self {
        Self {
            relative_path: relative_path.into(),
            base_content: content.clone(),
            content,
            revision,
            dirty: false,
            saving: false,
            conflict: None,
            save_error: None,
            cursor_line: None,
            cursor_column: None,
            scroll_top: None,
        }
    }

    /// What the tab strip, the conflict banner and the context drawer paint — everything but the
    /// document text, which is never cloned per frame.
    pub(crate) fn summary(&self) -> TabSummary {
        TabSummary {
            relative_path: self.relative_path.clone(),
            file_name: self.file_name().to_owned(),
            revision: self.revision.clone(),
            dirty: self.dirty,
            saving: self.saving,
            conflict: self.conflict.clone(),
            save_error: self.save_error.clone(),
        }
    }

    pub(crate) fn file_name(&self) -> &str {
        self.relative_path
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(self.relative_path.as_ref())
    }

    pub(crate) fn editor_snapshot(&self) -> EditorSnapshot {
        EditorSnapshot {
            text: self.content.clone(),
            revision: self.revision.clone(),
            selection: EditorSelection::caret(TextPosition {
                line: self.cursor_line.unwrap_or_default(),
                column: self.cursor_column.unwrap_or_default(),
            }),
            scroll_top: self.scroll_top.unwrap_or_default() as f32,
            dirty: self.dirty,
        }
    }

    fn apply_editor_snapshot(&mut self, snapshot: &EditorSnapshot) -> bool {
        let content_changed = self.content != snapshot.text;
        self.content.clone_from(&snapshot.text);
        self.dirty = self.content != self.base_content;
        self.cursor_line = Some(snapshot.selection.head.line);
        self.cursor_column = Some(snapshot.selection.head.column);
        self.scroll_top = Some(f64::from(snapshot.scroll_top));
        if content_changed {
            self.save_error = None;
        }
        content_changed
    }
}

/// Open editor tabs kept in memory at once. Every tab holds its text twice (live and last
/// saved), so the cap bounds the editor's footprint; opening one more closes the least recently
/// used tab that has nothing unsaved.
pub(crate) const MAX_OPEN_TABS: usize = 12;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TabSummary {
    pub relative_path: SharedString,
    pub file_name: String,
    pub revision: String,
    pub dirty: bool,
    pub saving: bool,
    pub conflict: Option<String>,
    pub save_error: Option<String>,
}

pub struct WorkspaceEditorModel {
    pub project_id: Option<SharedString>,
    pub root_path: Option<SharedString>,
    /// Canonicalized form of `root_path` (computed once when the workspace opens/restores),
    /// matching the `workspace_path` key the core emits on `IndexingEvent` — the core keys its
    /// embedding tables by `Path::canonicalize()`, which on Windows adds a `\\?\` extended-path
    /// prefix that `root_path` does not have. Plain `String` (not `SharedString`) since this is
    /// only ever compared against, never rendered.
    pub root_path_canonical: Option<String>,
    pub tree: Vec<FileNode>,
    pub active_relative_path: Option<SharedString>,
    pub editor: Entity<NativeEditor>,
    pub tabs: Vec<WorkspaceTabModel>,
    /// Paths in activation order, most recent last. Tabs missing here (restored directly into
    /// `tabs`) count as the least recently used.
    activation_order: Vec<SharedString>,
    pub context_drawer_open: bool,
    /// Shows the rendered Markdown instead of the editor; one or the other, never both.
    pub(crate) preview_mode: bool,
    pub(crate) folders: crate::file_tree::FolderState,
    pub(crate) brownfield: BrownfieldOnboardingModel,
}

impl WorkspaceEditorModel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            project_id: None,
            root_path: None,
            root_path_canonical: None,
            tree: Vec::new(),
            active_relative_path: None,
            editor: cx.new(NativeEditor::new),
            tabs: Vec::new(),
            activation_order: Vec::new(),
            context_drawer_open: true,
            preview_mode: false,
            folders: crate::file_tree::FolderState::default(),
            brownfield: BrownfieldOnboardingModel::default(),
        }
    }

    pub fn open_tab_count(&self) -> usize {
        self.tabs.len()
    }

    pub(crate) fn active_tab(&self) -> Option<&WorkspaceTabModel> {
        let path = self.active_relative_path.as_ref()?;
        self.tabs
            .iter()
            .find(|tab| tab.relative_path.as_ref() == path.as_ref())
    }

    pub(crate) fn active_tab_mut(&mut self) -> Option<&mut WorkspaceTabModel> {
        let path = self.active_relative_path.as_ref()?;
        self.tabs
            .iter_mut()
            .find(|tab| tab.relative_path.as_ref() == path.as_ref())
    }

    pub(crate) fn tab(&self, relative_path: &str) -> Option<&WorkspaceTabModel> {
        self.tabs
            .iter()
            .find(|tab| tab.relative_path.as_ref() == relative_path)
    }

    pub(crate) fn tab_mut(&mut self, relative_path: &str) -> Option<&mut WorkspaceTabModel> {
        self.tabs
            .iter_mut()
            .find(|tab| tab.relative_path.as_ref() == relative_path)
    }

    fn touch(&mut self, relative_path: &SharedString) {
        self.activation_order.retain(|path| path != relative_path);
        self.activation_order.push(relative_path.clone());
    }

    /// The tab a new one would replace at the cap: the least recently used tab that is not
    /// active and has nothing unsaved or in flight. `None` below the cap or when every tab
    /// holds unsaved work.
    pub(crate) fn eviction_candidate(&self) -> Option<SharedString> {
        if self.tabs.len() < MAX_OPEN_TABS {
            return None;
        }
        let rank = |path: &SharedString| {
            self.activation_order
                .iter()
                .position(|candidate| candidate == path)
                .map_or(0, |position| position + 1)
        };
        self.tabs
            .iter()
            .filter(|tab| !tab.dirty && !tab.saving)
            .filter(|tab| self.active_relative_path.as_ref() != Some(&tab.relative_path))
            .min_by_key(|tab| rank(&tab.relative_path))
            .map(|tab| tab.relative_path.clone())
    }

    /// Whether `relative_path` can open: it is already open, there is room, or a clean tab can
    /// make room.
    pub(crate) fn can_open(&self, relative_path: &str) -> bool {
        self.tab(relative_path).is_some()
            || self.tabs.len() < MAX_OPEN_TABS
            || self.eviction_candidate().is_some()
    }

    /// Opens (or re-activates) a tab. Returns the path of the tab it closed to stay within
    /// `MAX_OPEN_TABS`, if any, alongside whether a new tab was created.
    pub(crate) fn open_tab_evicting(
        &mut self,
        relative_path: impl Into<SharedString>,
        content: String,
        revision: String,
    ) -> (bool, Option<SharedString>) {
        let relative_path = relative_path.into();
        if self
            .tabs
            .iter()
            .any(|tab| tab.relative_path == relative_path)
        {
            self.touch(&relative_path);
            self.active_relative_path = Some(relative_path);
            return (false, None);
        }
        let evicted = self.eviction_candidate();
        if let Some(path) = &evicted {
            self.close_tab(path);
        }
        self.tabs.push(WorkspaceTabModel::new(
            relative_path.clone(),
            content,
            revision,
        ));
        self.touch(&relative_path);
        self.active_relative_path = Some(relative_path);
        (true, evicted)
    }

    /// Test convenience: opens without reporting which tab (if any) was evicted.
    #[cfg(test)]
    pub(crate) fn open_tab(
        &mut self,
        relative_path: impl Into<SharedString>,
        content: String,
        revision: String,
    ) -> bool {
        self.open_tab_evicting(relative_path, content, revision).0
    }

    pub(crate) fn select_tab(&mut self, relative_path: &str) -> bool {
        if self.tab(relative_path).is_none() {
            return false;
        }
        let path: SharedString = relative_path.to_owned().into();
        self.touch(&path);
        self.active_relative_path = Some(path);
        true
    }

    pub(crate) fn close_tab(&mut self, relative_path: &str) -> bool {
        let previous_len = self.tabs.len();
        self.tabs
            .retain(|tab| tab.relative_path.as_ref() != relative_path);
        if self.tabs.len() == previous_len {
            return false;
        }
        self.activation_order
            .retain(|path| path.as_ref() != relative_path);
        if self
            .active_relative_path
            .as_ref()
            .is_some_and(|active| active.as_ref() == relative_path)
        {
            self.active_relative_path = self.tabs.last().map(|tab| tab.relative_path.clone());
        }
        true
    }

    /// Caret and scroll only — the cheap path for editor notifications that did not edit.
    pub(crate) fn sync_active_cursor(&mut self, selection: EditorSelection, scroll_top: f32) {
        if let Some(tab) = self.active_tab_mut() {
            tab.cursor_line = Some(selection.head.line);
            tab.cursor_column = Some(selection.head.column);
            tab.scroll_top = Some(f64::from(scroll_top));
        }
    }

    pub(crate) fn sync_active_editor(&mut self, snapshot: &EditorSnapshot) -> bool {
        self.active_tab_mut()
            .is_some_and(|tab| tab.apply_editor_snapshot(snapshot))
    }

    pub(crate) fn mark_saving(&mut self, relative_path: &str) -> bool {
        let Some(tab) = self.tab_mut(relative_path) else {
            return false;
        };
        if !tab.dirty || tab.saving || tab.conflict.is_some() {
            return false;
        }
        tab.saving = true;
        tab.save_error = None;
        true
    }

    pub(crate) fn begin_conflict_resolution(&mut self, relative_path: &str) -> bool {
        let Some(tab) = self.tab_mut(relative_path) else {
            return false;
        };
        if tab.saving || tab.conflict.is_none() {
            return false;
        }
        tab.saving = true;
        tab.save_error = None;
        true
    }

    pub(crate) fn mark_saved(
        &mut self,
        relative_path: &str,
        saved_content: &str,
        revision: String,
    ) -> bool {
        let Some(tab) = self.tab_mut(relative_path) else {
            return false;
        };
        tab.base_content = saved_content.to_owned();
        tab.revision = revision;
        tab.dirty = tab.content != tab.base_content;
        tab.saving = false;
        tab.conflict = None;
        tab.save_error = None;
        tab.dirty
    }

    pub(crate) fn mark_save_failed(&mut self, relative_path: &str, error: WorkspaceOperationError) {
        let Some(tab) = self.tab_mut(relative_path) else {
            return;
        };
        tab.saving = false;
        if error.is_conflict() {
            tab.conflict = Some(error.message);
        } else {
            tab.save_error = Some(error.message);
        }
    }

    pub(crate) fn replace_from_disk(
        &mut self,
        relative_path: &str,
        snapshot: WorkspaceFileSnapshot,
    ) -> bool {
        let Some(tab) = self.tab_mut(relative_path) else {
            return false;
        };
        tab.content = snapshot.content.clone();
        tab.base_content = snapshot.content;
        tab.revision = snapshot.revision;
        tab.dirty = false;
        tab.saving = false;
        tab.conflict = None;
        tab.save_error = None;
        true
    }

    pub(crate) fn mark_external_change(&mut self, relative_path: &str, kind: &str) -> bool {
        let Some(tab) = self.tab_mut(relative_path) else {
            return false;
        };
        if kind == "removed" {
            tab.save_error = Some("The file was removed externally.".into());
            return false;
        }
        if tab.dirty || tab.saving {
            tab.saving = false;
            tab.conflict = Some(
                "External changes were detected while local edits exist. Reload or overwrite explicitly."
                    .into(),
            );
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tab_cap_tests {
    use super::*;
    use gpui::{AppContext, TestAppContext};

    #[gpui::test]
    fn the_tab_cap_closes_the_least_recently_used_clean_tab_and_never_unsaved_work(
        cx: &mut TestAppContext,
    ) {
        let workspace = cx.new(WorkspaceEditorModel::new);
        workspace.update(cx, |workspace, _| {
            for index in 0..MAX_OPEN_TABS {
                workspace.open_tab(format!("notes/{index}.md"), "text".into(), "r1".into());
            }
            assert_eq!(
                workspace.eviction_candidate().as_ref().map(AsRef::as_ref),
                Some("notes/0.md")
            );

            // Re-activating the oldest tab moves it to the back of the line.
            assert!(workspace.select_tab("notes/0.md"));
            assert_eq!(
                workspace.eviction_candidate().as_ref().map(AsRef::as_ref),
                Some("notes/1.md")
            );

            // Unsaved work is never a candidate.
            workspace.tab_mut("notes/1.md").unwrap().dirty = true;
            let (created, evicted) =
                workspace.open_tab_evicting("notes/new.md", "new".into(), "r1".into());
            assert!(created);
            assert_eq!(evicted.as_ref().map(AsRef::as_ref), Some("notes/2.md"));
            assert_eq!(workspace.tabs.len(), MAX_OPEN_TABS);
            assert!(workspace.tab("notes/1.md").is_some());

            // Every inactive tab dirty: nothing can make room, so nothing new may open.
            for tab in &mut workspace.tabs {
                tab.dirty = true;
            }
            assert!(workspace.eviction_candidate().is_none());
            assert!(!workspace.can_open("notes/another.md"));
            assert!(workspace.can_open("notes/new.md"), "already open");
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn opens_each_tab_once_and_activates_it(cx: &mut gpui::TestAppContext) {
        let workspace = cx.new(WorkspaceEditorModel::new);
        workspace.update(cx, |workspace, _| {
            assert!(workspace.open_tab("notes/one.md", "Initial".into(), "rev-1".into()));
            assert!(!workspace.open_tab("notes/one.md", "Ignored content".into(), "rev-2".into(),));
        });
        cx.read(|cx| {
            let workspace = workspace.read(cx);
            assert_eq!(workspace.tabs.len(), 1);
            assert_eq!(
                workspace
                    .active_relative_path
                    .as_ref()
                    .map(ToString::to_string),
                Some("notes/one.md".into())
            );
            assert_eq!(workspace.tabs[0].content, "Initial");
        });
    }

    #[gpui::test]
    fn clears_saving_conflict_and_error_after_save(cx: &mut gpui::TestAppContext) {
        let workspace = cx.new(WorkspaceEditorModel::new);
        workspace.update(cx, |workspace, _| {
            workspace.open_tab("notes/one.md", "Inicial".into(), "rev-1".into());
            let tab = workspace.active_tab_mut().unwrap();
            tab.content = "Salvo".into();
            tab.dirty = true;
            tab.saving = true;
            tab.conflict = Some("Conflito".into());
            tab.save_error = Some("Erro".into());
            workspace.mark_saved("notes/one.md", "Salvo", "rev-2".into());
        });
        cx.read(|cx| {
            let tab = &workspace.read(cx).tabs[0];
            assert_eq!(tab.base_content, "Salvo");
            assert_eq!(tab.revision, "rev-2");
            assert!(!tab.dirty);
            assert!(!tab.saving);
            assert!(tab.conflict.is_none());
            assert!(tab.save_error.is_none());
        });
    }

    #[gpui::test]
    fn activates_last_remaining_tab_after_close(cx: &mut gpui::TestAppContext) {
        let workspace = cx.new(WorkspaceEditorModel::new);
        workspace.update(cx, |workspace, _| {
            workspace.open_tab("notes/one.md", "one".into(), "r1".into());
            workspace.open_tab("notes/two.md", "two".into(), "r2".into());
            assert!(workspace.close_tab("notes/two.md"));
            assert_eq!(
                workspace
                    .active_relative_path
                    .as_ref()
                    .map(ToString::to_string),
                Some("notes/one.md".into())
            );
        });
    }

    #[gpui::test]
    fn preserves_new_edits_when_an_older_save_completes(cx: &mut gpui::TestAppContext) {
        let workspace = cx.new(WorkspaceEditorModel::new);
        workspace.update(cx, |workspace, _| {
            workspace.open_tab("notes/one.md", "one".into(), "r1".into());
            let tab = workspace.active_tab_mut().unwrap();
            tab.content = "first edit".into();
            tab.dirty = true;
            assert!(workspace.mark_saving("notes/one.md"));
            workspace.tab_mut("notes/one.md").unwrap().content = "second edit".into();
            assert!(workspace.mark_saved("notes/one.md", "first edit", "r2".into()));
            let tab = workspace.tab("notes/one.md").unwrap();
            assert_eq!(tab.base_content, "first edit");
            assert_eq!(tab.content, "second edit");
            assert!(tab.dirty);
        });
    }
}
