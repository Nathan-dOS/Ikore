use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use gpui::{AppContext as _, Context, Entity, SharedString};
use hvc_core::AppCore;
use hvc_core::orchestration::brownfield::{
    BrownfieldConsentState, BrownfieldCoverageMetrics, BrownfieldInventory,
    BrownfieldOnboardingResult, BrownfieldOnboardingStatus, SetBrownfieldConsentRequest,
    SetBrownfieldIgnoredRequest,
};
use hvc_core::workspace_service::{
    FileNode, ReadWorkspaceFileRequest, WriteWorkspaceFileAtomicRequest,
};
use hvc_editor::{EditorSelection, EditorSnapshot, TextPosition};

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
                        "worker de leitura do workspace interrompido: {error}"
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
                        "worker de escrita do workspace interrompido: {error}"
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
                        "worker do onboarding brownfield interrompido: {error}"
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
                        "worker de ignore brownfield interrompido: {error}"
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

pub struct WorkspaceEditorModel {
    pub project_id: Option<SharedString>,
    pub root_path: Option<SharedString>,
    pub tree: Vec<FileNode>,
    pub active_relative_path: Option<SharedString>,
    pub editor: Entity<NativeEditor>,
    pub tabs: Vec<WorkspaceTabModel>,
    pub view_mode: WorkspaceViewMode,
    pub context_drawer_open: bool,
    pub diff_expanded: bool,
    pub(crate) brownfield: BrownfieldOnboardingModel,
}

impl WorkspaceEditorModel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            project_id: None,
            root_path: None,
            tree: Vec::new(),
            active_relative_path: None,
            editor: cx.new(NativeEditor::new),
            tabs: Vec::new(),
            view_mode: WorkspaceViewMode::Editor,
            context_drawer_open: true,
            diff_expanded: false,
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

    pub(crate) fn open_tab(
        &mut self,
        relative_path: impl Into<SharedString>,
        content: String,
        revision: String,
    ) -> bool {
        let relative_path = relative_path.into();
        if self
            .tabs
            .iter()
            .any(|tab| tab.relative_path == relative_path)
        {
            self.active_relative_path = Some(relative_path);
            return false;
        }
        self.tabs.push(WorkspaceTabModel::new(
            relative_path.clone(),
            content,
            revision,
        ));
        self.active_relative_path = Some(relative_path);
        true
    }

    pub(crate) fn select_tab(&mut self, relative_path: &str) -> bool {
        if self.tab(relative_path).is_none() {
            return false;
        }
        self.active_relative_path = Some(relative_path.to_owned().into());
        true
    }

    pub(crate) fn close_tab(&mut self, relative_path: &str) -> bool {
        let previous_len = self.tabs.len();
        self.tabs
            .retain(|tab| tab.relative_path.as_ref() != relative_path);
        if self.tabs.len() == previous_len {
            return false;
        }
        if self
            .active_relative_path
            .as_ref()
            .is_some_and(|active| active.as_ref() == relative_path)
        {
            self.active_relative_path = self.tabs.last().map(|tab| tab.relative_path.clone());
        }
        true
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
            tab.save_error = Some("O arquivo foi removido externamente.".into());
            return false;
        }
        if tab.dirty || tab.saving {
            tab.saving = false;
            tab.conflict = Some(
                "Mudanças externas foram detectadas enquanto havia edições locais. Recarregue ou sobrescreva explicitamente."
                    .into(),
            );
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn opens_each_tab_once_and_activates_it(cx: &mut gpui::TestAppContext) {
        let workspace = cx.new(WorkspaceEditorModel::new);
        workspace.update(cx, |workspace, _| {
            assert!(workspace.open_tab("notes/one.md", "Inicial".into(), "rev-1".into()));
            assert!(!workspace.open_tab(
                "notes/one.md",
                "Conteúdo ignorado".into(),
                "rev-2".into(),
            ));
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
            assert_eq!(workspace.tabs[0].content, "Inicial");
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
