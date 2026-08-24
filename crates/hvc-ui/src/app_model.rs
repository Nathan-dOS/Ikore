use std::collections::HashMap;
use std::sync::Arc;

use gpui::{
    AppContext as _, Context, Entity, PathPromptOptions, SharedString, Subscription, Task, Window,
};
use hvc_core::persistence_service::{
    ApplyHitlDecisionRequest, EnsureProjectForWorkspaceRequest, GetPulsationRuntimeSnapshotRequest,
    HitlDecision, LoadUiSessionRequest, PulsationStatus, SaveUiSessionRequest, UiSessionState,
    UiTabSnapshot,
};
use hvc_core::workspace_service::{
    FileNode, OpenWorkspaceRequest, ReadWorkspaceFileRequest, WorkspaceChangedEvent,
};
use hvc_core::{AppCore, AppEvent};
use hvc_editor::{EditorSelection, EditorSnapshot, TextPosition};
use tokio::sync::broadcast::error::RecvError;

use crate::lifecycle::LifecycleState;
use crate::session::{
    CoreSessionPersistence, HeaderNotice, HeaderNoticeKind, HeaderNoticeQueue, SESSION_DEBOUNCE,
    SessionPersistence,
};
use crate::views::{
    ChatModel, LogsModel, NavigationModel, PulsationsModel, SettingsModel, VaultModel,
};
use crate::workspace::{
    AUTOSAVE_DEBOUNCE, CoreWorkspaceBackend, WorkspaceBackend, WorkspaceEditorModel,
    WorkspaceTabModel, WorkspaceViewMode,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppView {
    Workspace,
    Pulsations,
    Vault,
    Chat,
    Settings,
    Logs,
}

impl AppView {
    pub const ALL: [Self; 6] = [
        Self::Workspace,
        Self::Pulsations,
        Self::Vault,
        Self::Chat,
        Self::Settings,
        Self::Logs,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Pulsations => "pulsations",
            Self::Vault => "vault",
            Self::Chat => "chat",
            Self::Settings => "settings",
            Self::Logs => "logs",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Workspace => "Workspace & HITL",
            Self::Pulsations => "Pulsations & Pipeline",
            Self::Vault => "Vault & Graph",
            Self::Chat => "Chat / Companion",
            Self::Settings => "Settings & AI Runtime",
            Self::Logs => "Logs & Observability",
        }
    }

    pub const fn short_label(self) -> &'static str {
        match self {
            Self::Workspace => "Workspace",
            Self::Pulsations => "Pulsations",
            Self::Vault => "Vault",
            Self::Chat => "Companion",
            Self::Settings => "Settings",
            Self::Logs => "Logs",
        }
    }

    pub const fn glyph(self) -> &'static str {
        match self {
            Self::Workspace => "W",
            Self::Pulsations => "P",
            Self::Vault => "V",
            Self::Chat => "C",
            Self::Settings => "S",
            Self::Logs => "L",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::Workspace => "Arquivos, abas, editor, preview e revisão HITL",
            Self::Pulsations => "Execuções, checkpoints e decisões humanas",
            Self::Vault => "Notas Markdown, links e contexto do workspace",
            Self::Chat => "Conversa, streaming e ferramentas read-only",
            Self::Settings => "Provedores, rotas LLM e preferências locais",
            Self::Logs => "Eventos tipados e diagnósticos do runtime",
        }
    }

    pub fn from_id(id: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|view| view.id() == id)
            .unwrap_or(Self::Workspace)
    }
}

#[derive(Debug)]
struct LoadedTab {
    relative_path: String,
    content: String,
    base_content: String,
    revision: String,
    dirty: bool,
    cursor_line: Option<usize>,
    cursor_column: Option<usize>,
    scroll_top: Option<f64>,
    load_error: Option<String>,
}

struct RestoredSession {
    project_id: String,
    root_path: String,
    tree: Vec<FileNode>,
    session: Option<UiSessionState>,
    tabs: Vec<LoadedTab>,
    notice: Option<String>,
}

pub struct AppModel {
    core: Option<Arc<AppCore>>,
    session_persistence: Option<Arc<dyn SessionPersistence>>,
    workspace_backend: Option<Arc<dyn WorkspaceBackend>>,
    pub navigation: Entity<NavigationModel>,
    pub workspace: Entity<WorkspaceEditorModel>,
    pub pulsations: Entity<PulsationsModel>,
    pub vault: Entity<VaultModel>,
    pub chat: Entity<ChatModel>,
    pub settings: Entity<SettingsModel>,
    pub logs: Entity<LogsModel>,
    active_view: AppView,
    status: SharedString,
    event_count: u64,
    needs_resync: bool,
    notices: HeaderNoticeQueue,
    lifecycle: LifecycleState,
    hydrating: bool,
    session_generation: u64,
    session_dirty: bool,
    workspace_save_generations: HashMap<String, u64>,
    suppress_editor_sync: bool,
    event_task: Option<Task<()>>,
    polling_task: Option<Task<()>>,
    workspace_task: Option<Task<()>>,
    file_tasks: HashMap<String, Task<()>>,
    autosave_tasks: HashMap<String, Task<()>>,
    hitl_task: Option<Task<()>>,
    session_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl AppModel {
    pub fn new(core: Arc<AppCore>, cx: &mut Context<Self>) -> Self {
        let session_persistence: Arc<dyn SessionPersistence> =
            Arc::new(CoreSessionPersistence::new(Arc::clone(&core)));
        let workspace_backend: Arc<dyn WorkspaceBackend> =
            Arc::new(CoreWorkspaceBackend::new(Arc::clone(&core)));
        let mut model = Self::build(
            Some(core),
            Some(session_persistence),
            Some(workspace_backend),
            cx,
        );
        model.start_lifecycle(cx);
        cx.on_app_quit(|model, cx| model.flush_pending_session(cx))
            .detach();
        model.restore_last_session(cx);
        model
    }

    #[cfg(test)]
    pub fn new_for_test(cx: &mut Context<Self>) -> Self {
        Self::build(None, None, None, cx)
    }

    #[cfg(test)]
    fn new_for_test_with_persistence(
        persistence: Arc<dyn SessionPersistence>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::build(None, Some(persistence), None, cx)
    }

    #[cfg(test)]
    fn new_for_test_with_workspace_backend(
        backend: Arc<dyn WorkspaceBackend>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::build(None, None, Some(backend), cx)
    }

    fn build(
        core: Option<Arc<AppCore>>,
        session_persistence: Option<Arc<dyn SessionPersistence>>,
        workspace_backend: Option<Arc<dyn WorkspaceBackend>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let active_view = AppView::Workspace;
        let navigation = cx.new(|_| NavigationModel {
            active: active_view,
            sidebar_collapsed: false,
        });
        let workspace = cx.new(WorkspaceEditorModel::new);
        let editor = workspace.read(cx).editor.clone();
        let subscriptions = vec![cx.observe(&editor, |model, _, cx| {
            model.handle_editor_update(cx);
            model.schedule_session_persistence(cx);
        })];
        Self {
            core,
            session_persistence,
            workspace_backend,
            navigation,
            workspace,
            pulsations: cx.new(|_| PulsationsModel::default()),
            vault: cx.new(|_| VaultModel::default()),
            chat: cx.new(|_| ChatModel::default()),
            settings: cx.new(|_| SettingsModel::default()),
            logs: cx.new(|_| LogsModel::default()),
            active_view,
            status: "Pronto".into(),
            event_count: 0,
            needs_resync: false,
            notices: HeaderNoticeQueue::default(),
            lifecycle: LifecycleState::default(),
            hydrating: false,
            session_generation: 0,
            session_dirty: false,
            workspace_save_generations: HashMap::new(),
            suppress_editor_sync: false,
            event_task: None,
            polling_task: None,
            workspace_task: None,
            file_tasks: HashMap::new(),
            autosave_tasks: HashMap::new(),
            hitl_task: None,
            session_task: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn active_view(&self) -> AppView {
        self.active_view
    }

    pub fn status(&self) -> &SharedString {
        &self.status
    }

    pub fn event_count(&self) -> u64 {
        self.event_count
    }

    pub fn needs_resync(&self) -> bool {
        self.needs_resync
    }

    pub fn sidebar_collapsed(&self, cx: &gpui::App) -> bool {
        self.navigation.read(cx).sidebar_collapsed
    }

    pub(crate) fn current_notice(&self) -> Option<&HeaderNotice> {
        self.notices.current()
    }

    pub fn sidebar_badges(&self, cx: &gpui::App) -> (usize, usize) {
        let pulsations = self.pulsations.read(cx);
        (pulsations.awaiting_hitl, pulsations.running_jobs)
    }

    pub fn select_view(&mut self, view: AppView, cx: &mut Context<Self>) {
        self.active_view = view;
        self.navigation.update(cx, |navigation, _| {
            navigation.active = view;
        });
        self.workspace.update(cx, |workspace, _| {
            workspace.diff_expanded = false;
        });
        self.status = format!("View {} ativa", view.short_label()).into();
        self.schedule_session_persistence(cx);
        cx.notify();
    }

    pub fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.navigation.update(cx, |navigation, _| {
            navigation.sidebar_collapsed = !navigation.sidebar_collapsed;
        });
        self.schedule_session_persistence(cx);
        cx.notify();
    }

    pub fn set_sidebar_collapsed(&mut self, collapsed: bool, cx: &mut Context<Self>) {
        self.navigation.update(cx, |navigation, _| {
            navigation.sidebar_collapsed = collapsed;
        });
        self.schedule_session_persistence(cx);
        cx.notify();
    }

    pub fn set_context_drawer_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.workspace.update(cx, |workspace, _| {
            workspace.context_drawer_open = open;
        });
        self.schedule_session_persistence(cx);
        cx.notify();
    }

    pub fn dismiss_header_notice(&mut self, cx: &mut Context<Self>) {
        self.notices.dismiss();
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn push_header_notice(
        &mut self,
        kind: HeaderNoticeKind,
        message: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        self.notices.push(kind, message);
        cx.notify();
    }

    pub fn choose_workspace(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(core) = self.core.clone() else {
            self.status = "Seletor indisponível no ambiente de teste".into();
            cx.notify();
            return;
        };

        let selected = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Selecionar workspace".into()),
        });
        self.status = "Aguardando seleção do workspace…".into();
        cx.notify();

        self.workspace_task = Some(cx.spawn(async move |this, cx| {
            let path = match selected.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                _ => None,
            };
            let Some(path) = path else {
                let _ = this.update(cx, |model, cx| {
                    model.status = "Seleção de workspace cancelada".into();
                    cx.notify();
                });
                return;
            };
            let path = path.to_string_lossy().into_owned();
            let runtime = core.runtime_handle().clone();
            let restored = runtime
                .spawn_blocking(move || load_workspace(Arc::clone(&core), path))
                .await;
            let _ = this.update(cx, |model, cx| {
                model.finish_workspace_restore(restored.map(|result| result.map(Some)), cx);
            });
        }));
    }

    pub fn open_workspace_file(
        &mut self,
        relative_path: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        let relative_path = relative_path.into();
        if self.workspace.read(cx).tab(&relative_path).is_some() {
            self.select_workspace_tab(&relative_path, cx);
            return;
        }
        let Some(backend) = self.workspace_backend.clone() else {
            self.status = "Leitura de arquivo indisponível no ambiente de teste".into();
            cx.notify();
            return;
        };
        self.status = format!("Abrindo {relative_path}…").into();
        cx.notify();
        let requested_path = relative_path.clone();
        let task_key = format!("open:{relative_path}");
        let task = cx.spawn(async move |this, cx| {
            let result = backend.read(requested_path.clone()).await;
            let _ = this.update(cx, |model, cx| match result {
                Ok(snapshot) => {
                    model.sync_active_editor(cx);
                    model.workspace.update(cx, |workspace, _| {
                        workspace.open_tab(
                            requested_path.clone(),
                            snapshot.content,
                            snapshot.revision,
                        );
                    });
                    model.restore_editor_for_active(cx);
                    model.status = format!("Arquivo aberto: {requested_path}").into();
                    model.schedule_session_persistence(cx);
                    cx.notify();
                }
                Err(error) => {
                    model.status = format!("Falha ao abrir {requested_path}").into();
                    model.notices.push(HeaderNoticeKind::Error, error.message);
                    cx.notify();
                }
            });
        });
        self.file_tasks.insert(task_key, task);
    }

    pub fn select_workspace_tab(&mut self, relative_path: &str, cx: &mut Context<Self>) {
        let previous_path = self.active_workspace_path(cx);
        if self.sync_active_editor(cx)
            && let Some(previous_path) = previous_path
        {
            self.schedule_workspace_autosave_for_path(previous_path, cx);
        }
        let selected = self
            .workspace
            .update(cx, |workspace, _| workspace.select_tab(relative_path));
        if selected {
            self.restore_editor_for_active(cx);
            self.status = format!("Aba ativa: {relative_path}").into();
            self.schedule_session_persistence(cx);
            cx.notify();
        }
    }

    pub fn close_workspace_tab(&mut self, relative_path: &str, cx: &mut Context<Self>) {
        self.sync_active_editor(cx);
        self.workspace_save_generations.remove(relative_path);
        self.autosave_tasks.remove(relative_path);
        let closed = self
            .workspace
            .update(cx, |workspace, _| workspace.close_tab(relative_path));
        if closed {
            self.restore_editor_for_active(cx);
            self.status = format!("Aba fechada: {relative_path}").into();
            self.schedule_session_persistence(cx);
            cx.notify();
        }
    }

    pub fn set_workspace_view_mode(
        &mut self,
        view_mode: WorkspaceViewMode,
        cx: &mut Context<Self>,
    ) {
        self.workspace.update(cx, |workspace, _| {
            workspace.view_mode = view_mode;
            if view_mode == WorkspaceViewMode::Editor {
                workspace.diff_expanded = false;
            }
        });
        self.schedule_session_persistence(cx);
        cx.notify();
    }

    pub fn toggle_workspace_diff(&mut self, cx: &mut Context<Self>) {
        self.workspace.update(cx, |workspace, _| {
            workspace.diff_expanded = !workspace.diff_expanded;
        });
        cx.notify();
    }

    pub fn resolve_workspace_conflict_by_reload(
        &mut self,
        relative_path: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        let relative_path = relative_path.into();
        let Some(backend) = self.workspace_backend.clone() else {
            return;
        };
        if !self.workspace.update(cx, |workspace, _| {
            workspace.begin_conflict_resolution(&relative_path)
        }) {
            return;
        }
        cx.notify();
        let task_key = format!("conflict:{relative_path}");
        let task = cx.spawn(async move |this, cx| {
            let result = backend.read(relative_path.clone()).await;
            let _ = this.update(cx, |model, cx| match result {
                Ok(snapshot) => {
                    model.workspace.update(cx, |workspace, _| {
                        workspace.replace_from_disk(&relative_path, snapshot);
                    });
                    if model.active_workspace_path(cx).as_deref() == Some(relative_path.as_str()) {
                        model.restore_editor_for_active(cx);
                    }
                    model.status = format!("Arquivo recarregado: {relative_path}").into();
                    model.schedule_session_persistence(cx);
                    cx.notify();
                }
                Err(error) => {
                    model.workspace.update(cx, |workspace, _| {
                        workspace.mark_save_failed(&relative_path, error);
                    });
                    cx.notify();
                }
            });
        });
        self.file_tasks.insert(task_key, task);
    }

    pub fn resolve_workspace_conflict_by_overwrite(
        &mut self,
        relative_path: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        let relative_path = relative_path.into();
        let Some(backend) = self.workspace_backend.clone() else {
            return;
        };
        if !self.workspace.update(cx, |workspace, _| {
            workspace.begin_conflict_resolution(&relative_path)
        }) {
            return;
        }
        cx.notify();
        let task_key = format!("conflict:{relative_path}");
        let task = cx.spawn(async move |this, cx| {
            let latest = backend.read(relative_path.clone()).await;
            let latest = match latest {
                Ok(latest) => latest,
                Err(error) => {
                    let _ = this.update(cx, |model, cx| {
                        model.workspace.update(cx, |workspace, _| {
                            workspace.mark_save_failed(&relative_path, error);
                        });
                        cx.notify();
                    });
                    return;
                }
            };
            let content = this
                .update(cx, |model, cx| {
                    model
                        .workspace
                        .read(cx)
                        .tab(&relative_path)
                        .map(|tab| tab.content.clone())
                })
                .ok()
                .flatten();
            let Some(content) = content else {
                return;
            };
            let result = backend
                .write(relative_path.clone(), content.clone(), latest.revision)
                .await;
            let _ = this.update(cx, |model, cx| {
                model.finish_workspace_save(relative_path, content, result, cx);
            });
        });
        self.file_tasks.insert(task_key, task);
    }

    pub(crate) fn apply_hitl_decision(&mut self, decision: HitlDecision, cx: &mut Context<Self>) {
        let request = self
            .pulsations
            .update(cx, |pulsations, _| pulsations.hitl.begin(decision));
        let Some((pulsation_id, decision)) = request else {
            return;
        };
        let Some(core) = self.core.clone() else {
            self.pulsations.update(cx, |pulsations, _| {
                pulsations
                    .hitl
                    .finish(Err("Decisão HITL indisponível no ambiente de teste".into()));
            });
            cx.notify();
            return;
        };
        self.status = "Aplicando decisão HITL…".into();
        cx.notify();
        let runtime = core.runtime_handle().clone();
        self.hitl_task = Some(cx.spawn(async move |this, cx| {
            let result = runtime
                .spawn_blocking(move || {
                    core.persistence()
                        .apply_hitl_decision(ApplyHitlDecisionRequest {
                            pulsation_id,
                            decision,
                        })
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                })
                .await
                .map_err(|error| format!("worker HITL interrompido: {error}"))
                .and_then(|result| result);
            let _ = this.update(cx, |model, cx| {
                model.pulsations.update(cx, |pulsations, _| {
                    pulsations.hitl.finish(result.clone());
                    if result.is_ok() {
                        pulsations.awaiting_hitl = pulsations.awaiting_hitl.saturating_sub(1);
                    }
                });
                match result {
                    Ok(()) => {
                        model.workspace.update(cx, |workspace, _| {
                            workspace.diff_expanded = false;
                            workspace.view_mode = WorkspaceViewMode::Editor;
                        });
                        model.status = "Decisão HITL aplicada".into();
                        model.schedule_session_persistence(cx);
                    }
                    Err(error) => {
                        model.status = "Falha ao aplicar decisão HITL".into();
                        model.notices.push(HeaderNoticeKind::Error, error);
                    }
                }
                cx.notify();
            });
        }));
    }

    fn active_workspace_path(&self, cx: &gpui::App) -> Option<String> {
        self.workspace
            .read(cx)
            .active_relative_path
            .as_ref()
            .map(ToString::to_string)
    }

    fn sync_active_editor(&mut self, cx: &mut Context<Self>) -> bool {
        let snapshot = {
            let workspace = self.workspace.read(cx);
            workspace.editor.read(cx).buffer.snapshot()
        };
        self.workspace
            .update(cx, |workspace, _| workspace.sync_active_editor(&snapshot))
    }

    fn handle_editor_update(&mut self, cx: &mut Context<Self>) {
        if self.hydrating || self.lifecycle.is_disposed() {
            return;
        }
        if self.suppress_editor_sync {
            self.suppress_editor_sync = false;
            let restored_without_edit = {
                let workspace = self.workspace.read(cx);
                workspace.editor.read(cx).edit_generation() == 0
            };
            if restored_without_edit {
                return;
            }
        }
        let content_changed = self.sync_active_editor(cx);
        if content_changed {
            self.schedule_workspace_autosave(cx);
        }
    }

    fn restore_editor_for_active(&mut self, cx: &mut Context<Self>) {
        let (editor, relative_path, snapshot) = {
            let workspace = self.workspace.read(cx);
            let snapshot = workspace
                .active_tab()
                .map(WorkspaceTabModel::editor_snapshot);
            (
                workspace.editor.clone(),
                workspace
                    .active_relative_path
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
                snapshot,
            )
        };
        let snapshot = snapshot.unwrap_or(EditorSnapshot {
            text: String::new(),
            revision: "new".into(),
            selection: EditorSelection::default(),
            scroll_top: 0.0,
            dirty: false,
        });
        self.suppress_editor_sync = true;
        editor.update(cx, |editor, cx| {
            let mut snapshot = snapshot;
            if editor
                .restore_workspace_snapshot(&relative_path, snapshot.clone())
                .is_err()
            {
                snapshot.selection = EditorSelection::default();
                editor
                    .restore_workspace_snapshot(&relative_path, snapshot)
                    .expect("a seleção padrão deve ser válida");
            }
            cx.notify();
        });
    }

    fn schedule_workspace_autosave(&mut self, cx: &mut Context<Self>) {
        let Some(relative_path) = self.active_workspace_path(cx) else {
            return;
        };
        self.schedule_workspace_autosave_for_path(relative_path, cx);
    }

    fn schedule_workspace_autosave_for_path(
        &mut self,
        relative_path: String,
        cx: &mut Context<Self>,
    ) {
        let generation = self
            .workspace_save_generations
            .entry(relative_path.clone())
            .and_modify(|generation| *generation = generation.saturating_add(1))
            .or_insert(1)
            .to_owned();
        let eligible = self
            .workspace
            .read(cx)
            .tab(&relative_path)
            .is_some_and(|tab| tab.dirty && !tab.saving && tab.conflict.is_none());
        let Some(backend) = self.workspace_backend.clone() else {
            return;
        };
        if !eligible {
            return;
        }
        let timer = cx.background_executor().timer(AUTOSAVE_DEBOUNCE);
        let task_path = relative_path.clone();
        let task = cx.spawn(async move |this, cx| {
            timer.await;
            let request = this
                .update(cx, |model, cx| {
                    model.begin_workspace_save(generation, &task_path, cx)
                })
                .ok()
                .flatten();
            let Some((relative_path, content, revision)) = request else {
                return;
            };
            let result = backend
                .write(relative_path.clone(), content.clone(), revision)
                .await;
            let _ = this.update(cx, |model, cx| {
                model.finish_workspace_save(relative_path, content, result, cx);
            });
        });
        self.autosave_tasks.insert(relative_path, task);
    }

    fn begin_workspace_save(
        &mut self,
        generation: u64,
        relative_path: &str,
        cx: &mut Context<Self>,
    ) -> Option<(String, String, String)> {
        if self.workspace_save_generations.get(relative_path).copied() != Some(generation)
            || self.lifecycle.is_disposed()
        {
            return None;
        }
        let request = {
            let workspace = self.workspace.read(cx);
            let tab = workspace.tab(relative_path)?;
            (
                tab.relative_path.to_string(),
                tab.content.clone(),
                tab.revision.clone(),
            )
        };
        let started = self
            .workspace
            .update(cx, |workspace, _| workspace.mark_saving(&request.0));
        if started {
            cx.notify();
            Some(request)
        } else {
            None
        }
    }

    fn finish_workspace_save(
        &mut self,
        relative_path: String,
        saved_content: String,
        result: Result<String, crate::workspace::WorkspaceOperationError>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(revision) => {
                let active = self.active_workspace_path(cx).as_deref() == Some(&relative_path);
                let (still_dirty, current_content) = self.workspace.update(cx, |workspace, _| {
                    let still_dirty =
                        workspace.mark_saved(&relative_path, &saved_content, revision.clone());
                    let current_content =
                        workspace.tab(&relative_path).map(|tab| tab.content.clone());
                    (still_dirty, current_content)
                });
                if active {
                    let editor = self.workspace.read(cx).editor.clone();
                    editor.update(cx, |editor, _| {
                        let editor_still_dirty = current_content
                            .as_deref()
                            .is_some_and(|content| content != saved_content);
                        editor.buffer.acknowledge_save(revision, editor_still_dirty);
                    });
                }
                self.status = format!("Arquivo salvo: {relative_path}").into();
                self.schedule_session_persistence(cx);
                if still_dirty {
                    self.schedule_workspace_autosave_for_path(relative_path, cx);
                }
            }
            Err(error) => {
                let message = error.message.clone();
                let conflict = error.is_conflict();
                self.workspace.update(cx, |workspace, _| {
                    workspace.mark_save_failed(&relative_path, error);
                });
                self.status = if conflict {
                    format!("Conflito de revisão: {relative_path}")
                } else {
                    format!("Falha ao salvar: {relative_path}")
                }
                .into();
                self.notices.push(
                    if conflict {
                        HeaderNoticeKind::Warning
                    } else {
                        HeaderNoticeKind::Error
                    },
                    message,
                );
            }
        }
        cx.notify();
    }

    fn start_lifecycle(&mut self, cx: &mut Context<Self>) {
        let Some(core) = self.core.clone() else {
            return;
        };
        self.lifecycle.start(1);

        let mut receiver = core.subscribe();
        self.event_task = Some(cx.spawn(async move |this, cx| {
            loop {
                match receiver.recv().await {
                    Ok(event) => {
                        if this
                            .update(cx, |model, cx| model.handle_event(event, cx))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(RecvError::Lagged(_)) => {
                        if this
                            .update(cx, |model, cx| {
                                model.needs_resync = true;
                                model.status =
                                    "Eventos atrasados; recarregando snapshot persistido".into();
                                model.restore_last_session(cx);
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(RecvError::Closed) => break,
                }
            }
        }));

        let timer = cx.background_executor().clone();
        self.polling_task = Some(cx.spawn(async move |this, cx| {
            loop {
                timer.timer(std::time::Duration::from_millis(1_200)).await;
                let state = this.update(cx, |model, cx| {
                    if model.lifecycle.is_disposed() {
                        return (true, None);
                    }
                    let project_id = model
                        .workspace
                        .read(cx)
                        .project_id
                        .as_ref()
                        .map(ToString::to_string);
                    if project_id.is_none() || !model.lifecycle.try_begin_poll() {
                        return (false, None);
                    }
                    (false, project_id)
                });
                let Ok((disposed, project_id)) = state else {
                    break;
                };
                if disposed {
                    break;
                }
                let Some(project_id) = project_id else {
                    continue;
                };

                let worker_core = Arc::clone(&core);
                let runtime = core.runtime_handle().clone();
                let snapshot = runtime
                    .spawn_blocking(move || {
                        worker_core.persistence().get_pulsation_runtime_snapshot(
                            GetPulsationRuntimeSnapshotRequest { project_id },
                        )
                    })
                    .await;
                if this
                    .update(cx, |model, cx| {
                        model.lifecycle.finish_poll();
                        match snapshot {
                            Ok(Ok(response)) => {
                                let checkpoint = response
                                    .snapshot
                                    .pulsations
                                    .iter()
                                    .find(|pulsation| {
                                        pulsation.status == PulsationStatus::Checkpoint
                                    })
                                    .cloned();
                                model.pulsations.update(cx, |pulsations, _| {
                                    pulsations.awaiting_hitl = response.snapshot.hitl_waiting_count;
                                    pulsations.running_jobs =
                                        response.snapshot.background_agents_count;
                                    pulsations.hitl.load_checkpoint(checkpoint.as_ref());
                                    if let Some(checkpoint) = checkpoint {
                                        pulsations.active_pulsation_id = Some(checkpoint.id.into());
                                    }
                                });
                            }
                            Ok(Err(error)) => model.notices.push(
                                HeaderNoticeKind::Warning,
                                format!("Falha ao atualizar runtime: {error}"),
                            ),
                            Err(error) => model.notices.push(
                                HeaderNoticeKind::Warning,
                                format!("Worker de runtime interrompido: {error}"),
                            ),
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    fn restore_last_session(&mut self, cx: &mut Context<Self>) {
        let Some(core) = self.core.clone() else {
            return;
        };
        self.hydrating = true;
        self.status = "Restaurando sessão persistida…".into();
        cx.notify();

        let runtime = core.runtime_handle().clone();
        self.workspace_task = Some(cx.spawn(async move |this, cx| {
            let restored = runtime
                .spawn_blocking(move || -> Result<Option<RestoredSession>, String> {
                    let project = core
                        .persistence()
                        .get_last_active_project()
                        .map_err(|error| error.to_string())?
                        .project;
                    let Some(project) = project else {
                        return Ok(None);
                    };
                    load_workspace(Arc::clone(&core), project.workspace_path).map(Some)
                })
                .await;
            let _ = this.update(cx, |model, cx| {
                model.finish_workspace_restore(restored, cx);
            });
        }));
    }

    fn finish_workspace_restore(
        &mut self,
        restored: Result<Result<Option<RestoredSession>, String>, tokio::task::JoinError>,
        cx: &mut Context<Self>,
    ) {
        match restored {
            Ok(Ok(Some(restored))) => self.apply_restored_session(restored, cx),
            Ok(Ok(None)) => {
                self.hydrating = false;
                self.needs_resync = false;
                self.status = "Pronto — selecione um workspace".into();
            }
            Ok(Err(error)) => {
                self.hydrating = false;
                self.status = format!("Falha ao restaurar sessão: {error}").into();
                self.notices.push(HeaderNoticeKind::Error, error);
            }
            Err(error) => {
                self.hydrating = false;
                self.status = format!("Falha no worker de restauração: {error}").into();
                self.notices.push(
                    HeaderNoticeKind::Error,
                    format!("Worker de restauração interrompido: {error}"),
                );
            }
        }
        cx.notify();
    }

    fn apply_restored_session(&mut self, restored: RestoredSession, cx: &mut Context<Self>) {
        self.hydrating = true;
        self.suppress_editor_sync = true;
        let session = restored.session.as_ref();
        let active_view = session
            .map(|session| AppView::from_id(&session.active_view))
            .unwrap_or(AppView::Workspace);
        let requested_active = session.and_then(|session| session.active_tab_path.as_deref());
        let active_path = requested_active
            .filter(|path| restored.tabs.iter().any(|tab| tab.relative_path == *path))
            .map(str::to_owned)
            .or_else(|| restored.tabs.first().map(|tab| tab.relative_path.clone()));
        let tabs = restored
            .tabs
            .into_iter()
            .map(|tab| WorkspaceTabModel {
                relative_path: tab.relative_path.into(),
                content: tab.content,
                base_content: tab.base_content,
                revision: tab.revision,
                dirty: tab.dirty,
                saving: false,
                conflict: None,
                save_error: tab.load_error,
                cursor_line: tab.cursor_line,
                cursor_column: tab.cursor_column,
                scroll_top: tab.scroll_top,
            })
            .collect::<Vec<_>>();

        self.navigation.update(cx, |navigation, _| {
            navigation.active = active_view;
            navigation.sidebar_collapsed = session.is_some_and(|value| value.sidebar_collapsed);
        });
        self.workspace.update(cx, |workspace, cx| {
            workspace.project_id = Some(restored.project_id.clone().into());
            workspace.root_path = Some(restored.root_path.clone().into());
            workspace.tree = restored.tree.clone();
            workspace.active_relative_path = active_path.clone().map(Into::into);
            workspace.tabs = tabs;
            workspace.view_mode = session
                .map(|value| WorkspaceViewMode::from_id(&value.workspace_view_mode))
                .unwrap_or(WorkspaceViewMode::Editor);
            workspace.context_drawer_open = session.is_none_or(|value| value.context_drawer_open);
            workspace.diff_expanded = false;

            if let Some(active_path) = &active_path
                && let Some(tab) = workspace
                    .tabs
                    .iter()
                    .find(|tab| tab.relative_path.as_ref() == active_path)
            {
                let position = TextPosition {
                    line: tab.cursor_line.unwrap_or_default(),
                    column: tab.cursor_column.unwrap_or_default(),
                };
                let mut snapshot = EditorSnapshot {
                    text: tab.content.clone(),
                    revision: tab.revision.clone(),
                    selection: EditorSelection::caret(position),
                    scroll_top: tab.scroll_top.unwrap_or_default() as f32,
                    dirty: tab.dirty,
                };
                workspace.editor.update(cx, |editor, cx| {
                    if editor
                        .restore_workspace_snapshot(active_path, snapshot.clone())
                        .is_err()
                    {
                        snapshot.selection = EditorSelection::default();
                        editor
                            .restore_workspace_snapshot(active_path, snapshot)
                            .expect("a seleção padrão deve ser válida");
                    }
                    cx.notify();
                });
            }
        });
        self.pulsations.update(cx, |pulsations, _| {
            pulsations.active_pulsation_id = session
                .and_then(|value| value.active_pulsation_id.clone())
                .map(Into::into);
        });
        self.vault.update(cx, |vault, _| {
            vault.selected_path = session
                .and_then(|value| value.selected_vault_path.clone())
                .map(Into::into);
        });
        self.chat.update(cx, |chat, _| {
            chat.active_conversation = session
                .and_then(|value| value.active_chat_conversation_id.clone())
                .map(Into::into);
        });
        self.active_view = active_view;
        self.needs_resync = false;
        self.session_dirty = false;
        self.status = format!("Sessão restaurada: {}", restored.root_path).into();
        if let Some(notice) = restored.notice {
            self.notices.push(HeaderNoticeKind::Notice, notice);
        }
        self.hydrating = false;
        cx.notify();
    }

    fn schedule_session_persistence(&mut self, cx: &mut Context<Self>) {
        if self.hydrating || self.lifecycle.is_disposed() {
            return;
        }
        let Some(request) = self.session_request(cx) else {
            return;
        };
        let Some(persistence) = self.session_persistence.clone() else {
            return;
        };

        self.session_generation = self.session_generation.saturating_add(1);
        let generation = self.session_generation;
        self.session_dirty = true;
        let timer = cx.background_executor().timer(SESSION_DEBOUNCE);
        self.session_task = Some(cx.spawn(async move |this, cx| {
            timer.await;
            let result = persistence.save(request).await;
            let _ = this.update(cx, |model, cx| {
                model.complete_session_save(generation, result, cx);
            });
        }));
    }

    fn complete_session_save(
        &mut self,
        generation: u64,
        result: Result<String, String>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.session_generation {
            return;
        }
        match result {
            Ok(_) => {
                self.session_dirty = false;
                self.status = "Sessão salva".into();
            }
            Err(error) => {
                self.session_dirty = true;
                self.notices
                    .push(HeaderNoticeKind::Warning, format!("Persistência: {error}"));
            }
        }
        cx.notify();
    }

    fn session_request(&self, cx: &gpui::App) -> Option<SaveUiSessionRequest> {
        let workspace = self.workspace.read(cx);
        let project_id = workspace.project_id.as_ref()?.to_string();
        let workspace_root = workspace.root_path.as_ref()?.to_string();
        let active_path = workspace
            .active_relative_path
            .as_ref()
            .map(ToString::to_string);
        let editor = workspace.editor.read(cx);
        let editor_snapshot = editor.buffer.snapshot();
        let editor_was_edited = editor.edit_generation() > 0;
        let tabs = workspace
            .tabs
            .iter()
            .map(|tab| {
                let is_active = active_path
                    .as_deref()
                    .is_some_and(|path| tab.relative_path.as_ref() == path);
                if is_active && editor_was_edited {
                    UiTabSnapshot {
                        relative_path: tab.relative_path.to_string(),
                        draft_content: editor_snapshot.dirty.then(|| editor_snapshot.text.clone()),
                        base_revision: Some(editor_snapshot.revision.clone()),
                        cursor_line: Some(editor_snapshot.selection.head.line.saturating_add(1)),
                        cursor_column: Some(
                            editor_snapshot.selection.head.column.saturating_add(1),
                        ),
                        scroll_top: Some(f64::from(editor_snapshot.scroll_top)),
                    }
                } else {
                    UiTabSnapshot {
                        relative_path: tab.relative_path.to_string(),
                        draft_content: tab.dirty.then(|| tab.content.clone()),
                        base_revision: Some(tab.revision.clone()),
                        cursor_line: tab.cursor_line.map(|line| line.saturating_add(1)),
                        cursor_column: tab.cursor_column.map(|column| column.saturating_add(1)),
                        scroll_top: tab.scroll_top,
                    }
                }
            })
            .collect();
        let pulsations = self.pulsations.read(cx);
        let vault = self.vault.read(cx);
        let chat = self.chat.read(cx);

        Some(SaveUiSessionRequest {
            project_id,
            workspace_root,
            active_tab_path: active_path,
            workspace_view_mode: workspace.view_mode.id().to_string(),
            active_view: self.active_view.id().to_string(),
            sidebar_collapsed: self.navigation.read(cx).sidebar_collapsed,
            context_drawer_open: workspace.context_drawer_open,
            active_pulsation_id: pulsations
                .active_pulsation_id
                .as_ref()
                .map(ToString::to_string),
            selected_vault_path: vault.selected_path.as_ref().map(ToString::to_string),
            active_chat_conversation_id: chat.active_conversation.as_ref().map(ToString::to_string),
            scroll_top: Some(f64::from(editor_snapshot.scroll_top)),
            tabs,
        })
    }

    fn flush_pending_session(&mut self, cx: &mut Context<Self>) -> Task<()> {
        self.lifecycle.dispose();
        self.event_task.take();
        self.polling_task.take();
        self.workspace_task.take();
        self.file_tasks.clear();
        self.autosave_tasks.clear();
        self.hitl_task.take();
        self.session_task.take();
        self.session_generation = self.session_generation.saturating_add(1);
        self.workspace_save_generations.clear();

        let request = self
            .session_dirty
            .then(|| self.session_request(cx))
            .flatten();
        let persistence = self.session_persistence.clone();
        cx.spawn(async move |this, cx| {
            let result = match (persistence, request) {
                (Some(persistence), Some(request)) => Some(persistence.save(request).await),
                _ => None,
            };
            if let Some(result) = result {
                let _ = this.update(cx, |model, cx| {
                    match result {
                        Ok(_) => model.session_dirty = false,
                        Err(error) => model.notices.push(
                            HeaderNoticeKind::Warning,
                            format!("Persistência no encerramento: {error}"),
                        ),
                    }
                    cx.notify();
                });
            }
        })
    }

    fn handle_event(&mut self, event: AppEvent, cx: &mut Context<Self>) {
        self.event_count = self.event_count.saturating_add(1);
        self.status = format!("Evento recebido: {}", event.event_name()).into();
        if let AppEvent::WorkspaceChanged(event) = event {
            self.handle_workspace_change(event, cx);
        }
        self.logs.update(cx, |logs, _| {
            logs.visible_entries = logs.visible_entries.saturating_add(1);
        });
        cx.notify();
    }

    fn handle_workspace_change(&mut self, event: WorkspaceChangedEvent, cx: &mut Context<Self>) {
        let should_reload = self.workspace.update(cx, |workspace, _| {
            workspace.mark_external_change(&event.relative_path, &event.kind)
        });
        if !should_reload {
            return;
        }
        let Some(backend) = self.workspace_backend.clone() else {
            return;
        };
        let relative_path = event.relative_path;
        let task_key = format!("external:{relative_path}");
        let task = cx.spawn(async move |this, cx| {
            let result = backend.read(relative_path.clone()).await;
            let _ = this.update(cx, |model, cx| {
                if let Ok(snapshot) = result {
                    model.workspace.update(cx, |workspace, _| {
                        workspace.replace_from_disk(&relative_path, snapshot);
                    });
                    if model.active_workspace_path(cx).as_deref() == Some(relative_path.as_str()) {
                        model.restore_editor_for_active(cx);
                    }
                    model.schedule_session_persistence(cx);
                }
                cx.notify();
            });
        });
        self.file_tasks.insert(task_key, task);
    }
}

fn load_workspace(core: Arc<AppCore>, path: String) -> Result<RestoredSession, String> {
    let opened = core
        .open_workspace(OpenWorkspaceRequest { path })
        .map_err(|error| error.message)?;
    let project = core
        .persistence()
        .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
            workspace_path: opened.root_path.clone(),
            preferred_name: None,
        })
        .map_err(|error| error.to_string())?
        .project;
    let session = core
        .persistence()
        .load_ui_session(LoadUiSessionRequest {
            project_id: project.id.clone(),
        })
        .map_err(|error| error.to_string())?
        .session;
    let tabs = session
        .as_ref()
        .map(|session| {
            session
                .tabs
                .iter()
                .cloned()
                .map(|snapshot| load_tab(&core, snapshot))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Ok(RestoredSession {
        project_id: project.id,
        root_path: opened.root_path,
        tree: opened.tree,
        session,
        tabs,
        notice: Some(opened.greenfield.message),
    })
}

fn load_tab(core: &AppCore, snapshot: UiTabSnapshot) -> LoadedTab {
    let disk = core.read_workspace_file(ReadWorkspaceFileRequest {
        relative_path: snapshot.relative_path.clone(),
    });
    let (base_content, revision, load_error) = match disk {
        Ok(response) => (response.content, response.revision, None),
        Err(error) => (
            String::new(),
            snapshot.base_revision.clone().unwrap_or_default(),
            Some(format!(
                "Não foi possível carregar {}: {}",
                snapshot.relative_path, error.message
            )),
        ),
    };
    let has_draft = snapshot.draft_content.is_some();
    let content = snapshot
        .draft_content
        .clone()
        .unwrap_or_else(|| base_content.clone());
    let dirty = has_draft && content != base_content;
    LoadedTab {
        relative_path: snapshot.relative_path,
        content,
        base_content,
        revision,
        dirty,
        cursor_line: snapshot.cursor_line.map(|line| line.saturating_sub(1)),
        cursor_column: snapshot
            .cursor_column
            .map(|column| column.saturating_sub(1)),
        scroll_top: snapshot.scroll_top,
        load_error,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use gpui::TestAppContext;
    use hvc_core::workspace_service::WorkspaceErrorCode::Conflict;

    use super::*;
    use crate::workspace::{WorkspaceFileSnapshot, WorkspaceFuture, WorkspaceOperationError};

    #[derive(Default)]
    struct RecordingSessionPersistence {
        requests: Arc<Mutex<Vec<SaveUiSessionRequest>>>,
    }

    impl SessionPersistence for RecordingSessionPersistence {
        fn save(&self, request: SaveUiSessionRequest) -> crate::session::SessionSaveFuture {
            let requests = Arc::clone(&self.requests);
            Box::pin(async move {
                requests
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(request);
                Ok("now".into())
            })
        }
    }

    struct RecordingWorkspaceBackend {
        reads: Arc<Mutex<Vec<String>>>,
        writes: Arc<Mutex<Vec<(String, String, String)>>>,
        read_snapshot: WorkspaceFileSnapshot,
        write_result: Arc<Mutex<Result<String, WorkspaceOperationError>>>,
    }

    impl RecordingWorkspaceBackend {
        fn successful() -> Self {
            Self {
                reads: Arc::new(Mutex::new(Vec::new())),
                writes: Arc::new(Mutex::new(Vec::new())),
                read_snapshot: WorkspaceFileSnapshot {
                    content: "# Disk".into(),
                    revision: "rev-1".into(),
                },
                write_result: Arc::new(Mutex::new(Ok("rev-2".into()))),
            }
        }

        fn conflicting() -> Self {
            let backend = Self::successful();
            *backend
                .write_result
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                Err(WorkspaceOperationError {
                    code: Conflict,
                    message: "A revisão do arquivo mudou no disco.".into(),
                });
            backend
        }
    }

    impl WorkspaceBackend for RecordingWorkspaceBackend {
        fn read(&self, relative_path: String) -> WorkspaceFuture<WorkspaceFileSnapshot> {
            self.reads
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(relative_path);
            let snapshot = self.read_snapshot.clone();
            Box::pin(async move { Ok(snapshot) })
        }

        fn write(
            &self,
            relative_path: String,
            content: String,
            expected_revision: String,
        ) -> WorkspaceFuture<String> {
            self.writes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((relative_path, content, expected_revision));
            let result = self
                .write_result
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            Box::pin(async move { result })
        }
    }

    fn configure_editable_tab(model: &mut AppModel, cx: &mut Context<AppModel>) {
        model.workspace.update(cx, |workspace, _| {
            workspace.open_tab("notes/one.md", "Inicial".into(), "rev-1".into());
        });
        model.restore_editor_for_active(cx);
    }

    fn edit_active_buffer(model: &AppModel, cx: &mut Context<AppModel>) {
        let editor = model.workspace.read(cx).editor.clone();
        editor.update(cx, |editor, cx| {
            editor
                .buffer
                .set_selection(EditorSelection::caret(TextPosition { line: 0, column: 7 }))
                .unwrap();
            editor.insert_text(" alterado", cx);
        });
    }

    fn configure_restorable_state(model: &mut AppModel, cx: &mut Context<AppModel>) {
        let editor = model.workspace.read(cx).editor.clone();
        model.workspace.update(cx, |workspace, _| {
            workspace.project_id = Some("project-1".into());
            workspace.root_path = Some("C:\\vault".into());
            workspace.active_relative_path = Some("Features/Login.md".into());
            workspace.view_mode = WorkspaceViewMode::Hitl;
            workspace.tabs = vec![WorkspaceTabModel {
                relative_path: "Features/Login.md".into(),
                content: "# Local draft".into(),
                base_content: "# Disk".into(),
                revision: "2:1".into(),
                dirty: true,
                saving: false,
                conflict: None,
                save_error: None,
                cursor_line: Some(6),
                cursor_column: Some(2),
                scroll_top: Some(44.0),
            }];
        });
        editor.update(cx, |editor, _| {
            editor
                .buffer
                .restore_snapshot(EditorSnapshot {
                    text: "# Local draft".into(),
                    revision: "2:1".into(),
                    selection: EditorSelection::caret(TextPosition { line: 0, column: 3 }),
                    scroll_top: 44.0,
                    dirty: true,
                })
                .unwrap();
        });
        model.pulsations.update(cx, |pulsations, _| {
            pulsations.active_pulsation_id = Some("pulse-1".into());
        });
        model.vault.update(cx, |vault, _| {
            vault.selected_path = Some("Features/Login.md".into());
        });
        model.chat.update(cx, |chat, _| {
            chat.active_conversation = Some("chat-1".into());
        });
    }

    #[gpui::test]
    fn navigates_all_six_views(cx: &mut TestAppContext) {
        let model = cx.new(AppModel::new_for_test);
        for view in AppView::ALL {
            model.update(cx, |model, cx| model.select_view(view, cx));
            assert_eq!(cx.read(|cx| model.read(cx).active_view()), view);
        }
    }

    #[gpui::test]
    fn creates_specific_entities_for_every_view(cx: &mut TestAppContext) {
        let model = cx.new(AppModel::new_for_test);
        cx.read(|cx| {
            let model = model.read(cx);
            assert_eq!(model.workspace.read(cx).open_tab_count(), 0);
            assert_eq!(model.workspace.read(cx).editor.read(cx).buffer.text(), "");
            assert_eq!(model.pulsations.read(cx).running_jobs, 0);
            assert_eq!(model.vault.read(cx).indexed_notes, 0);
            assert!(!model.chat.read(cx).streaming);
            assert_eq!(model.settings.read(cx).configured_providers, 0);
            assert_eq!(model.logs.read(cx).visible_entries, 0);
        });
    }

    #[gpui::test]
    fn unknown_persisted_view_falls_back_to_workspace(cx: &mut TestAppContext) {
        let _ = cx;
        assert_eq!(AppView::from_id("chat"), AppView::Chat);
        assert_eq!(AppView::from_id("future-view"), AppView::Workspace);
    }

    #[gpui::test]
    fn persists_layout_and_domain_selections_with_debounce(cx: &mut TestAppContext) {
        let persistence = Arc::new(RecordingSessionPersistence::default());
        let requests = Arc::clone(&persistence.requests);
        let model = cx.new(|cx| AppModel::new_for_test_with_persistence(persistence, cx));
        model.update(cx, |model, cx| {
            configure_restorable_state(model, cx);
            model.select_view(AppView::Settings, cx);
            model.set_sidebar_collapsed(true, cx);
            model.set_context_drawer_open(false, cx);
        });

        cx.executor()
            .advance_clock(SESSION_DEBOUNCE - std::time::Duration::from_millis(1));
        cx.run_until_parked();
        assert!(
            requests
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        );

        cx.executor()
            .advance_clock(std::time::Duration::from_millis(1));
        cx.run_until_parked();
        let requests = requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(request.project_id, "project-1");
        assert_eq!(request.active_view, "settings");
        assert!(request.sidebar_collapsed);
        assert!(!request.context_drawer_open);
        assert_eq!(request.workspace_view_mode, "hitl");
        assert_eq!(request.active_pulsation_id.as_deref(), Some("pulse-1"));
        assert_eq!(
            request.selected_vault_path.as_deref(),
            Some("Features/Login.md")
        );
        assert_eq!(
            request.active_chat_conversation_id.as_deref(),
            Some("chat-1")
        );
    }

    #[gpui::test]
    fn flushes_pending_session_during_shutdown(cx: &mut TestAppContext) {
        let persistence = Arc::new(RecordingSessionPersistence::default());
        let requests = Arc::clone(&persistence.requests);
        let model = cx.new(|cx| AppModel::new_for_test_with_persistence(persistence, cx));
        model.update(cx, |model, cx| {
            configure_restorable_state(model, cx);
            model.select_view(AppView::Vault, cx);
            model.select_view(AppView::Chat, cx);
        });
        cx.executor()
            .advance_clock(SESSION_DEBOUNCE - std::time::Duration::from_millis(1));
        cx.run_until_parked();
        assert!(
            requests
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        );

        model
            .update(cx, |model, cx| model.flush_pending_session(cx))
            .detach();
        cx.run_until_parked();
        let requests = requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].active_view, "chat");
    }

    #[gpui::test]
    fn rehydrates_all_domain_selections(cx: &mut TestAppContext) {
        let model = cx.new(AppModel::new_for_test);
        let session = UiSessionState {
            project_id: "project-1".into(),
            workspace_root: "C:\\vault".into(),
            active_tab_path: Some("Features/Login.md".into()),
            workspace_view_mode: "hitl".into(),
            active_view: "chat".into(),
            sidebar_collapsed: true,
            context_drawer_open: false,
            active_pulsation_id: Some("pulse-1".into()),
            selected_vault_path: Some("Features/Login.md".into()),
            active_chat_conversation_id: Some("chat-1".into()),
            scroll_top: Some(44.0),
            tabs: Vec::new(),
            updated_at: "now".into(),
        };
        model.update(cx, |model, cx| {
            model.apply_restored_session(
                RestoredSession {
                    project_id: "project-1".into(),
                    root_path: "C:\\vault".into(),
                    tree: Vec::new(),
                    session: Some(session),
                    tabs: vec![LoadedTab {
                        relative_path: "Features/Login.md".into(),
                        content: "# Local draft".into(),
                        base_content: "# Disk".into(),
                        revision: "2:1".into(),
                        dirty: true,
                        cursor_line: Some(6),
                        cursor_column: Some(2),
                        scroll_top: Some(44.0),
                        load_error: None,
                    }],
                    notice: None,
                },
                cx,
            );
        });

        cx.read(|cx| {
            let model = model.read(cx);
            assert_eq!(model.active_view, AppView::Chat);
            assert!(model.navigation.read(cx).sidebar_collapsed);
            let workspace = model.workspace.read(cx);
            assert!(!workspace.context_drawer_open);
            assert_eq!(workspace.view_mode, WorkspaceViewMode::Hitl);
            assert_eq!(workspace.open_tab_count(), 1);
            assert_eq!(
                workspace
                    .active_relative_path
                    .as_ref()
                    .map(ToString::to_string),
                Some("Features/Login.md".into())
            );
            assert!(workspace.editor.read(cx).buffer.is_dirty());
            assert_eq!(
                model
                    .pulsations
                    .read(cx)
                    .active_pulsation_id
                    .as_ref()
                    .map(ToString::to_string),
                Some("pulse-1".into())
            );
            assert_eq!(
                model
                    .vault
                    .read(cx)
                    .selected_path
                    .as_ref()
                    .map(ToString::to_string),
                Some("Features/Login.md".into())
            );
            assert_eq!(
                model
                    .chat
                    .read(cx)
                    .active_conversation
                    .as_ref()
                    .map(ToString::to_string),
                Some("chat-1".into())
            );
            let round_trip = model.session_request(cx).unwrap();
            assert_eq!(round_trip.tabs[0].cursor_line, Some(7));
            assert_eq!(round_trip.tabs[0].cursor_column, Some(3));
        });
    }

    #[gpui::test]
    fn hydrates_persisted_layout_and_resets_transient_state(cx: &mut TestAppContext) {
        let model = cx.new(AppModel::new_for_test);
        model.update(cx, |model, cx| {
            model.workspace.update(cx, |workspace, _| {
                workspace.diff_expanded = true;
            });
            model.apply_restored_session(
                RestoredSession {
                    project_id: "project-1".into(),
                    root_path: "C:\\vault".into(),
                    tree: Vec::new(),
                    session: Some(UiSessionState {
                        project_id: "project-1".into(),
                        workspace_root: "C:\\vault".into(),
                        active_tab_path: None,
                        workspace_view_mode: "editor".into(),
                        active_view: "vault".into(),
                        sidebar_collapsed: true,
                        context_drawer_open: false,
                        active_pulsation_id: None,
                        selected_vault_path: None,
                        active_chat_conversation_id: None,
                        scroll_top: None,
                        tabs: Vec::new(),
                        updated_at: "now".into(),
                    }),
                    tabs: Vec::new(),
                    notice: None,
                },
                cx,
            );
        });

        cx.read(|cx| {
            let model = model.read(cx);
            assert_eq!(model.active_view, AppView::Vault);
            assert!(model.navigation.read(cx).sidebar_collapsed);
            assert!(!model.workspace.read(cx).context_drawer_open);
            assert!(!model.workspace.read(cx).diff_expanded);
        });
    }

    #[gpui::test]
    fn persists_editor_changes_only_after_the_one_second_debounce(cx: &mut TestAppContext) {
        let backend = Arc::new(RecordingWorkspaceBackend::successful());
        let writes = Arc::clone(&backend.writes);
        let model = cx.new(|cx| AppModel::new_for_test_with_workspace_backend(backend, cx));
        model.update(cx, |model, cx| {
            configure_editable_tab(model, cx);
            edit_active_buffer(model, cx);
        });
        cx.run_until_parked();

        cx.executor()
            .advance_clock(AUTOSAVE_DEBOUNCE - std::time::Duration::from_millis(1));
        cx.run_until_parked();
        assert!(
            writes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        );

        cx.executor()
            .advance_clock(std::time::Duration::from_millis(1));
        cx.run_until_parked();
        let writes = writes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].0, "notes/one.md");
        assert_eq!(writes[0].2, "rev-1");
        drop(writes);
        cx.read(|cx| {
            let workspace = model.read(cx).workspace.read(cx);
            let tab = workspace.tab("notes/one.md").unwrap();
            assert_eq!(tab.revision, "rev-2");
            assert_eq!(tab.base_content, tab.content);
            assert!(!tab.dirty);
            assert!(!tab.saving);
            assert!(tab.conflict.is_none());
            assert!(tab.save_error.is_none());
        });
    }

    #[gpui::test]
    fn exposes_revision_conflicts_without_overwriting_the_disk(cx: &mut TestAppContext) {
        let backend = Arc::new(RecordingWorkspaceBackend::conflicting());
        let model = cx.new(|cx| AppModel::new_for_test_with_workspace_backend(backend, cx));
        model.update(cx, |model, cx| {
            configure_editable_tab(model, cx);
            edit_active_buffer(model, cx);
        });
        cx.run_until_parked();
        cx.executor().advance_clock(AUTOSAVE_DEBOUNCE);
        cx.run_until_parked();

        cx.read(|cx| {
            let workspace = model.read(cx).workspace.read(cx);
            let tab = workspace.tab("notes/one.md").unwrap();
            assert!(tab.dirty);
            assert!(!tab.saving);
            assert_eq!(
                tab.conflict.as_deref(),
                Some("A revisão do arquivo mudou no disco.")
            );
            assert_eq!(tab.base_content, "Inicial");
        });
    }

    #[gpui::test]
    fn autosaves_dirty_tabs_independently(cx: &mut TestAppContext) {
        let backend = Arc::new(RecordingWorkspaceBackend::successful());
        let writes = Arc::clone(&backend.writes);
        let model = cx.new(|cx| AppModel::new_for_test_with_workspace_backend(backend, cx));
        model.update(cx, |model, cx| {
            model.workspace.update(cx, |workspace, _| {
                workspace.open_tab("notes/one.md", "Primeira".into(), "rev-1".into());
                workspace.open_tab("notes/two.md", "Segunda".into(), "rev-1".into());
                workspace.select_tab("notes/one.md");
            });
            model.restore_editor_for_active(cx);
            edit_active_buffer(model, cx);
            model.select_workspace_tab("notes/two.md", cx);
            edit_active_buffer(model, cx);
        });
        cx.run_until_parked();
        cx.executor().advance_clock(AUTOSAVE_DEBOUNCE);
        cx.run_until_parked();

        let mut paths = writes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|write| write.0.clone())
            .collect::<Vec<_>>();
        paths.sort();
        assert_eq!(paths, vec!["notes/one.md", "notes/two.md"]);
    }

    #[gpui::test]
    fn opens_a_workspace_file_through_the_async_backend(cx: &mut TestAppContext) {
        let backend = Arc::new(RecordingWorkspaceBackend::successful());
        let reads = Arc::clone(&backend.reads);
        let model = cx.new(|cx| AppModel::new_for_test_with_workspace_backend(backend, cx));
        model.update(cx, |model, cx| {
            model.open_workspace_file("notes/one.md", cx);
        });
        cx.run_until_parked();

        assert_eq!(
            *reads
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec!["notes/one.md"]
        );
        cx.read(|cx| {
            let workspace = model.read(cx).workspace.read(cx);
            assert_eq!(workspace.open_tab_count(), 1);
            assert_eq!(workspace.active_tab().unwrap().content, "# Disk");
        });
    }
}
