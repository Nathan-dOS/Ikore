use std::collections::HashMap;
use std::sync::Arc;

#[path = "preparation_ui.rs"]
mod preparation_ui;

use gpui::{
    AppContext as _, ClipboardItem, Context, Entity, PathPromptOptions, SharedString, Subscription,
    Task, Window,
};
use hvc_core::agent_runtime::ContextStrategy;
use hvc_core::chat_service::ChatEventEnvelope;
use hvc_core::llm::{
    DeleteProviderConnectionRequest, DeleteProviderRouteRequest, ResetLlmResilienceStateRequest,
    RouteTarget, RoutingMode, TestProviderConnectionRequest, UpsertProviderConnectionRequest,
    UpsertProviderRouteBindingRequest, UpsertProviderRouteRequest,
};
use hvc_core::orchestration::brownfield::BrownfieldOnboardingStatus;
use hvc_core::persistence_service::{
    EnsureProjectForWorkspaceRequest, HitlDecision, LoadUiSessionRequest, PulsationRuntimeSnapshot,
    SaveUiSessionRequest, UiSessionState, UiTabSnapshot, UpdateAgentRuntimeConfigRequest,
};
use hvc_core::workspace_service::{
    FileNode, OpenWorkspaceRequest, ReadWorkspaceFileRequest, SearchMode, WorkspaceChangedEvent,
    WorkspaceKind,
};
use hvc_core::{AppCore, AppEvent};
use hvc_editor::{EditorSelection, EditorSnapshot, TextPosition};
use tokio::sync::broadcast::error::RecvError;

use crate::chat::{ChatBackend, ChatEventOutcome, ChatModel, CoreChatBackend};
use crate::lifecycle::LifecycleState;
use crate::logs::{CoreLogsBackend, LogsBackend, LogsModel, SequenceClassification};
use crate::pulsations::{CorePulsationsBackend, PulsationsBackend, PulsationsModel};
use crate::session::{
    CoreSessionPersistence, HeaderNotice, HeaderNoticeKind, HeaderNoticeQueue, SESSION_DEBOUNCE,
    SessionPersistence,
};
use crate::settings::{CoreSettingsBackend, SettingsBackend, SettingsModel};
use crate::text_input::NativeTextInput;
use crate::vault::{
    CONTEXT_DOCUMENTS_DEBOUNCE, CoreVaultBackend, VAULT_REFRESH_DEBOUNCE, VaultBackend, VaultModel,
    normalize_rename_target,
};
use crate::views::NavigationModel;
use crate::workspace::{
    AUTOSAVE_DEBOUNCE, BrownfieldOnboardingState, CoreWorkspaceBackend, WorkspaceBackend,
    WorkspaceEditorModel, WorkspaceTabModel, WorkspaceViewMode,
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
    brownfield_status: Option<BrownfieldOnboardingStatus>,
}

pub struct AppModel {
    core: Option<Arc<AppCore>>,
    session_persistence: Option<Arc<dyn SessionPersistence>>,
    workspace_backend: Option<Arc<dyn WorkspaceBackend>>,
    pulsations_backend: Option<Arc<dyn PulsationsBackend>>,
    vault_backend: Option<Arc<dyn VaultBackend>>,
    settings_backend: Option<Arc<dyn SettingsBackend>>,
    logs_backend: Option<Arc<dyn LogsBackend>>,
    chat_backend: Option<Arc<dyn ChatBackend>>,
    pub navigation: Entity<NavigationModel>,
    pub workspace: Entity<WorkspaceEditorModel>,
    pub pulsations: Entity<PulsationsModel>,
    pub vault: Entity<VaultModel>,
    pub(crate) pulsation_feature_input: Entity<NativeTextInput>,
    pub(crate) reconciliation_input: Entity<NativeTextInput>,
    pub(crate) description_review_input: Entity<NativeTextInput>,
    pub(crate) specification_answer_inputs: Vec<(String, Entity<NativeTextInput>)>,
    pub(crate) stage_feedback_input: Entity<NativeTextInput>,
    pub(crate) preparation_threshold_input: Entity<NativeTextInput>,
    pub(crate) context_limit_input: Entity<NativeTextInput>,
    preparation_scope: Option<(String, u64, Option<String>)>,
    question_subscriptions: Vec<Subscription>,
    pub(crate) artifact_preview_page: usize,
    pub(crate) vault_search_input: Entity<NativeTextInput>,
    pub(crate) vault_rename_input: Entity<NativeTextInput>,
    pub(crate) settings_connection_name_input: Entity<NativeTextInput>,
    pub(crate) settings_connection_model_input: Entity<NativeTextInput>,
    pub(crate) settings_connection_endpoint_input: Entity<NativeTextInput>,
    pub(crate) settings_connection_secret_input: Entity<NativeTextInput>,
    pub(crate) settings_route_name_input: Entity<NativeTextInput>,
    pub(crate) settings_embedding_model_input: Entity<NativeTextInput>,
    pub(crate) chat_draft_input: Entity<NativeTextInput>,
    pub chat: Entity<ChatModel>,
    pub(crate) settings: Entity<SettingsModel>,
    pub(crate) logs: Entity<LogsModel>,
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
    brownfield_task: Option<Task<()>>,
    brownfield_generation: u64,
    file_tasks: HashMap<String, Task<()>>,
    autosave_tasks: HashMap<String, Task<()>>,
    hitl_task: Option<Task<()>>,
    pulsations_task: Option<Task<()>>,
    reconciliation_task: Option<Task<()>>,
    preparation_action_task: Option<Task<()>>,
    artifact_preview_task: Option<Task<()>>,
    pulsation_artifacts_task: Option<Task<()>>,
    vault_refresh_task: Option<Task<()>>,
    vault_search_task: Option<Task<()>>,
    vault_rename_task: Option<Task<()>>,
    vault_context_task: Option<Task<()>>,
    vault_document_tasks: HashMap<String, Task<()>>,
    settings_task: Option<Task<()>>,
    logs_task: Option<Task<()>>,
    logs_resync_tasks: HashMap<String, Task<()>>,
    chat_task: Option<Task<()>>,
    chat_resync_tasks: HashMap<String, Task<()>>,
    session_task: Option<Task<()>>,
    performance_probe: Option<crate::performance::FrameLatencyProbe>,
    _subscriptions: Vec<Subscription>,
}

impl AppModel {
    pub fn new(core: Arc<AppCore>, cx: &mut Context<Self>) -> Self {
        let session_persistence: Arc<dyn SessionPersistence> =
            Arc::new(CoreSessionPersistence::new(Arc::clone(&core)));
        let workspace_backend: Arc<dyn WorkspaceBackend> =
            Arc::new(CoreWorkspaceBackend::new(Arc::clone(&core)));
        let pulsations_backend: Arc<dyn PulsationsBackend> =
            Arc::new(CorePulsationsBackend::new(Arc::clone(&core)));
        let vault_backend: Arc<dyn VaultBackend> =
            Arc::new(CoreVaultBackend::new(Arc::clone(&core)));
        let settings_backend: Arc<dyn SettingsBackend> =
            Arc::new(CoreSettingsBackend::new(Arc::clone(&core)));
        let logs_backend: Arc<dyn LogsBackend> = Arc::new(CoreLogsBackend::new(Arc::clone(&core)));
        let chat_backend: Arc<dyn ChatBackend> = Arc::new(CoreChatBackend::new(Arc::clone(&core)));
        let mut model = Self::build(
            Some(core),
            Some(session_persistence),
            Some(workspace_backend),
            cx,
        );
        model.pulsations_backend = Some(pulsations_backend);
        model.vault_backend = Some(vault_backend);
        model.settings_backend = Some(settings_backend);
        model.logs_backend = Some(logs_backend);
        model.chat_backend = Some(chat_backend);
        model.start_lifecycle(cx);
        cx.on_app_quit(|model, cx| model.flush_pending_session(cx))
            .detach();
        model.restore_last_session(cx);
        model.performance_probe = crate::performance::start_probe();
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

    #[cfg(test)]
    fn new_for_test_with_domain_backends(
        pulsations_backend: Arc<dyn PulsationsBackend>,
        vault_backend: Arc<dyn VaultBackend>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut model = Self::build(None, None, None, cx);
        model.pulsations_backend = Some(pulsations_backend);
        model.vault_backend = Some(vault_backend);
        model
    }

    #[cfg(test)]
    pub(crate) fn new_for_test_with_settings_backend(
        settings_backend: Arc<dyn SettingsBackend>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut model = Self::build(None, None, None, cx);
        model.settings_backend = Some(settings_backend);
        model
    }

    #[cfg(test)]
    pub(crate) fn new_for_test_with_chat_backend(
        chat_backend: Arc<dyn ChatBackend>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut model = Self::build(None, None, None, cx);
        model.chat_backend = Some(chat_backend);
        model
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
        let pulsation_feature_input =
            cx.new(|cx| NativeTextInput::new("Nome da funcionalidade", cx));
        let reconciliation_input =
            cx.new(|cx| NativeTextInput::new("O que você verificou ou corrigiu na base?", cx));
        let description_review_input =
            cx.new(|cx| NativeTextInput::new("Descreva a ação e o resultado esperado", cx));
        let stage_feedback_input =
            cx.new(|cx| NativeTextInput::new("Feedback para solicitar revisão do estágio", cx));
        let preparation_threshold_input =
            cx.new(|cx| NativeTextInput::new("Threshold de 0 a 100", cx));
        let context_limit_input = cx
            .new(|cx| NativeTextInput::new("Orçamento de contexto: 2.000 a 80.000 caracteres", cx));
        let vault_search_input = cx.new(|cx| NativeTextInput::new("Texto ou regex", cx));
        let vault_rename_input = cx.new(|cx| NativeTextInput::new("Novo nome da nota", cx));
        let settings_connection_name_input =
            cx.new(|cx| NativeTextInput::new("Nome da conexão", cx));
        let settings_connection_model_input =
            cx.new(|cx| NativeTextInput::new("Modelo padrão", cx));
        let settings_connection_endpoint_input =
            cx.new(|cx| NativeTextInput::new("Endpoint customizado", cx));
        let settings_connection_secret_input = cx
            .new(|cx| NativeTextInput::new("API key (não será exibida novamente)", cx).obscured());
        let settings_route_name_input = cx.new(|cx| NativeTextInput::new("Nome da rota", cx));
        let settings_embedding_model_input =
            cx.new(|cx| NativeTextInput::new("Modelo de embeddings", cx));
        let chat_draft_input = cx.new(|cx| NativeTextInput::new("Mensagem para o Companion", cx));
        let subscriptions = vec![
            cx.observe(&editor, |model, _, cx| {
                model.handle_editor_update(cx);
                model.schedule_vault_context_documents(cx);
                model.schedule_session_persistence(cx);
            }),
            cx.observe(&pulsation_feature_input, |_, _, cx| cx.notify()),
            cx.observe(&reconciliation_input, |_, _, cx| cx.notify()),
            cx.observe(&description_review_input, |_, _, cx| cx.notify()),
            cx.observe(&stage_feedback_input, |_, _, cx| cx.notify()),
            cx.observe(&preparation_threshold_input, |_, _, cx| cx.notify()),
            cx.observe(&context_limit_input, |_, _, cx| cx.notify()),
            cx.observe(&vault_search_input, |_, _, cx| cx.notify()),
            cx.observe(&vault_rename_input, |_, _, cx| cx.notify()),
            cx.observe(&settings_connection_name_input, |_, _, cx| cx.notify()),
            cx.observe(&settings_connection_model_input, |_, _, cx| cx.notify()),
            cx.observe(&settings_connection_endpoint_input, |_, _, cx| cx.notify()),
            cx.observe(&settings_connection_secret_input, |_, _, cx| cx.notify()),
            cx.observe(&settings_route_name_input, |_, _, cx| cx.notify()),
            cx.observe(&settings_embedding_model_input, |_, _, cx| cx.notify()),
            cx.observe(&chat_draft_input, |_, _, cx| cx.notify()),
        ];
        Self {
            core,
            session_persistence,
            workspace_backend,
            pulsations_backend: None,
            vault_backend: None,
            settings_backend: None,
            logs_backend: None,
            chat_backend: None,
            navigation,
            workspace,
            pulsations: cx.new(|_| PulsationsModel::default()),
            vault: cx.new(|_| VaultModel::default()),
            pulsation_feature_input,
            reconciliation_input,
            description_review_input,
            stage_feedback_input,
            preparation_threshold_input,
            context_limit_input,
            specification_answer_inputs: Vec::new(),
            preparation_scope: None,
            question_subscriptions: Vec::new(),
            artifact_preview_page: 0,
            vault_search_input,
            vault_rename_input,
            settings_connection_name_input,
            settings_connection_model_input,
            settings_connection_endpoint_input,
            settings_connection_secret_input,
            settings_route_name_input,
            settings_embedding_model_input,
            chat_draft_input,
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
            brownfield_task: None,
            brownfield_generation: 0,
            file_tasks: HashMap::new(),
            autosave_tasks: HashMap::new(),
            hitl_task: None,
            pulsations_task: None,
            reconciliation_task: None,
            preparation_action_task: None,
            artifact_preview_task: None,
            pulsation_artifacts_task: None,
            vault_refresh_task: None,
            vault_search_task: None,
            vault_rename_task: None,
            vault_context_task: None,
            vault_document_tasks: HashMap::new(),
            settings_task: None,
            logs_task: None,
            logs_resync_tasks: HashMap::new(),
            chat_task: None,
            chat_resync_tasks: HashMap::new(),
            session_task: None,
            performance_probe: None,
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

    pub(crate) fn observe_performance_frame(&mut self, window: &Window) {
        if self
            .performance_probe
            .as_mut()
            .is_some_and(|probe| probe.observe_render(window))
        {
            self.performance_probe = None;
        }
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
        if view == AppView::Chat {
            let project_id = self
                .workspace
                .read(cx)
                .project_id
                .as_ref()
                .map(ToString::to_string);
            let active_conversation = self
                .chat
                .read(cx)
                .active_conversation
                .as_ref()
                .map(ToString::to_string);
            if let Some(project_id) = project_id {
                self.hydrate_chat_project(project_id, active_conversation, cx);
            }
        }
        match view {
            AppView::Settings => self.refresh_settings(cx),
            AppView::Logs => self.refresh_logs(cx),
            _ => {}
        }
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

    pub fn set_brownfield_mapping_enabled(&mut self, consent: bool, cx: &mut Context<Self>) {
        let Some(backend) = self.workspace_backend.clone() else {
            self.status = "Onboarding brownfield indisponível no ambiente de teste".into();
            cx.notify();
            return;
        };
        let (project_id, busy, state) = {
            let workspace = self.workspace.read(cx);
            (
                workspace.project_id.as_ref().map(ToString::to_string),
                workspace.brownfield.busy,
                workspace.brownfield.state,
            )
        };
        let Some(project_id) = project_id else {
            return;
        };
        if busy || state == BrownfieldOnboardingState::NotApplicable {
            return;
        }

        self.workspace.update(cx, |workspace, _| {
            workspace.brownfield.busy = true;
            workspace.brownfield.error = None;
        });
        self.brownfield_generation = self.brownfield_generation.saturating_add(1);
        let generation = self.brownfield_generation;
        let requested_project = project_id.clone();
        self.status = if consent {
            "Executando inventário brownfield limitado…".into()
        } else {
            "Revogando consentimento brownfield…".into()
        };
        cx.notify();

        self.brownfield_task = Some(cx.spawn(async move |this, cx| {
            let result = backend.set_brownfield_consent(project_id, consent).await;
            let _ = this.update(cx, |model, cx| {
                if generation != model.brownfield_generation
                    || model
                        .workspace
                        .read(cx)
                        .project_id
                        .as_ref()
                        .is_none_or(|active| active.as_ref() != requested_project)
                {
                    return;
                }
                let error_message = result.as_ref().err().map(|error| error.presentable());
                model.workspace.update(cx, |workspace, _| match result {
                    Ok(result) => workspace.brownfield.apply_result(result),
                    Err(error) => {
                        workspace.brownfield.busy = false;
                        workspace.brownfield.error = Some(error.presentable());
                    }
                });
                model.status = match error_message {
                    Some(error) => format!("Falha no onboarding brownfield: {error}").into(),
                    None if consent => {
                        "Inventário concluído; mapeamento estrutural agendado".into()
                    }
                    None => "Mapeamento brownfield desativado".into(),
                };
                cx.notify();
            });
        }));
    }

    pub(crate) fn set_active_brownfield_ignored(&mut self, ignored: bool, cx: &mut Context<Self>) {
        let Some(backend) = self.workspace_backend.clone() else {
            return;
        };
        let (project_id, relative_path, busy, enabled) = {
            let workspace = self.workspace.read(cx);
            (
                workspace.project_id.as_ref().map(ToString::to_string),
                workspace
                    .active_relative_path
                    .as_ref()
                    .map(ToString::to_string),
                workspace.brownfield.busy,
                workspace.brownfield.state == BrownfieldOnboardingState::Enabled,
            )
        };
        let (Some(project_id), Some(relative_path)) = (project_id, relative_path) else {
            return;
        };
        if busy || !enabled {
            return;
        }
        self.workspace.update(cx, |workspace, _| {
            workspace.brownfield.busy = true;
            workspace.brownfield.error = None;
        });
        self.brownfield_generation = self.brownfield_generation.saturating_add(1);
        let generation = self.brownfield_generation;
        let requested_project = project_id.clone();
        self.status = if ignored {
            format!("Ignorando `{relative_path}` no mapa brownfield…").into()
        } else {
            format!("Reativando `{relative_path}` no mapa brownfield…").into()
        };
        self.brownfield_task = Some(cx.spawn(async move |this, cx| {
            let result = backend
                .set_brownfield_ignored(project_id, relative_path.clone(), ignored)
                .await;
            let _ = this.update(cx, |model, cx| {
                if model.brownfield_generation != generation
                    || model
                        .workspace
                        .read(cx)
                        .project_id
                        .as_ref()
                        .is_none_or(|active| active.as_ref() != requested_project)
                {
                    return;
                }
                let error_message = result.as_ref().err().map(|error| error.presentable());
                model.workspace.update(cx, |workspace, _| {
                    workspace.brownfield.busy = false;
                    match result {
                        Ok(status) => {
                            workspace.brownfield.ignored_paths = status.ignored_paths;
                            workspace.brownfield.coverage = status.coverage;
                            workspace.brownfield.error = None;
                        }
                        Err(error) => {
                            workspace.brownfield.error = Some(error.presentable());
                        }
                    }
                });
                model.status = match error_message {
                    Some(error) => format!("Falha ao atualizar ignore brownfield: {error}").into(),
                    None if ignored => {
                        format!("`{relative_path}` será ignorado por scans futuros.").into()
                    }
                    None => {
                        format!("`{relative_path}` foi reativado e entrou na fila incremental.")
                            .into()
                    }
                };
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub fn open_workspace_file(
        &mut self,
        relative_path: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        self.open_workspace_file_at(relative_path, None, cx);
    }

    pub(crate) fn open_workspace_file_at(
        &mut self,
        relative_path: impl Into<String>,
        cursor: Option<TextPosition>,
        cx: &mut Context<Self>,
    ) {
        let relative_path = relative_path.into();
        if self.workspace.read(cx).tab(&relative_path).is_some() {
            self.select_workspace_tab(&relative_path, cx);
            if let Some(cursor) = cursor {
                self.apply_workspace_cursor(cursor, cx);
            }
            self.select_view(AppView::Workspace, cx);
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
                    if let Some(cursor) = cursor {
                        model.apply_workspace_cursor(cursor, cx);
                    }
                    model.select_view(AppView::Workspace, cx);
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

    fn apply_workspace_cursor(&mut self, cursor: TextPosition, cx: &mut Context<Self>) {
        let editor = self.workspace.read(cx).editor.clone();
        editor.update(cx, |editor, cx| {
            if editor
                .buffer
                .set_selection(EditorSelection::caret(cursor))
                .is_ok()
            {
                editor.buffer.set_scroll_top(cursor.line as f32 * 20.0);
                cx.notify();
            }
        });
        self.sync_active_editor(cx);
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
        self.pulsations.update(cx, |pulsations, _| {
            pulsations.set_workspace_override(view_mode);
        });
        self.workspace.update(cx, |workspace, _| {
            workspace.view_mode = view_mode;
            if view_mode == WorkspaceViewMode::Editor {
                workspace.diff_expanded = false;
            }
        });
        self.schedule_session_persistence(cx);
        cx.notify();
    }

    pub fn clear_workspace_view_override(&mut self, cx: &mut Context<Self>) {
        let view_mode = self.pulsations.update(cx, |pulsations, _| {
            pulsations.clear_workspace_override();
            pulsations.derived_workspace_mode()
        });
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

    pub(crate) fn refresh_pulsations(&mut self, cx: &mut Context<Self>) {
        let preferred = self.pulsations.read(cx).active_id();
        self.refresh_pulsations_with_preferred(preferred, cx);
    }

    fn refresh_pulsations_with_preferred(
        &mut self,
        preferred_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.pulsations_backend.clone() else {
            return;
        };
        let Some(project_id) = self
            .workspace
            .read(cx)
            .project_id
            .as_ref()
            .map(ToString::to_string)
        else {
            return;
        };
        self.pulsations.update(cx, |pulsations, _| {
            pulsations.runtime_loading = true;
            pulsations.error = None;
        });
        self.pulsations_task = Some(cx.spawn(async move |this, cx| {
            let result = backend.runtime_snapshot(project_id.clone()).await;
            let _ = this.update(cx, |model, cx| match result {
                Ok(snapshot) => {
                    let active_id = model.apply_pulsation_snapshot(
                        project_id,
                        snapshot,
                        preferred_id.as_deref(),
                        cx,
                    );
                    if let Some(active_id) = active_id {
                        model.refresh_pulsation_artifacts(active_id, cx);
                    }
                }
                Err(error) => {
                    model.pulsations.update(cx, |pulsations, _| {
                        pulsations.runtime_loading = false;
                        pulsations.error = Some(error.clone());
                    });
                    model.notices.push(HeaderNoticeKind::Warning, error);
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    fn apply_pulsation_snapshot(
        &mut self,
        project_id: String,
        snapshot: PulsationRuntimeSnapshot,
        preferred_id: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        let (active_id, workspace_mode) = self.pulsations.update(cx, |pulsations, _| {
            let active_id = pulsations.apply_snapshot(project_id, snapshot, preferred_id);
            (active_id, pulsations.derived_workspace_mode())
        });
        self.sync_preparation_inputs(cx);
        self.workspace.update(cx, |workspace, _| {
            workspace.view_mode = workspace_mode;
            if workspace_mode == WorkspaceViewMode::Editor {
                workspace.diff_expanded = false;
            }
        });
        cx.notify();
        active_id
    }

    fn refresh_pulsation_artifacts(&mut self, pulsation_id: String, cx: &mut Context<Self>) {
        let Some(backend) = self.pulsations_backend.clone() else {
            return;
        };
        self.pulsations.update(cx, |pulsations, _| {
            pulsations.artifacts_loading = true;
        });
        self.pulsation_artifacts_task = Some(cx.spawn(async move |this, cx| {
            let result = backend.artifacts(pulsation_id.clone()).await;
            let _ = this.update(cx, |model, cx| {
                model.pulsations.update(cx, |pulsations, _| {
                    pulsations.finish_artifacts(&pulsation_id, result);
                });
                cx.notify();
            });
        }));
    }

    pub(crate) fn select_pulsation(
        &mut self,
        pulsation_id: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        let pulsation_id = pulsation_id.into();
        let (selected, workspace_mode) = self.pulsations.update(cx, |pulsations, _| {
            let selected = pulsations.select(&pulsation_id);
            (selected, pulsations.derived_workspace_mode())
        });
        if !selected {
            return;
        }
        self.sync_preparation_inputs(cx);
        self.workspace.update(cx, |workspace, _| {
            workspace.view_mode = workspace_mode;
            workspace.diff_expanded = false;
        });
        self.refresh_pulsation_artifacts(pulsation_id, cx);
        self.refresh_logs(cx);
        self.schedule_session_persistence(cx);
        cx.notify();
    }

    pub(crate) fn create_pulsation(&mut self, cx: &mut Context<Self>) {
        let feature_name = self.pulsation_feature_input.read(cx).value().to_owned();
        if feature_name.trim().is_empty() {
            return;
        }
        let Some(project_id) = self
            .workspace
            .read(cx)
            .project_id
            .as_ref()
            .map(ToString::to_string)
        else {
            return;
        };
        let Some(backend) = self.pulsations_backend.clone() else {
            return;
        };
        self.pulsations.update(cx, |pulsations, _| {
            pulsations.runtime_loading = true;
            pulsations.notice = None;
            pulsations.error = None;
        });
        self.pulsations_task = Some(cx.spawn(async move |this, cx| {
            let result = match backend
                .create(project_id.clone(), feature_name.clone())
                .await
            {
                Ok(created) => backend
                    .runtime_snapshot(project_id.clone())
                    .await
                    .map(|snapshot| (created, snapshot)),
                Err(error) => Err(error),
            };
            let _ = this.update(cx, |model, cx| match result {
                Ok((created, snapshot)) => {
                    let active_id =
                        model.apply_pulsation_snapshot(project_id, snapshot, Some(&created.id), cx);
                    model.pulsations.update(cx, |pulsations, _| {
                        pulsations.notice =
                            Some(format!("Pulsação “{}” iniciada.", created.feature_name));
                    });
                    model
                        .pulsation_feature_input
                        .update(cx, |input, cx| input.clear(cx));
                    if let Some(active_id) = active_id {
                        model.refresh_pulsation_artifacts(active_id, cx);
                    }
                    model.schedule_session_persistence(cx);
                }
                Err(error) => {
                    model.pulsations.update(cx, |pulsations, _| {
                        pulsations.runtime_loading = false;
                        pulsations.error = Some(error.clone());
                    });
                    model.notices.push(HeaderNoticeKind::Error, error);
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    pub(crate) fn reconcile_project_base(&mut self, cx: &mut Context<Self>) {
        if self.reconciliation_task.is_some() || self.lifecycle.is_disposed() {
            return;
        }
        let Some(backend) = self.pulsations_backend.clone() else {
            return;
        };
        let explanation = self.reconciliation_input.read(cx).value().to_owned();
        let Some(request) = self
            .pulsations
            .update(cx, |p, _| p.begin_reconciliation(explanation))
        else {
            return;
        };
        let project = request.project_id.clone();
        // Separate from polling/refresh: a refresh must not detach an in-flight confirmation.
        self.reconciliation_task = Some(cx.spawn(async move |this, cx| {
            let result = backend.reconcile(request).await;
            let _ = this.update(cx, |model, cx| {
                model.reconciliation_task = None;
                if model.lifecycle.is_disposed() {
                    return;
                }
                if model
                    .pulsations
                    .read(cx)
                    .project_id
                    .as_ref()
                    .is_none_or(|id| id.as_ref() != project.as_str())
                {
                    return;
                }
                model.pulsations.update(cx, |p, _| {
                    p.reconciliation_busy = false;
                    match &result {
                        Ok(()) => p.notice = Some(
                            "Base atual adotada. Saídas e pulsações anteriores foram preservadas."
                                .into(),
                        ),
                        Err(e) => p.error = Some(e.clone()),
                    }
                });
                if result.is_ok() {
                    model
                        .reconciliation_input
                        .update(cx, |input, cx| input.set_value("", cx));
                    model.refresh_pulsations(cx);
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(crate) fn cancel_active_pulsation_jobs(&mut self, cx: &mut Context<Self>) {
        let Some(pulsation_id) = self.pulsations.read(cx).active_id() else {
            return;
        };
        let Some(project_id) = self
            .workspace
            .read(cx)
            .project_id
            .as_ref()
            .map(ToString::to_string)
        else {
            return;
        };
        let Some(backend) = self.pulsations_backend.clone() else {
            return;
        };
        self.pulsations.update(cx, |pulsations, _| {
            pulsations.runtime_loading = true;
            pulsations.notice = None;
        });
        self.pulsations_task = Some(cx.spawn(async move |this, cx| {
            let result = match backend.cancel_jobs(pulsation_id.clone()).await {
                Ok(cancelled) => backend
                    .runtime_snapshot(project_id.clone())
                    .await
                    .map(|snapshot| (cancelled, snapshot)),
                Err(error) => Err(error),
            };
            let _ = this.update(cx, |model, cx| match result {
                Ok((cancelled, snapshot)) => {
                    let active_id = model.apply_pulsation_snapshot(
                        project_id,
                        snapshot,
                        Some(&pulsation_id),
                        cx,
                    );
                    model.pulsations.update(cx, |pulsations, _| {
                        pulsations.notice =
                            Some(format!("{cancelled} job(s) marcado(s) para cancelamento."));
                    });
                    if let Some(active_id) = active_id {
                        model.refresh_pulsation_artifacts(active_id, cx);
                    }
                }
                Err(error) => {
                    model.pulsations.update(cx, |pulsations, _| {
                        pulsations.runtime_loading = false;
                        pulsations.error = Some(error.clone());
                    });
                    model.notices.push(HeaderNoticeKind::Error, error);
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    pub(crate) fn refresh_vault(&mut self, force_rebuild: bool, cx: &mut Context<Self>) {
        if self.vault.read(cx).workspace_root.is_none() {
            return;
        }
        let Some(backend) = self.vault_backend.clone() else {
            return;
        };
        self.vault.update(cx, |vault, _| {
            vault.refreshing_tree = true;
            vault.indexing = true;
            vault.error = None;
        });
        self.vault_refresh_task = Some(cx.spawn(async move |this, cx| {
            let (tree, index) = tokio::join!(backend.tree(), backend.index(force_rebuild));
            let _ = this.update(cx, |model, cx| {
                let selected = model.vault.update(cx, |vault, _| {
                    vault.apply_tree(tree);
                    vault.apply_index(index);
                    vault.selected_path.as_ref().map(ToString::to_string)
                });
                if let Some(selected) = selected {
                    model.ensure_vault_documents(vec![selected], cx);
                }
                model.schedule_vault_context_documents(cx);
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn schedule_vault_refresh(&mut self, cx: &mut Context<Self>) {
        if self.vault.read(cx).workspace_root.is_none() {
            return;
        }
        self.vault.update(cx, |vault, _| vault.stale = true);
        let timer = cx.background_executor().timer(VAULT_REFRESH_DEBOUNCE);
        self.vault_refresh_task = Some(cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |model, cx| model.refresh_vault(false, cx));
        }));
        cx.notify();
    }

    pub(crate) fn set_vault_search_mode(
        &mut self,
        search_mode: SearchMode,
        cx: &mut Context<Self>,
    ) {
        self.vault.update(cx, |vault, _| {
            vault.search_mode = search_mode;
        });
        cx.notify();
    }

    pub(crate) fn toggle_vault_search_case(&mut self, cx: &mut Context<Self>) {
        self.vault.update(cx, |vault, _| {
            vault.search_case_sensitive = !vault.search_case_sensitive;
        });
        cx.notify();
    }

    pub(crate) fn search_vault(&mut self, cx: &mut Context<Self>) {
        let query = self.vault_search_input.read(cx).value().to_owned();
        let request = self.vault.update(cx, |vault, _| {
            vault.search_query = query;
            let request = vault.search_request();
            if request.is_some() {
                vault.searching = true;
                vault.error = None;
            } else {
                vault.search_response = None;
            }
            request
        });
        let Some(request) = request else {
            cx.notify();
            return;
        };
        let Some(backend) = self.vault_backend.clone() else {
            return;
        };
        self.vault_search_task = Some(cx.spawn(async move |this, cx| {
            let result = backend.search(request).await;
            let _ = this.update(cx, |model, cx| {
                model
                    .vault
                    .update(cx, |vault, _| vault.apply_search(result));
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(crate) fn clear_vault_search(&mut self, cx: &mut Context<Self>) {
        self.vault_search_task.take();
        self.vault_search_input
            .update(cx, |input, cx| input.clear(cx));
        self.vault.update(cx, |vault, _| {
            vault.search_query.clear();
            vault.search_response = None;
            vault.searching = false;
        });
        cx.notify();
    }

    pub(crate) fn select_vault_path(
        &mut self,
        relative_path: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        let relative_path = relative_path.into();
        self.vault.update(cx, |vault, _| {
            vault.select(Some(relative_path.clone()));
        });
        let file_name = relative_path.rsplit('/').next().unwrap_or(&relative_path);
        self.vault_rename_input
            .update(cx, |input, cx| input.set_value(file_name, cx));
        self.ensure_vault_documents(vec![relative_path], cx);
        self.schedule_session_persistence(cx);
        cx.notify();
    }

    pub(crate) fn ensure_vault_documents(
        &mut self,
        relative_paths: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.vault_backend.clone() else {
            return;
        };
        let mut unique = relative_paths;
        unique.sort();
        unique.dedup();
        for relative_path in unique {
            let should_load = self
                .vault
                .update(cx, |vault, _| vault.begin_document_load(&relative_path));
            if !should_load {
                continue;
            }
            let document_backend = Arc::clone(&backend);
            let task_path = relative_path.clone();
            let task = cx.spawn(async move |this, cx| {
                let result = document_backend.read(task_path.clone()).await;
                let _ = this.update(cx, |model, cx| {
                    model.vault.update(cx, |vault, _| {
                        vault.finish_document_load(task_path, result);
                    });
                    cx.notify();
                });
            });
            self.vault_document_tasks.insert(relative_path, task);
        }
        cx.notify();
    }

    fn schedule_vault_context_documents(&mut self, cx: &mut Context<Self>) {
        if self.vault.read(cx).workspace_root.is_none() {
            return;
        }
        let timer = cx.background_executor().timer(CONTEXT_DOCUMENTS_DEBOUNCE);
        self.vault_context_task = Some(cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |model, cx| {
                let active_path = model.active_workspace_path(cx);
                let paths = model.vault.read(cx).context_paths(active_path.as_deref());
                model.ensure_vault_documents(paths, cx);
            });
        }));
    }

    pub(crate) fn rename_selected_vault_note(&mut self, cx: &mut Context<Self>) {
        let Some(from_relative_path) = self
            .vault
            .read(cx)
            .selected_path
            .as_ref()
            .map(ToString::to_string)
        else {
            return;
        };
        let requested_name = self.vault_rename_input.read(cx).value().to_owned();
        let target = match normalize_rename_target(&from_relative_path, &requested_name) {
            Ok(Some(target)) => target,
            Ok(None) => return,
            Err(error) => {
                self.vault.update(cx, |vault, _| vault.error = Some(error));
                cx.notify();
                return;
            }
        };
        if self
            .workspace
            .read(cx)
            .tab(&from_relative_path)
            .is_some_and(|tab| tab.dirty || tab.saving)
        {
            self.vault.update(cx, |vault, _| {
                vault.error = Some(
                    "Salve a nota aberta antes de renomeá-la para evitar perda de alterações."
                        .into(),
                );
            });
            cx.notify();
            return;
        }
        let Some(backend) = self.vault_backend.clone() else {
            return;
        };
        let was_open = self.workspace.read(cx).tab(&from_relative_path).is_some();
        self.vault.update(cx, |vault, _| {
            vault.renaming = true;
            vault.error = None;
        });
        self.vault_rename_task = Some(cx.spawn(async move |this, cx| {
            let renamed = backend
                .rename(hvc_core::workspace_service::RenameWorkspaceNoteRequest {
                    from_relative_path: from_relative_path.clone(),
                    to_relative_path: target,
                })
                .await;
            let result = match renamed {
                Ok(response) => {
                    let (tree, index) = tokio::join!(backend.tree(), backend.index(true));
                    Ok((response, tree, index))
                }
                Err(error) => Err(error),
            };
            let _ = this.update(cx, |model, cx| match result {
                Ok((response, tree, index)) => {
                    let renamed_to = response.renamed_to.clone();
                    model.vault.update(cx, |vault, _| {
                        vault.apply_rename(&from_relative_path, &response);
                        vault.apply_tree(tree);
                        vault.apply_index(index);
                    });
                    if was_open {
                        model.close_workspace_tab(&from_relative_path, cx);
                        model.open_workspace_file(renamed_to.clone(), cx);
                    }
                    let renamed_name = renamed_to
                        .rsplit('/')
                        .next()
                        .unwrap_or(&renamed_to)
                        .to_owned();
                    model
                        .vault_rename_input
                        .update(cx, |input, cx| input.set_value(renamed_name, cx));
                    model.schedule_session_persistence(cx);
                    cx.notify();
                }
                Err(error) => {
                    model.vault.update(cx, |vault, _| {
                        vault.renaming = false;
                        vault.error = Some(error.clone());
                    });
                    model.notices.push(HeaderNoticeKind::Error, error);
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    pub(crate) fn toggle_vault_graph(&mut self, cx: &mut Context<Self>) {
        self.vault.update(cx, |vault, _| {
            vault.graph_expanded = !vault.graph_expanded;
        });
        cx.notify();
    }

    pub(crate) fn apply_hitl_decision(&mut self, decision: HitlDecision, cx: &mut Context<Self>) {
        let request = self
            .pulsations
            .update(cx, |pulsations, _| pulsations.hitl.begin(decision));
        let request = request.map(|mut request| {
            if decision == HitlDecision::RequestRevision {
                request.feedback = Some(self.stage_feedback_input.read(cx).value().to_owned());
            }
            request
        });
        let Some(request) = request else {
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
                        .apply_hitl_decision(request)
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                })
                .await
                .map_err(|error| format!("worker HITL interrompido: {error}"))
                .and_then(|result| result);
            let _ = this.update(cx, |model, cx| {
                if model.lifecycle.is_disposed() {
                    return;
                }
                model.pulsations.update(cx, |pulsations, _| {
                    pulsations.hitl.finish(result.clone());
                    if result.is_ok() {
                        pulsations.awaiting_hitl = pulsations.awaiting_hitl.saturating_sub(1);
                        pulsations.clear_workspace_override();
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
                        model.refresh_pulsations(cx);
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

    fn hydrate_chat_project(
        &mut self,
        project_id: String,
        restored_conversation_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let generation = self.chat.update(cx, |chat, _| {
            chat.begin_hydration(project_id.clone(), restored_conversation_id.clone())
        });
        let Some(backend) = self.chat_backend.clone() else {
            self.chat.update(cx, |chat, _| chat.loading = false);
            return;
        };
        self.chat_task = Some(cx.spawn(async move |this, cx| {
            let result = backend
                .hydrate(project_id.clone(), restored_conversation_id)
                .await;
            let _ = this.update(cx, |model, cx| {
                let applied = match result {
                    Ok(hydration) => model
                        .chat
                        .update(cx, |chat, _| chat.apply_hydration(generation, hydration)),
                    Err(error) => model
                        .chat
                        .update(cx, |chat, _| chat.fail_hydration(generation, error)),
                };
                if applied {
                    model.schedule_session_persistence(cx);
                }
                cx.notify();
            });
        }));
    }

    pub(crate) fn select_chat_conversation(
        &mut self,
        conversation_id: String,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.chat_backend.clone() else {
            return;
        };
        let Some(project_id) = self
            .chat
            .read(cx)
            .project_id
            .as_ref()
            .map(ToString::to_string)
        else {
            return;
        };
        if !self.chat.update(cx, |chat, _| {
            chat.select_conversation_locally(&conversation_id)
        }) {
            return;
        }
        self.schedule_session_persistence(cx);
        cx.notify();
        self.chat_task = Some(cx.spawn(async move |this, cx| {
            let result = backend.load_conversation(conversation_id.clone()).await;
            let _ = this.update(cx, |model, cx| {
                match result {
                    Ok(snapshot) => {
                        model.chat.update(cx, |chat, _| {
                            chat.apply_selected_conversation(
                                &project_id,
                                &conversation_id,
                                snapshot,
                            );
                        });
                    }
                    Err(error) => model.chat.update(cx, |chat, _| chat.set_error(error)),
                }
                cx.notify();
            });
        }));
    }

    pub(crate) fn cycle_chat_connection(&mut self, cx: &mut Context<Self>) {
        self.chat.update(cx, |chat, _| {
            let eligible = chat
                .eligible_connections()
                .map(|connection| connection.id.clone())
                .collect::<Vec<_>>();
            if eligible.is_empty() {
                chat.selected_connection_id = None;
                return;
            }
            let current = chat
                .selected_connection_id
                .as_ref()
                .and_then(|selected| eligible.iter().position(|id| id == selected.as_ref()))
                .unwrap_or(eligible.len().saturating_sub(1));
            chat.selected_connection_id =
                Some(eligible[(current + 1) % eligible.len()].clone().into());
        });
        cx.notify();
    }

    pub(crate) fn create_chat_conversation(&mut self, cx: &mut Context<Self>) {
        let Some(backend) = self.chat_backend.clone() else {
            return;
        };
        let (Some(project_id), Some(connection_id)) = ({
            let chat = self.chat.read(cx);
            (
                chat.project_id.as_ref().map(ToString::to_string),
                chat.selected_connection_id
                    .as_ref()
                    .map(ToString::to_string),
            )
        }) else {
            return;
        };
        if !self.chat.update(cx, |chat, _| chat.begin_busy()) {
            return;
        }
        cx.notify();
        self.chat_task = Some(cx.spawn(async move |this, cx| {
            let result = backend.create_conversation(project_id, connection_id).await;
            let _ = this.update(cx, |model, cx| {
                model.chat.update(cx, |chat, _| {
                    chat.finish_busy();
                    match result {
                        Ok(snapshot) => chat.apply_created(snapshot),
                        Err(error) => chat.set_error(error),
                    }
                });
                model
                    .chat_draft_input
                    .update(cx, |input, cx| input.clear(cx));
                model.schedule_session_persistence(cx);
                cx.notify();
            });
        }));
    }

    pub(crate) fn delete_chat_conversation(
        &mut self,
        conversation_id: String,
        cx: &mut Context<Self>,
    ) {
        let confirmed = self
            .chat
            .read(cx)
            .pending_delete_conversation_id
            .as_ref()
            .is_some_and(|pending| pending.as_ref() == conversation_id);
        if !confirmed {
            self.chat.update(cx, |chat, _| {
                chat.pending_delete_conversation_id = Some(conversation_id.into());
            });
            cx.notify();
            return;
        }
        let Some(backend) = self.chat_backend.clone() else {
            return;
        };
        if !self.chat.update(cx, |chat, _| chat.begin_busy()) {
            return;
        }
        cx.notify();
        self.chat_task = Some(cx.spawn(async move |this, cx| {
            let result = backend.delete_conversation(conversation_id.clone()).await;
            let _ = this.update(cx, |model, cx| {
                let next = model.chat.update(cx, |chat, _| {
                    chat.finish_busy();
                    match result {
                        Ok(true) => chat.apply_deleted(&conversation_id),
                        Ok(false) => {
                            chat.set_error(crate::errors::UiCommandError::from_chat_parts(
                                "conversation_not_found",
                                "Conversa não encontrada.",
                            ));
                            None
                        }
                        Err(error) => {
                            chat.set_error(error);
                            None
                        }
                    }
                });
                if let Some(next) = next {
                    model.select_chat_conversation(next, cx);
                }
                model.schedule_session_persistence(cx);
                cx.notify();
            });
        }));
    }

    pub(crate) fn acknowledge_chat_disclosure(&mut self, cx: &mut Context<Self>) {
        self.chat
            .update(cx, |chat, _| chat.acknowledge_disclosure());
        cx.notify();
    }

    pub(crate) fn send_chat_message(&mut self, cx: &mut Context<Self>) {
        let Some(backend) = self.chat_backend.clone() else {
            return;
        };
        let content = self.chat_draft_input.read(cx).value().trim().to_string();
        let conversation_id = {
            let chat = self.chat.read(cx);
            if content.is_empty()
                || !chat.composer_available()
                || !chat.disclosure_accepted()
                || chat.streaming
                || chat
                    .active_conversation_record()
                    .is_some_and(|conversation| !conversation.connection_available)
            {
                return;
            }
            chat.active_conversation.as_ref().map(ToString::to_string)
        };
        let Some(conversation_id) = conversation_id else {
            return;
        };
        if !self.chat.update(cx, |chat, _| chat.begin_busy()) {
            return;
        }
        self.chat_draft_input
            .update(cx, |input, cx| input.clear(cx));
        cx.notify();
        self.chat_task = Some(cx.spawn(async move |this, cx| {
            let result = backend
                .send_message(conversation_id.clone(), content.clone())
                .await;
            let _ = this.update(cx, |model, cx| {
                model.chat.update(cx, |chat, _| {
                    chat.finish_busy();
                    match result {
                        Ok((started, snapshot)) => {
                            chat.apply_started_snapshot(
                                &conversation_id,
                                &started.generation_id,
                                snapshot,
                            );
                        }
                        Err(error) => chat.set_error(error),
                    }
                });
                if model.chat.read(cx).error.is_some()
                    && model.chat_draft_input.read(cx).value().is_empty()
                {
                    model
                        .chat_draft_input
                        .update(cx, |input, cx| input.set_value(content, cx));
                }
                model.schedule_session_persistence(cx);
                cx.notify();
            });
        }));
    }

    pub(crate) fn cancel_chat_turn(&mut self, cx: &mut Context<Self>) {
        let Some(backend) = self.chat_backend.clone() else {
            return;
        };
        let Some(execution_id) = self.chat.read(cx).active_execution_id() else {
            return;
        };
        self.chat_task = Some(cx.spawn(async move |this, cx| {
            let result = backend.cancel_turn(execution_id).await;
            let _ = this.update(cx, |model, cx| {
                if let Err(error) = result {
                    model.chat.update(cx, |chat, _| chat.set_error(error));
                }
                cx.notify();
            });
        }));
    }

    pub(crate) fn copy_chat_diff(&mut self, diff: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(diff));
        self.status = "Diff copiado; nenhuma alteração foi aplicada".into();
        cx.notify();
    }

    fn handle_chat_event(&mut self, envelope: ChatEventEnvelope, cx: &mut Context<Self>) {
        let outcome = self.chat.update(cx, |chat, _| chat.consume_event(envelope));
        match outcome {
            ChatEventOutcome::Ignored | ChatEventOutcome::Applied => {}
            ChatEventOutcome::Reconcile {
                generation_id,
                conversation_id,
            } => {
                self.needs_resync = true;
                self.resync_chat_turn(generation_id, conversation_id, cx);
            }
        }
    }

    fn resync_chat_turn(
        &mut self,
        generation_id: String,
        conversation_id: String,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.chat_backend.clone() else {
            return;
        };
        let Some(project_id) = self
            .chat
            .read(cx)
            .project_id
            .as_ref()
            .map(ToString::to_string)
        else {
            return;
        };
        let task_key = generation_id.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = backend.turn_snapshot(generation_id.clone()).await;
            let _ = this.update(cx, |model, cx| {
                match result {
                    Ok(snapshot) => {
                        let applied = model.chat.update(cx, |chat, _| {
                            chat.apply_turn_snapshot(
                                &project_id,
                                &generation_id,
                                &conversation_id,
                                snapshot,
                            )
                        });
                        if applied {
                            model.needs_resync = false;
                            model.status = "Turno do Chat ressincronizado".into();
                        }
                    }
                    Err(error) => model.chat.update(cx, |chat, _| chat.set_error(error)),
                }
                cx.notify();
            });
        });
        self.chat_resync_tasks.insert(task_key, task);
    }

    pub(crate) fn refresh_settings(&mut self, cx: &mut Context<Self>) {
        if self.lifecycle.is_disposed() {
            return;
        }
        let Some(backend) = self.settings_backend.clone() else {
            return;
        };
        let generation = self.settings.update(cx, |settings, _| settings.begin(true));
        cx.notify();
        self.settings_task = Some(cx.spawn(async move |this, cx| {
            let result = backend.snapshot().await;
            let _ = this.update(cx, |model, cx| {
                if model.lifecycle.is_disposed() {
                    return;
                }
                match result {
                    Ok(snapshot) => {
                        let embedding_model = snapshot.config.embedding_model.clone();
                        model.fill_preparation_threshold(&snapshot, cx);
                        let applied = model.settings.update(cx, |settings, _| {
                            settings.apply_snapshot(generation, snapshot)
                        });
                        if applied
                            && model
                                .settings_embedding_model_input
                                .read(cx)
                                .value()
                                .is_empty()
                        {
                            model
                                .settings_embedding_model_input
                                .update(cx, |input, cx| {
                                    input.set_value(embedding_model, cx);
                                });
                        }
                    }
                    Err(error) => {
                        model
                            .settings
                            .update(cx, |settings, _| settings.fail(generation, error));
                    }
                }
                cx.notify();
            });
        }));
    }

    fn run_settings_operation(
        &mut self,
        operation: crate::settings::SettingsFuture<String>,
        cx: &mut Context<Self>,
    ) {
        if self.lifecycle.is_disposed() {
            return;
        }
        let generation = self
            .settings
            .update(cx, |settings, _| settings.begin(false));
        cx.notify();
        self.settings_task = Some(cx.spawn(async move |this, cx| {
            let result = operation.await;
            let _ = this.update(cx, |model, cx| {
                if model.lifecycle.is_disposed() {
                    return;
                }
                match result {
                    Ok(message) => {
                        let current = model.settings.read(cx).generation;
                        if current == generation {
                            model.settings.update(cx, |settings, _| {
                                settings.notice = Some(message.into());
                            });
                            model.refresh_settings(cx);
                        }
                    }
                    Err(error) => {
                        let message = error.presentable();
                        if model
                            .settings
                            .update(cx, |settings, _| settings.fail(generation, error))
                        {
                            model.notices.push(HeaderNoticeKind::Error, message);
                        }
                        cx.notify();
                    }
                }
            });
        }));
    }

    fn update_runtime_config(
        &mut self,
        request: UpdateAgentRuntimeConfigRequest,
        success: &'static str,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.settings_backend.clone() else {
            return;
        };
        self.run_settings_operation(
            Box::pin(async move {
                backend.update_config(request).await?;
                Ok(success.to_string())
            }),
            cx,
        );
    }

    pub(crate) fn cycle_routing_mode(&mut self, cx: &mut Context<Self>) {
        let current = self
            .settings
            .read(cx)
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.config.routing_v2_mode)
            .unwrap_or(RoutingMode::Off);
        let next = match current {
            RoutingMode::Off => RoutingMode::Shadow,
            RoutingMode::Shadow => RoutingMode::On,
            RoutingMode::On => RoutingMode::Off,
        };
        self.update_runtime_config(
            runtime_config_update().with_routing_mode(next),
            "Modo de roteamento atualizado.",
            cx,
        );
    }

    pub(crate) fn toggle_context_strategy(&mut self, cx: &mut Context<Self>) {
        let current = self
            .settings
            .read(cx)
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.config.context_strategy)
            .unwrap_or(ContextStrategy::YamlGraph);
        let next = match current {
            ContextStrategy::YamlGraph => ContextStrategy::YamlGraphRag,
            ContextStrategy::YamlGraphRag => ContextStrategy::YamlGraph,
        };
        self.update_runtime_config(
            runtime_config_update().with_context_strategy(next),
            "Estratégia de contexto atualizada.",
            cx,
        );
    }

    pub(crate) fn toggle_embeddings(&mut self, cx: &mut Context<Self>) {
        let enabled = self
            .settings
            .read(cx)
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.config.embedding_enabled);
        self.update_runtime_config(
            runtime_config_update().with_embedding_enabled(!enabled),
            "Configuração de embeddings atualizada.",
            cx,
        );
    }

    pub(crate) fn reindex_embeddings(&mut self, cx: &mut Context<Self>) {
        let Some(backend) = self.settings_backend.clone() else {
            return;
        };
        let Some(workspace_path) = self
            .workspace
            .read(cx)
            .root_path
            .as_ref()
            .map(ToString::to_string)
        else {
            self.notices.push(
                HeaderNoticeKind::Warning,
                "Abra um workspace antes de reindexar embeddings.",
            );
            cx.notify();
            return;
        };
        let model = self
            .settings_embedding_model_input
            .read(cx)
            .value()
            .trim()
            .to_string();
        self.run_settings_operation(
            Box::pin(async move {
                if !model.is_empty() {
                    backend
                        .update_config(runtime_config_update().with_embedding_model(model.clone()))
                        .await?;
                }
                let response = backend
                    .reindex(workspace_path, (!model.is_empty()).then_some(model))
                    .await?;
                Ok(format!("Reconciliação de embeddings solicitada para {}. O supervisor executará os deltas em segundo plano.", response.workspace_path))
            }),
            cx,
        );
    }

    pub(crate) fn cycle_embedding_top_k(&mut self, cx: &mut Context<Self>) {
        let current = self
            .settings
            .read(cx)
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.config.embedding_top_k)
            .unwrap_or(5);
        let next = match current {
            1..=4 => 5,
            5..=9 => 10,
            10..=19 => 20,
            _ => 3,
        };
        self.update_runtime_config(
            runtime_config_update().with_embedding_top_k(next),
            "Top K de embeddings atualizado.",
            cx,
        );
    }

    pub(crate) fn cycle_settings_provider(&mut self, cx: &mut Context<Self>) {
        self.settings.update(cx, |settings, _| {
            let ids = settings
                .snapshot
                .as_ref()
                .map(|snapshot| {
                    snapshot
                        .profiles
                        .iter()
                        .map(|profile| profile.id.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if let Some(index) = ids
                .iter()
                .position(|id| id == settings.connection_provider_id.as_ref())
                && let Some(next) = ids.get((index + 1) % ids.len().max(1))
            {
                settings.connection_provider_id = next.clone().into();
            }
        });
        cx.notify();
    }

    pub(crate) fn select_settings_connection(
        &mut self,
        connection_id: String,
        cx: &mut Context<Self>,
    ) {
        let connection = self.settings.update(cx, |settings, _| {
            settings.selected_connection_id = Some(connection_id.into());
            settings.connection().cloned()
        });
        if let Some(connection) = connection {
            self.settings.update(cx, |settings, _| {
                settings.connection_provider_id = connection.provider_id.clone().into();
            });
            self.settings_connection_name_input
                .update(cx, |input, cx| input.set_value(connection.display_name, cx));
            self.settings_connection_model_input
                .update(cx, |input, cx| {
                    input.set_value(connection.default_model, cx)
                });
            self.settings_connection_endpoint_input
                .update(cx, |input, cx| {
                    input.set_value(connection.endpoint_url.unwrap_or_default(), cx)
                });
        }
        cx.notify();
    }

    pub(crate) fn new_settings_connection(&mut self, cx: &mut Context<Self>) {
        self.settings.update(cx, |settings, _| {
            settings.selected_connection_id = None;
        });
        for input in [
            &self.settings_connection_name_input,
            &self.settings_connection_model_input,
            &self.settings_connection_endpoint_input,
            &self.settings_connection_secret_input,
        ] {
            input.update(cx, |input, cx| input.clear(cx));
        }
        cx.notify();
    }

    pub(crate) fn save_settings_connection(&mut self, cx: &mut Context<Self>) {
        let Some(backend) = self.settings_backend.clone() else {
            return;
        };
        let (existing, provider_id, endpoint_allowed, default_model) = {
            let settings = self.settings.read(cx);
            let provider_id = settings.connection_provider_id.to_string();
            let profile = settings
                .snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.profiles.iter().find(|item| item.id == provider_id));
            (
                settings.connection().cloned(),
                provider_id,
                profile.is_some_and(|item| item.endpoint_configurable),
                profile
                    .map(|item| item.default_model.clone())
                    .unwrap_or_default(),
            )
        };
        let display_name = self
            .settings_connection_name_input
            .read(cx)
            .value()
            .trim()
            .to_string();
        let model = self
            .settings_connection_model_input
            .read(cx)
            .value()
            .trim()
            .to_string();
        let endpoint = self
            .settings_connection_endpoint_input
            .read(cx)
            .value()
            .trim()
            .to_string();
        let secret = self
            .settings_connection_secret_input
            .read(cx)
            .value()
            .to_string();
        self.settings_connection_secret_input
            .update(cx, |input, cx| input.clear(cx));
        let request = UpsertProviderConnectionRequest {
            id: existing.as_ref().map(|item| item.id.clone()),
            provider_id,
            display_name: if display_name.is_empty() {
                "Conexão LLM".to_string()
            } else {
                display_name
            },
            endpoint_url: endpoint_allowed
                .then_some(endpoint)
                .filter(|value| !value.is_empty()),
            default_model: if model.is_empty() {
                default_model
            } else {
                model
            },
            enabled: Some(existing.as_ref().is_none_or(|item| item.enabled)),
            max_concurrency: Some(existing.as_ref().map_or(2, |item| item.max_concurrency)),
            api_key: (!secret.trim().is_empty()).then_some(secret),
            version: existing.as_ref().map(|item| item.version),
        };
        self.run_settings_operation(
            Box::pin(async move {
                backend.upsert_connection(request).await?;
                Ok("Conexão e credencial salvas com segurança.".to_string())
            }),
            cx,
        );
    }

    pub(crate) fn test_settings_connection(&mut self, cx: &mut Context<Self>) {
        let Some(backend) = self.settings_backend.clone() else {
            return;
        };
        let Some(connection_id) = self
            .settings
            .read(cx)
            .selected_connection_id
            .as_ref()
            .map(ToString::to_string)
        else {
            return;
        };
        self.run_settings_operation(
            Box::pin(async move {
                let response = backend
                    .test_connection(TestProviderConnectionRequest { connection_id })
                    .await?;
                Ok(format!(
                    "Conexão {} em {} ms: {}",
                    if response.reachable {
                        "alcançável"
                    } else {
                        "indisponível"
                    },
                    response.latency_ms,
                    response.message
                ))
            }),
            cx,
        );
    }

    pub(crate) fn delete_settings_connection(&mut self, cx: &mut Context<Self>) {
        let Some(backend) = self.settings_backend.clone() else {
            return;
        };
        let Some(connection) = self.settings.read(cx).connection().cloned() else {
            return;
        };
        self.run_settings_operation(
            Box::pin(async move {
                backend
                    .delete_connection(DeleteProviderConnectionRequest {
                        connection_id: connection.id,
                        version: connection.version,
                    })
                    .await?;
                Ok("Conexão excluída.".to_string())
            }),
            cx,
        );
    }

    pub(crate) fn select_settings_route(&mut self, route_id: String, cx: &mut Context<Self>) {
        let route = self.settings.update(cx, |settings, _| {
            settings.selected_route_id = Some(route_id.into());
            settings.route().cloned()
        });
        if let Some(route) = route {
            self.settings.update(cx, |settings, _| {
                settings.route_strategy = route.strategy.clone().into();
                settings.route_max_attempts = route.max_attempts;
                settings.route_attempt_timeout_ms = route.attempt_timeout_ms;
                settings.route_timeout_ms = route.route_timeout_ms;
                settings.route_targets = route.targets.clone();
            });
            self.settings_route_name_input
                .update(cx, |input, cx| input.set_value(route.name, cx));
        }
        cx.notify();
    }

    pub(crate) fn new_settings_route(&mut self, cx: &mut Context<Self>) {
        self.settings.update(cx, |settings, _| {
            settings.selected_route_id = None;
            settings.route_strategy = "priority".into();
            settings.route_max_attempts = 3;
            settings.route_attempt_timeout_ms = 45_000;
            settings.route_timeout_ms = 120_000;
            settings.route_targets.clear();
        });
        self.settings_route_name_input
            .update(cx, |input, cx| input.clear(cx));
        cx.notify();
    }

    pub(crate) fn cycle_route_strategy(&mut self, cx: &mut Context<Self>) {
        self.settings.update(cx, |settings, _| {
            settings.route_strategy = match settings.route_strategy.as_ref() {
                "priority" => "lkgp".into(),
                "lkgp" => "score".into(),
                _ => "priority".into(),
            };
        });
        cx.notify();
    }

    pub(crate) fn cycle_route_limits(&mut self, cx: &mut Context<Self>) {
        self.settings.update(cx, |settings, _| {
            let (attempts, attempt_timeout, route_timeout) = match settings.route_max_attempts {
                1 => (2, 30_000, 60_000),
                2 => (3, 45_000, 120_000),
                _ => (1, 15_000, 30_000),
            };
            settings.route_max_attempts = attempts;
            settings.route_attempt_timeout_ms = attempt_timeout;
            settings.route_timeout_ms = route_timeout;
        });
        cx.notify();
    }

    pub(crate) fn move_selected_route_target(&mut self, direction: isize, cx: &mut Context<Self>) {
        let selected_connection = self
            .settings
            .read(cx)
            .selected_connection_id
            .as_ref()
            .map(ToString::to_string);
        let Some(selected_connection) = selected_connection else {
            return;
        };
        self.settings.update(cx, |settings, _| {
            let Some(index) = settings
                .route_targets
                .iter()
                .position(|target| target.connection_id == selected_connection)
            else {
                return;
            };
            let next = index
                .saturating_add_signed(direction)
                .min(settings.route_targets.len() - 1);
            if next != index {
                settings.route_targets.swap(index, next);
                for (position, target) in settings.route_targets.iter_mut().enumerate() {
                    target.position = position as u32;
                }
            }
        });
        cx.notify();
    }

    pub(crate) fn save_settings_route(&mut self, cx: &mut Context<Self>) {
        let Some(backend) = self.settings_backend.clone() else {
            return;
        };
        let (
            existing,
            strategy,
            connection,
            mut targets,
            max_attempts,
            attempt_timeout_ms,
            route_timeout_ms,
        ) = {
            let settings = self.settings.read(cx);
            (
                settings.route().cloned(),
                settings.route_strategy.to_string(),
                settings.connection().cloned(),
                settings.route_targets.clone(),
                settings.route_max_attempts,
                settings.route_attempt_timeout_ms,
                settings.route_timeout_ms,
            )
        };
        let Some(connection) = connection else {
            return;
        };
        let name = self
            .settings_route_name_input
            .read(cx)
            .value()
            .trim()
            .to_string();
        let requested_model = self
            .settings_connection_model_input
            .read(cx)
            .value()
            .trim()
            .to_string();
        if let Some(target) = targets
            .iter_mut()
            .find(|target| target.connection_id == connection.id)
        {
            target.model_id = if requested_model.is_empty() {
                connection.default_model.clone()
            } else {
                requested_model.clone()
            };
            target.enabled = true;
        } else {
            targets.push(RouteTarget {
                position: targets.len() as u32,
                connection_id: connection.id,
                model_id: if requested_model.is_empty() {
                    connection.default_model
                } else {
                    requested_model
                },
                enabled: true,
            });
        }
        targets.sort_by_key(|target| target.position);
        for (position, target) in targets.iter_mut().enumerate() {
            target.position = position as u32;
        }
        let request = UpsertProviderRouteRequest {
            id: existing.as_ref().map(|route| route.id.clone()),
            name: if name.is_empty() {
                "Rota LLM".into()
            } else {
                name
            },
            strategy,
            max_attempts: Some(max_attempts),
            attempt_timeout_ms: Some(attempt_timeout_ms),
            route_timeout_ms: Some(route_timeout_ms),
            enabled: Some(existing.as_ref().is_none_or(|route| route.enabled)),
            targets,
            version: existing.as_ref().map(|route| route.version),
        };
        self.run_settings_operation(
            Box::pin(async move {
                backend.upsert_route(request).await?;
                Ok("Rota salva com a ordem de alvos atual.".to_string())
            }),
            cx,
        );
    }

    pub(crate) fn delete_settings_route(&mut self, cx: &mut Context<Self>) {
        let Some(backend) = self.settings_backend.clone() else {
            return;
        };
        let Some(route) = self.settings.read(cx).route().cloned() else {
            return;
        };
        self.run_settings_operation(
            Box::pin(async move {
                backend
                    .delete_route(DeleteProviderRouteRequest {
                        route_id: route.id,
                        version: route.version,
                    })
                    .await?;
                Ok("Rota excluída.".to_string())
            }),
            cx,
        );
    }

    pub(crate) fn cycle_binding_role(&mut self, cx: &mut Context<Self>) {
        const ROLES: [&str; 7] = [
            "intake_router",
            "task_specifier",
            "contract_agent",
            "backend_agent",
            "test_agent",
            "integrator",
            "learning_agent",
        ];
        self.settings.update(cx, |settings, _| {
            let current = ROLES
                .iter()
                .position(|role| *role == settings.binding_agent_role.as_ref())
                .unwrap_or(0);
            settings.binding_agent_role = ROLES[(current + 1) % ROLES.len()].into();
        });
        cx.notify();
    }

    pub(crate) fn cycle_binding_stage(&mut self, cx: &mut Context<Self>) {
        const STAGES: [&str; 7] = [
            "Intake",
            "Specification",
            "Contract",
            "Code",
            "Test",
            "Integrator",
            "Learning",
        ];
        self.settings.update(cx, |settings, _| {
            let current = STAGES
                .iter()
                .position(|stage| *stage == settings.binding_stage.as_ref())
                .unwrap_or(0);
            settings.binding_stage = STAGES[(current + 1) % STAGES.len()].into();
        });
        cx.notify();
    }

    pub(crate) fn save_settings_binding(&mut self, cx: &mut Context<Self>) {
        let Some(backend) = self.settings_backend.clone() else {
            return;
        };
        let Some(project_id) = self
            .workspace
            .read(cx)
            .project_id
            .as_ref()
            .map(ToString::to_string)
        else {
            self.notices.push(
                HeaderNoticeKind::Warning,
                "Abra um projeto antes de criar o binding.",
            );
            cx.notify();
            return;
        };
        let (route_id, agent_role, stage, version) = {
            let settings = self.settings.read(cx);
            let Some(route_id) = settings.selected_route_id.as_ref().map(ToString::to_string)
            else {
                return;
            };
            let agent_role = settings.binding_agent_role.to_string();
            let stage = settings.binding_stage.to_string();
            let version = settings.snapshot.as_ref().and_then(|snapshot| {
                snapshot
                    .bindings
                    .iter()
                    .find(|binding| {
                        binding.project_id == project_id
                            && binding.agent_role == agent_role
                            && binding.stage == stage
                    })
                    .map(|binding| binding.version)
            });
            (route_id, agent_role, stage, version)
        };
        self.run_settings_operation(
            Box::pin(async move {
                backend
                    .upsert_binding(UpsertProviderRouteBindingRequest {
                        project_id,
                        agent_role,
                        stage,
                        route_id,
                        enabled: Some(true),
                        version,
                    })
                    .await?;
                Ok("Binding exato salvo.".to_string())
            }),
            cx,
        );
    }

    pub(crate) fn reset_settings_resilience(&mut self, cx: &mut Context<Self>) {
        let Some(backend) = self.settings_backend.clone() else {
            return;
        };
        let connection_id = self
            .settings
            .read(cx)
            .selected_connection_id
            .as_ref()
            .map(ToString::to_string);
        self.run_settings_operation(
            Box::pin(async move {
                let count = backend
                    .reset_resilience(ResetLlmResilienceStateRequest {
                        connection_id,
                        provider_id: None,
                    })
                    .await?;
                Ok(format!("{count} estado(s) de resiliência removido(s)."))
            }),
            cx,
        );
    }

    pub(crate) fn refresh_logs(&mut self, cx: &mut Context<Self>) {
        let Some(backend) = self.logs_backend.clone() else {
            return;
        };
        let generation = self.logs.update(cx, |logs, _| logs.begin_poll());
        let Some(generation) = generation else {
            return;
        };
        let pulsation_id = self
            .pulsations
            .read(cx)
            .active_pulsation_id
            .as_ref()
            .map(ToString::to_string);
        self.logs_task = Some(cx.spawn(async move |this, cx| {
            let result = backend.observability(pulsation_id).await;
            let _ = this.update(cx, |model, cx| {
                match result {
                    Ok(snapshot) => {
                        model.logs.update(cx, |logs, _| {
                            logs.apply_poll(generation, snapshot);
                        });
                    }
                    Err(error) => {
                        model.logs.update(cx, |logs, _| {
                            logs.fail_poll(generation, error);
                        });
                    }
                }
                cx.notify();
            });
        }));
    }

    pub(crate) fn select_log_run(&mut self, run_id: String, cx: &mut Context<Self>) {
        self.logs.update(cx, |logs, _| {
            logs.selected_run_id = Some(run_id.into());
        });
        cx.notify();
    }

    fn resync_llm_execution(&mut self, execution_id: String, cx: &mut Context<Self>) {
        let Some(backend) = self.logs_backend.clone() else {
            return;
        };
        let generation = self.logs.update(cx, |logs, _| logs.begin_resync());
        let task_key = execution_id.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = backend.execution_snapshot(execution_id.clone()).await;
            let _ = this.update(cx, |model, cx| {
                match result {
                    Ok(snapshot) => {
                        let applied = model.logs.update(cx, |logs, _| {
                            logs.apply_execution_snapshot(generation, snapshot)
                        });
                        if applied {
                            model.needs_resync = false;
                            model.status = "Execução LLM ressincronizada".into();
                        }
                    }
                    Err(error) => {
                        model.logs.update(cx, |logs, _| logs.error = Some(error));
                    }
                }
                cx.notify();
            });
        });
        self.logs_resync_tasks.insert(task_key, task);
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
                                model.refresh_settings(cx);
                                model.refresh_logs(cx);
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

        let Some(pulsations_backend) = self.pulsations_backend.clone() else {
            return;
        };
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

                let snapshot = pulsations_backend
                    .runtime_snapshot(project_id.clone())
                    .await;
                if this
                    .update(cx, |model, cx| {
                        model.lifecycle.finish_poll();
                        match snapshot {
                            Ok(snapshot) => {
                                let active_id =
                                    model.apply_pulsation_snapshot(project_id, snapshot, None, cx);
                                if let Some(active_id) = active_id {
                                    model.refresh_pulsation_artifacts(active_id, cx);
                                }
                                model.refresh_logs(cx);
                            }
                            Err(error) => model.notices.push(
                                HeaderNoticeKind::Warning,
                                format!("Falha ao atualizar runtime: {error}"),
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
        self.brownfield_task.take();
        self.brownfield_generation = self.brownfield_generation.saturating_add(1);
        self.hydrating = true;
        self.suppress_editor_sync = true;
        let session = restored.session.as_ref();
        let preferred_pulsation_id = session.and_then(|value| value.active_pulsation_id.clone());
        let restored_vault_selection = session.and_then(|value| value.selected_vault_path.clone());
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
            workspace.brownfield = crate::workspace::BrownfieldOnboardingModel::from_status(
                restored.brownfield_status.clone(),
            );

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
            pulsations.reset(Some(restored.project_id.clone()));
            pulsations.active_pulsation_id = preferred_pulsation_id.clone().map(Into::into);
        });
        self.vault.update(cx, |vault, _| {
            vault.hydrate_workspace(
                restored.root_path.clone(),
                restored.tree.clone(),
                restored_vault_selection.clone(),
            );
        });
        let restored_chat_conversation =
            session.and_then(|value| value.active_chat_conversation_id.clone());
        self.hydrate_chat_project(restored.project_id.clone(), restored_chat_conversation, cx);
        self.active_view = active_view;
        self.needs_resync = false;
        self.session_dirty = false;
        self.status = format!("Sessão restaurada: {}", restored.root_path).into();
        if let Some(notice) = restored.notice {
            self.notices.push(HeaderNoticeKind::Notice, notice);
        }
        self.hydrating = false;
        self.refresh_pulsations_with_preferred(preferred_pulsation_id, cx);
        self.refresh_vault(true, cx);
        match active_view {
            AppView::Settings => self.refresh_settings(cx),
            AppView::Logs => self.refresh_logs(cx),
            _ => {}
        }
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
        self.brownfield_task.take();
        self.file_tasks.clear();
        self.autosave_tasks.clear();
        let hitl = self.hitl_task.take();
        self.pulsations_task.take();
        let reconciliation = self.reconciliation_task.take();
        let preparation_action = self.preparation_action_task.take();
        self.artifact_preview_task.take();
        self.pulsation_artifacts_task.take();
        self.vault_refresh_task.take();
        self.vault_search_task.take();
        self.vault_rename_task.take();
        self.vault_context_task.take();
        self.vault_document_tasks.clear();
        let settings = self.settings_task.take();
        self.logs_task.take();
        self.logs_resync_tasks.clear();
        self.chat_task.take();
        self.chat_resync_tasks.clear();
        self.session_task.take();
        self.performance_probe.take();
        self.session_generation = self.session_generation.saturating_add(1);
        self.workspace_save_generations.clear();

        let request = self
            .session_dirty
            .then(|| self.session_request(cx))
            .flatten();
        let persistence = self.session_persistence.clone();
        cx.spawn(async move |this, cx| {
            // UI-originated confirmations/publications retain ownership through close as well.
            if let Some(task) = hitl {
                task.await;
            }
            if let Some(task) = reconciliation {
                task.await;
            }
            if let Some(task) = preparation_action {
                task.await;
            }
            if let Some(task) = settings {
                task.await;
            }
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
        match event {
            AppEvent::WorkspaceChanged(event) => {
                self.logs.update(cx, |logs, _| {
                    logs.record_workspace(event.clone());
                });
                self.handle_workspace_change(event, cx);
            }
            AppEvent::Llm(envelope) => {
                let execution_id = envelope.execution_id.clone();
                let classification = self.logs.update(cx, |logs, _| logs.record_llm(envelope));
                if classification == SequenceClassification::Gap {
                    self.needs_resync = true;
                    self.status = "Lacuna LLM detectada; recarregando execução persistida".into();
                    self.resync_llm_execution(execution_id, cx);
                }
            }
            AppEvent::Chat(envelope) => self.handle_chat_event(envelope, cx),
        }
        cx.notify();
    }

    fn handle_workspace_change(&mut self, event: WorkspaceChangedEvent, cx: &mut Context<Self>) {
        let should_reload = self.workspace.update(cx, |workspace, _| {
            workspace.mark_external_change(&event.relative_path, &event.kind)
        });
        self.schedule_vault_refresh(cx);
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

fn runtime_config_update() -> UpdateAgentRuntimeConfigRequest {
    UpdateAgentRuntimeConfigRequest {
        provider: None,
        model: None,
        temperature: None,
        max_output_tokens: None,
        timeout_ms: None,
        max_retries: None,
        context_chars_limit: None,
        max_context_files: None,
        context_strategy: None,
        embedding_enabled: None,
        embedding_provider: None,
        embedding_model: None,
        embedding_top_k: None,
        routing_v2_mode: None,
    }
}

trait RuntimeConfigUpdateExt {
    fn with_routing_mode(self, mode: RoutingMode) -> Self;
    fn with_context_strategy(self, strategy: ContextStrategy) -> Self;
    fn with_embedding_enabled(self, enabled: bool) -> Self;
    fn with_embedding_model(self, model: String) -> Self;
    fn with_embedding_top_k(self, top_k: usize) -> Self;
}

impl RuntimeConfigUpdateExt for UpdateAgentRuntimeConfigRequest {
    fn with_routing_mode(mut self, mode: RoutingMode) -> Self {
        self.routing_v2_mode = Some(mode);
        self
    }

    fn with_context_strategy(mut self, strategy: ContextStrategy) -> Self {
        self.context_strategy = Some(strategy);
        self
    }

    fn with_embedding_enabled(mut self, enabled: bool) -> Self {
        self.embedding_enabled = Some(enabled);
        self
    }

    fn with_embedding_model(mut self, model: String) -> Self {
        self.embedding_model = Some(model);
        self
    }

    fn with_embedding_top_k(mut self, top_k: usize) -> Self {
        self.embedding_top_k = Some(top_k);
        self
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
    let brownfield_status = match opened.kind {
        WorkspaceKind::Greenfield => None,
        WorkspaceKind::Brownfield => Some(
            core.get_brownfield_onboarding(&project.id)
                .map_err(|error| error.message)?,
        ),
    };
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

    let notice = opened.greenfield.map(|status| status.message).or_else(|| {
        (opened.kind == WorkspaceKind::Brownfield).then(|| {
            "Workspace existente detectado; escolha se deseja iniciar o mapeamento gradual."
                .to_string()
        })
    });
    Ok(RestoredSession {
        project_id: project.id,
        root_path: opened.root_path,
        tree: opened.tree,
        session,
        tabs,
        notice,
        brownfield_status,
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
    use hvc_core::persistence_service::{Artifact, Pulsation, PulsationStage, PulsationStatus};
    use hvc_core::workspace_service::{
        GetWorkspaceVaultIndexResponse, RenameWorkspaceNoteRequest, RenameWorkspaceNoteResponse,
        SearchMatch, SearchWorkspaceTextRequest, SearchWorkspaceTextResponse, VaultNoteSummary,
    };

    use super::*;
    use crate::pulsations::PulsationsFuture;
    use crate::vault::VaultFuture;
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
        consent_requests: Arc<Mutex<Vec<(String, bool)>>>,
        ignored_requests: Arc<Mutex<Vec<(String, String, bool)>>>,
        read_snapshot: WorkspaceFileSnapshot,
        write_result: Arc<Mutex<Result<String, WorkspaceOperationError>>>,
    }

    impl RecordingWorkspaceBackend {
        fn successful() -> Self {
            Self {
                reads: Arc::new(Mutex::new(Vec::new())),
                writes: Arc::new(Mutex::new(Vec::new())),
                consent_requests: Arc::new(Mutex::new(Vec::new())),
                ignored_requests: Arc::new(Mutex::new(Vec::new())),
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
                    code: "conflict".into(),
                    message: "A revisão do arquivo mudou no disco.".into(),
                    details: None,
                    remediation: Some("Recarregue o arquivo.".into()),
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

        fn set_brownfield_consent(
            &self,
            project_id: String,
            consent: bool,
        ) -> WorkspaceFuture<hvc_core::orchestration::brownfield::BrownfieldOnboardingResult>
        {
            self.consent_requests
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((project_id.clone(), consent));
            Box::pin(async move {
                Ok(
                    hvc_core::orchestration::brownfield::BrownfieldOnboardingResult {
                        status: BrownfieldOnboardingStatus {
                            project_id,
                            consent: if consent {
                                hvc_core::orchestration::brownfield::BrownfieldConsentState::Granted
                            } else {
                                hvc_core::orchestration::brownfield::BrownfieldConsentState::Declined
                            },
                            ignored_paths: Vec::new(),
                            coverage: None,
                        },
                        created_paths: Vec::new(),
                        inventory: None,
                    },
                )
            })
        }

        fn set_brownfield_ignored(
            &self,
            project_id: String,
            relative_path: String,
            ignored: bool,
        ) -> WorkspaceFuture<BrownfieldOnboardingStatus> {
            self.ignored_requests
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((project_id.clone(), relative_path.clone(), ignored));
            Box::pin(async move {
                Ok(BrownfieldOnboardingStatus {
                    project_id,
                    consent: hvc_core::orchestration::brownfield::BrownfieldConsentState::Granted,
                    ignored_paths: if ignored {
                        vec![relative_path]
                    } else {
                        Vec::new()
                    },
                    coverage: None,
                })
            })
        }
    }

    struct RecordingPulsationsBackend {
        actions: Arc<Mutex<Vec<crate::pulsations::PulsationAction>>>,
        snapshot_calls: Arc<Mutex<usize>>,
        artifact_calls: Arc<Mutex<Vec<String>>>,
        create_calls: Arc<Mutex<Vec<(String, String)>>>,
        cancel_calls: Arc<Mutex<Vec<String>>>,
        snapshot: Arc<Mutex<PulsationRuntimeSnapshot>>,
        artifacts: Arc<Mutex<Vec<Artifact>>>,
    }

    impl RecordingPulsationsBackend {
        fn new() -> Self {
            Self {
                actions: Arc::new(Mutex::new(Vec::new())),
                snapshot_calls: Arc::new(Mutex::new(0)),
                artifact_calls: Arc::new(Mutex::new(Vec::new())),
                create_calls: Arc::new(Mutex::new(Vec::new())),
                cancel_calls: Arc::new(Mutex::new(Vec::new())),
                snapshot: Arc::new(Mutex::new(PulsationRuntimeSnapshot {
                    details: Vec::new(),
                    coordination: None,
                    main_agents_count: 0,
                    hitl_checkpoints: Vec::new(),
                    pulsations: vec![test_pulsation()],
                    hitl_waiting_count: 0,
                    background_agents_count: 1,
                    brownfield_coverage: None,
                })),
                artifacts: Arc::new(Mutex::new(vec![test_artifact("Plan")])),
            }
        }
    }

    impl PulsationsBackend for RecordingPulsationsBackend {
        fn action(&self, action: crate::pulsations::PulsationAction) -> PulsationsFuture<()> {
            self.actions.lock().unwrap().push(action);
            Box::pin(async { Ok(()) })
        }
        fn artifact_content(&self, _pulse: String, _artifact: String) -> PulsationsFuture<String> {
            Box::pin(async { Ok("Conteúdo persistido da fixture".into()) })
        }
        fn reconcile(
            &self,
            _request: hvc_core::orchestration::ConfirmBaseReconciliationRequest,
        ) -> PulsationsFuture<()> {
            Box::pin(async { Ok(()) })
        }
        fn runtime_snapshot(
            &self,
            _project_id: String,
        ) -> PulsationsFuture<PulsationRuntimeSnapshot> {
            *self
                .snapshot_calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) += 1;
            let snapshot = self
                .snapshot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            Box::pin(async move { Ok(snapshot) })
        }

        fn artifacts(&self, pulsation_id: String) -> PulsationsFuture<Vec<Artifact>> {
            self.artifact_calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(pulsation_id);
            let artifacts = self
                .artifacts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            Box::pin(async move { Ok(artifacts) })
        }

        fn create(&self, project_id: String, feature_name: String) -> PulsationsFuture<Pulsation> {
            self.create_calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((project_id, feature_name));
            Box::pin(async { Ok(test_pulsation()) })
        }

        fn cancel_jobs(&self, pulsation_id: String) -> PulsationsFuture<usize> {
            self.cancel_calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(pulsation_id);
            Box::pin(async { Ok(1) })
        }
    }

    struct RecordingVaultBackend {
        tree_calls: Arc<Mutex<usize>>,
        index_calls: Arc<Mutex<Vec<bool>>>,
        search_calls: Arc<Mutex<Vec<(String, bool, bool)>>>,
        read_calls: Arc<Mutex<Vec<String>>>,
    }

    impl RecordingVaultBackend {
        fn new() -> Self {
            Self {
                tree_calls: Arc::new(Mutex::new(0)),
                index_calls: Arc::new(Mutex::new(Vec::new())),
                search_calls: Arc::new(Mutex::new(Vec::new())),
                read_calls: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl VaultBackend for RecordingVaultBackend {
        fn tree(&self) -> VaultFuture<Vec<FileNode>> {
            *self
                .tree_calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) += 1;
            Box::pin(async { Ok(test_tree()) })
        }

        fn index(&self, force_rebuild: bool) -> VaultFuture<GetWorkspaceVaultIndexResponse> {
            self.index_calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(force_rebuild);
            Box::pin(async { Ok(test_vault_index()) })
        }

        fn search(
            &self,
            request: SearchWorkspaceTextRequest,
        ) -> VaultFuture<SearchWorkspaceTextResponse> {
            self.search_calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((
                    request.query,
                    matches!(request.mode, SearchMode::Regex),
                    request.case_sensitive,
                ));
            Box::pin(async {
                Ok(SearchWorkspaceTextResponse {
                    matches: vec![SearchMatch {
                        relative_path: "Features/Login.md".into(),
                        line: 3,
                        column: 4,
                        line_text: "token".into(),
                    }],
                    truncated: false,
                    scanned_files: 1,
                })
            })
        }

        fn read(&self, relative_path: String) -> VaultFuture<String> {
            self.read_calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(relative_path);
            Box::pin(async { Ok("# Login".into()) })
        }

        fn rename(
            &self,
            request: RenameWorkspaceNoteRequest,
        ) -> VaultFuture<RenameWorkspaceNoteResponse> {
            Box::pin(async move {
                Ok(RenameWorkspaceNoteResponse {
                    renamed_to: request.to_relative_path,
                    updated_references: 1,
                    affected_files: 1,
                })
            })
        }
    }

    fn test_pulsation() -> Pulsation {
        Pulsation {
            terminal_reason: None,
            id: "pulse-1".into(),
            project_id: "project-1".into(),
            feature_name: "Login".into(),
            stage: PulsationStage::Code,
            status: PulsationStatus::Running,
            created_at: "2026-08-22T00:00:00Z".into(),
            updated_at: "2026-08-22T00:01:00Z".into(),
        }
    }

    fn test_artifact(name: &str) -> Artifact {
        Artifact {
            id: format!("artifact-{name}"),
            pulsation_id: "pulse-1".into(),
            name: name.into(),
            kind: "markdown".into(),
            relative_path: Some("Features/Login.md".into()),
            content_sha256: None,
            created_at: "now".into(),
            updated_at: "now".into(),
        }
    }

    fn test_tree() -> Vec<FileNode> {
        vec![FileNode {
            name: "Login.md".into(),
            relative_path: "Features/Login.md".into(),
            kind: hvc_core::workspace_service::FileNodeKind::File,
            children: Vec::new(),
        }]
    }

    fn test_vault_index() -> GetWorkspaceVaultIndexResponse {
        GetWorkspaceVaultIndexResponse {
            notes: vec![VaultNoteSummary {
                relative_path: "Features/Login.md".into(),
                title: "Login".into(),
                revision: "r1".into(),
                frontmatter: None,
                normalized_properties: None,
                links: Vec::new(),
                outgoing_paths: Vec::new(),
                backlink_paths: Vec::new(),
                schema_warnings: Vec::new(),
            }],
            edges: Vec::new(),
            broken_links: Vec::new(),
            indexed_at: "now".into(),
        }
    }

    fn configure_domain_workspace(model: &mut AppModel, cx: &mut Context<AppModel>) {
        model.workspace.update(cx, |workspace, _| {
            workspace.project_id = Some("project-1".into());
            workspace.root_path = Some("C:\\vault".into());
            workspace.tree = test_tree();
        });
        model.vault.update(cx, |vault, _| {
            vault.hydrate_workspace(
                "C:\\vault".into(),
                test_tree(),
                Some("Features/Login.md".into()),
            );
        });
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
            assert!(model.vault.read(cx).index.is_none());
            assert!(!model.chat.read(cx).streaming);
            assert!(model.settings.read(cx).snapshot.is_none());
            assert!(model.logs.read(cx).events.is_empty());
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
                    brownfield_status: None,
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
                    brownfield_status: None,
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
    fn brownfield_consent_action_is_project_scoped_and_updates_visible_state(
        cx: &mut TestAppContext,
    ) {
        let backend = Arc::new(RecordingWorkspaceBackend::successful());
        let requests = Arc::clone(&backend.consent_requests);
        let model = cx.new(|cx| AppModel::new_for_test_with_workspace_backend(backend, cx));
        model.update(cx, |model, cx| {
            model.apply_restored_session(
                RestoredSession {
                    project_id: "project-brownfield".into(),
                    root_path: "C:\\existing".into(),
                    tree: Vec::new(),
                    session: None,
                    tabs: Vec::new(),
                    notice: None,
                    brownfield_status: Some(BrownfieldOnboardingStatus {
                        project_id: "project-brownfield".into(),
                        consent:
                            hvc_core::orchestration::brownfield::BrownfieldConsentState::NotDecided,
                        ignored_paths: Vec::new(),
                        coverage: None,
                    }),
                },
                cx,
            );
            model.set_brownfield_mapping_enabled(true, cx);
        });
        cx.run_until_parked();

        assert_eq!(
            requests
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_slice(),
            &[("project-brownfield".to_string(), true)]
        );
        cx.read(|cx| {
            let workspace = model.read(cx).workspace.read(cx);
            assert_eq!(
                workspace.brownfield.state,
                BrownfieldOnboardingState::Enabled
            );
            assert!(!workspace.brownfield.busy);
            assert!(workspace.brownfield.error.is_none());
        });
    }

    #[gpui::test]
    fn brownfield_ignore_action_is_scoped_to_the_selected_source(cx: &mut TestAppContext) {
        let backend = Arc::new(RecordingWorkspaceBackend::successful());
        let requests = Arc::clone(&backend.ignored_requests);
        let model = cx.new(|cx| AppModel::new_for_test_with_workspace_backend(backend, cx));
        model.update(cx, |model, cx| {
            model.workspace.update(cx, |workspace, _| {
                workspace.project_id = Some("project-brownfield".into());
                workspace.active_relative_path = Some("src/lib.rs".into());
                workspace.brownfield.state = BrownfieldOnboardingState::Enabled;
            });
            model.set_active_brownfield_ignored(true, cx);
        });
        cx.run_until_parked();

        assert_eq!(
            requests
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_slice(),
            &[(
                "project-brownfield".to_string(),
                "src/lib.rs".to_string(),
                true,
            )]
        );
        cx.read(|cx| {
            let workspace = model.read(cx).workspace.read(cx);
            assert_eq!(workspace.brownfield.ignored_paths, ["src/lib.rs"]);
            assert!(!workspace.brownfield.busy);
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

    #[gpui::test]
    fn refreshes_pulsation_artifacts_through_the_typed_backend(cx: &mut TestAppContext) {
        let pulsations_backend = Arc::new(RecordingPulsationsBackend::new());
        let vault_backend = Arc::new(RecordingVaultBackend::new());
        let snapshot_calls = Arc::clone(&pulsations_backend.snapshot_calls);
        let artifact_calls = Arc::clone(&pulsations_backend.artifact_calls);
        let artifacts = Arc::clone(&pulsations_backend.artifacts);
        let model = cx.new(|cx| {
            AppModel::new_for_test_with_domain_backends(
                pulsations_backend.clone(),
                vault_backend,
                cx,
            )
        });

        model.update(cx, |model, cx| {
            configure_domain_workspace(model, cx);
            model.refresh_pulsations(cx);
        });
        cx.run_until_parked();

        assert_eq!(
            *snapshot_calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            1
        );
        assert_eq!(
            *artifact_calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec!["pulse-1"]
        );
        cx.read(|cx| {
            assert_eq!(model.read(cx).pulsations.read(cx).artifacts[0].name, "Plan");
        });

        *artifacts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            vec![test_artifact("Generated code")];
        model.update(cx, AppModel::refresh_pulsations);
        cx.run_until_parked();

        assert_eq!(
            *artifact_calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec!["pulse-1", "pulse-1"]
        );
        cx.read(|cx| {
            assert_eq!(
                model.read(cx).pulsations.read(cx).artifacts[0].name,
                "Generated code"
            );
        });
    }

    fn preparation_detail(
        with_questions: bool,
    ) -> hvc_core::orchestration::PulsationCoordinationSnapshot {
        use hvc_core::orchestration::*;
        PulsationCoordinationSnapshot {
            description_editable: !with_questions,
            context_disposition: None,
            diagnostic: None,
            pulsation_id: "pulse-1".into(),
            project_id: "project-1".into(),
            queue_sequence: 1,
            pipeline_version: 2,
            input_revision: 3,
            state_version: 9,
            stage: if with_questions {
                PulsationStage::Specification
            } else {
                PulsationStage::Intake
            },
            status: if with_questions {
                PulsationStatus::Checkpoint
            } else {
                PulsationStatus::Planning
            },
            blocking_reason: None,
            preparation: PreparationSnapshot {
                assessment: None,
                description_review: DescriptionReviewState::Pending,
                prespec_state: PreSpecState::NotScheduled,
                context_artifact_id: None,
                skip_reason: None,
            },
            questions: with_questions.then(|| SpecificationQuestionSet {
                id: "questions-1".into(),
                checkpoint_id: "clarification-1".into(),
                fence: ExecutionFence {
                    project_id: "project-1".into(),
                    pulsation_id: Some("pulse-1".into()),
                    input_revision: Some(3),
                    job_id: "job-1".into(),
                    generation: 2,
                    state_version: 9,
                },
                conversation_id: "conversation-1".into(),
                questions: vec![SpecificationQuestion {
                    id: "token".into(),
                    question: "Qual token?".into(),
                    rationale: "Definir contrato".into(),
                }],
            }),
            learning: LearningState::Unavailable,
            learning_run: None,
            artifact_ids: vec![],
        }
    }

    #[gpui::test]
    fn clarification_ui_preserves_drafts_and_submits_the_displayed_fence_once(
        cx: &mut TestAppContext,
    ) {
        let backend = Arc::new(RecordingPulsationsBackend::new());
        backend.snapshot.lock().unwrap().details = vec![preparation_detail(true)];
        let model = cx.new(|cx| {
            AppModel::new_for_test_with_domain_backends(
                backend.clone(),
                Arc::new(RecordingVaultBackend::new()),
                cx,
            )
        });
        model.update(cx, |m, cx| {
            configure_domain_workspace(m, cx);
            m.refresh_pulsations(cx);
        });
        cx.run_until_parked();
        model.update(cx, |m, cx| {
            assert!(
                m.pulsations
                    .update(cx, |p, _| p.hitl.begin(HitlDecision::Approve))
                    .is_none()
            );
            m.specification_answer_inputs[0]
                .1
                .update(cx, |i, cx| i.set_value("JWT assinado", cx));
            m.refresh_pulsations(cx);
        });
        cx.run_until_parked();
        model.update(cx, |m, cx| {
            assert_eq!(
                m.specification_answer_inputs[0].1.read(cx).value(),
                "JWT assinado"
            );
            m.answer_specification(cx);
            m.answer_specification(cx);
        });
        cx.run_until_parked();
        let actions = backend.actions.lock().unwrap();
        assert_eq!(actions.len(), 1);
        let crate::pulsations::PulsationAction::Answer(request) = &actions[0] else {
            panic!("answer action")
        };
        assert_eq!(request.question_set_id, "questions-1");
        assert_eq!(request.expected_input_revision, 3);
        assert_eq!(request.expected_state_version, 9);
        assert_eq!(request.answers[0].answer, "JWT assinado");
        drop(actions);
        backend.snapshot.lock().unwrap().details[0]
            .questions
            .as_mut()
            .unwrap()
            .id = "questions-2".into();
        model.update(cx, AppModel::refresh_pulsations);
        cx.run_until_parked();
        cx.read(|cx| {
            assert!(
                model.read(cx).specification_answer_inputs[0]
                    .1
                    .read(cx)
                    .value()
                    .is_empty()
            )
        });
    }

    #[gpui::test]
    fn learning_ui_submits_the_displayed_generation_once(cx: &mut TestAppContext) {
        use hvc_core::orchestration::{LearningRunSnapshot, LearningState};
        let backend = Arc::new(RecordingPulsationsBackend::new());
        let mut detail = preparation_detail(false);
        detail.learning = LearningState::DeferredBudget;
        detail.learning_run = Some(LearningRunSnapshot {
            job_id: "learning-1".into(),
            generation: 7,
            budget_calls: 3,
            total_calls: 3,
            budget_grants: 1,
            artifact_id: None,
            diagnostic: Some("budget".into()),
        });
        backend.snapshot.lock().unwrap().details = vec![detail];
        let model = cx.new(|cx| {
            AppModel::new_for_test_with_domain_backends(
                backend.clone(),
                Arc::new(RecordingVaultBackend::new()),
                cx,
            )
        });
        model.update(cx, |m, cx| {
            configure_domain_workspace(m, cx);
            m.refresh_pulsations(cx);
        });
        cx.run_until_parked();
        model.update(cx, |m, cx| {
            m.learning_action(true, cx);
            m.learning_action(true, cx);
        });
        cx.run_until_parked();
        {
            let actions = backend.actions.lock().unwrap();
            assert_eq!(actions.len(), 1);
            let crate::pulsations::PulsationAction::RetryLearning(request) = &actions[0] else {
                panic!("retry learning");
            };
            assert_eq!(request.job_id, "learning-1");
            assert_eq!(request.expected_generation, 7);
        }
        model.update(cx, |m, cx| m.learning_action(false, cx));
        cx.run_until_parked();
        assert!(matches!(
            backend.actions.lock().unwrap()[1],
            crate::pulsations::PulsationAction::DismissLearning(_)
        ));
    }

    #[gpui::test]
    fn description_ui_uses_its_own_revision_api_and_pathless_artifact_content(
        cx: &mut TestAppContext,
    ) {
        let backend = Arc::new(RecordingPulsationsBackend::new());
        backend.snapshot.lock().unwrap().details = vec![preparation_detail(false)];
        let model = cx.new(|cx| {
            AppModel::new_for_test_with_domain_backends(
                backend.clone(),
                Arc::new(RecordingVaultBackend::new()),
                cx,
            )
        });
        model.update(cx, |m, cx| {
            configure_domain_workspace(m, cx);
            m.refresh_pulsations(cx);
        });
        cx.run_until_parked();
        model.update(cx, |m, cx| {
            m.description_review_input
                .update(cx, |i, cx| i.set_value("  Crie ação Unicode  ", cx));
            m.revise_description(cx);
        });
        cx.run_until_parked();
        let actions = backend.actions.lock().unwrap();
        let crate::pulsations::PulsationAction::Revise(request) = &actions[0] else {
            panic!("description action")
        };
        assert_eq!(request.expected_input_revision, 3);
        assert_eq!(request.description, "  Crie ação Unicode  ");
        drop(actions);
        model.update(cx, |m, cx| {
            m.preview_artifact("pulse-1".into(), "internal-1".into(), cx)
        });
        cx.run_until_parked();
        cx.read(|cx| {
            assert_eq!(
                model
                    .read(cx)
                    .pulsations
                    .read(cx)
                    .artifact_preview
                    .as_ref()
                    .unwrap()
                    .1,
                "Conteúdo persistido da fixture"
            )
        });
    }

    #[gpui::test]
    fn creates_and_cancels_pulsations_through_the_typed_backend(cx: &mut TestAppContext) {
        let pulsations_backend = Arc::new(RecordingPulsationsBackend::new());
        let vault_backend = Arc::new(RecordingVaultBackend::new());
        let create_calls = Arc::clone(&pulsations_backend.create_calls);
        let cancel_calls = Arc::clone(&pulsations_backend.cancel_calls);
        let model = cx.new(|cx| {
            AppModel::new_for_test_with_domain_backends(pulsations_backend, vault_backend, cx)
        });

        model.update(cx, |model, cx| {
            configure_domain_workspace(model, cx);
            model
                .pulsation_feature_input
                .update(cx, |input, cx| input.set_value("Login", cx));
            model.create_pulsation(cx);
        });
        cx.run_until_parked();

        assert_eq!(
            *create_calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec![("project-1".into(), "Login".into())]
        );
        cx.read(|cx| {
            let model = model.read(cx);
            assert!(model.pulsation_feature_input.read(cx).value().is_empty());
            assert_eq!(
                model.pulsations.read(cx).notice.as_deref(),
                Some("Pulsação “Login” iniciada.")
            );
        });

        model.update(cx, AppModel::cancel_active_pulsation_jobs);
        cx.run_until_parked();

        assert_eq!(
            *cancel_calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec!["pulse-1"]
        );
        cx.read(|cx| {
            assert_eq!(
                model.read(cx).pulsations.read(cx).notice.as_deref(),
                Some("1 job(s) marcado(s) para cancelamento.")
            );
        });
    }

    #[gpui::test]
    fn debounces_vault_refresh_and_deduplicates_document_reads(cx: &mut TestAppContext) {
        let pulsations_backend = Arc::new(RecordingPulsationsBackend::new());
        let vault_backend = Arc::new(RecordingVaultBackend::new());
        let tree_calls = Arc::clone(&vault_backend.tree_calls);
        let index_calls = Arc::clone(&vault_backend.index_calls);
        let search_calls = Arc::clone(&vault_backend.search_calls);
        let read_calls = Arc::clone(&vault_backend.read_calls);
        let model = cx.new(|cx| {
            AppModel::new_for_test_with_domain_backends(pulsations_backend, vault_backend, cx)
        });

        model.update(cx, |model, cx| {
            configure_domain_workspace(model, cx);
            model
                .vault_search_input
                .update(cx, |input, cx| input.set_value("token", cx));
            model.set_vault_search_mode(SearchMode::Regex, cx);
            model.toggle_vault_search_case(cx);
            model.search_vault(cx);
            model.ensure_vault_documents(
                vec!["Features/Login.md".into(), "Features/Login.md".into()],
                cx,
            );
            model.ensure_vault_documents(vec!["Features/Login.md".into()], cx);
            model.schedule_vault_refresh(cx);
        });
        cx.run_until_parked();

        assert_eq!(
            *search_calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec![("token".into(), true, true)]
        );
        assert_eq!(
            *read_calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec!["Features/Login.md"]
        );

        cx.executor()
            .advance_clock(VAULT_REFRESH_DEBOUNCE - std::time::Duration::from_millis(1));
        cx.run_until_parked();
        assert_eq!(
            *tree_calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            0
        );
        assert!(
            index_calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        );

        cx.executor()
            .advance_clock(std::time::Duration::from_millis(1));
        cx.run_until_parked();
        assert_eq!(
            *tree_calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            1
        );
        assert_eq!(
            *index_calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec![false]
        );
        cx.read(|cx| {
            let vault = model.read(cx).vault.read(cx);
            assert!(!vault.stale);
            assert_eq!(vault.search_response.as_ref().unwrap().matches.len(), 1);
            assert_eq!(
                vault.documents.get("Features/Login.md").map(String::as_str),
                Some("# Login")
            );
        });
    }
}
