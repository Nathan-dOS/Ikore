use std::collections::HashMap;
use std::sync::Arc;

#[path = "preparation_ui.rs"]
mod preparation_ui;

use gpui::{
    AppContext as _, ClipboardItem, Context, Entity, PathPromptOptions, SharedString, Subscription,
    Task, Window,
};
use hvc_core::agent_runtime::{AdaptiveContextMode, ContextStrategy, DynamicContextPolicy};
use hvc_core::chat_service::{ChatEventEnvelope, ChatExecutionTargetRequest, ChatTransportKind};
use hvc_core::llm::{
    DeleteProviderConnectionRequest, DeleteProviderRouteRequest, ResetLlmResilienceStateRequest,
    RouteTarget, RoutingMode, TestProviderConnectionRequest, UpsertProviderConnectionRequest,
    UpsertProviderRouteBindingRequest, UpsertProviderRouteRequest,
};
use hvc_core::orchestration::IndexingEvent;
use hvc_core::orchestration::brownfield::BrownfieldOnboardingStatus;
use hvc_core::persistence_service::{
    ClearExecutionOverrideRequest, CliFallbackDecision, EnsureProjectForWorkspaceRequest,
    HitlDecision, LoadUiSessionRequest, PulsationRuntimeSnapshot, PulsationStatus,
    ResolveCliFallbackRequest, SaveUiSessionRequest, StageFailureDecision, UiSessionState,
    UiTabSnapshot, UpdateAgentRuntimeConfigRequest,
};
use hvc_core::terminals::{
    CliAgentConfig, CliBackend, CliModelSelectionSource, CreateFreeFormSlotRequest,
    ResumeManualRunRequest, StartFreeFormRunRequest, StartManualRunRequest, TerminalRunStatus,
    TerminalSlot, ThinkingLevel, UpdateCliAgentConfigRequest, UpdateFreeFormSlotRequest,
};
use hvc_core::workspace_service::{
    FileNode, OpenWorkspaceRequest, ReadWorkspaceFileRequest, SearchMode, WorkspaceChangedEvent,
    WorkspaceKind,
};
use hvc_core::{AppCore, AppEvent};
use hvc_editor::{EditorSelection, EditorSnapshot, TextPosition};
use tokio::sync::broadcast::error::RecvError;

use crate::agents::{
    AgentsBackend, AgentsDashboardSnapshot, AgentsModel, AgentsTab, CliModelPickerState,
    CliModelPickerTarget, CoreAgentsBackend,
};
use crate::chat::{ChatBackend, ChatEventOutcome, ChatModel, CoreChatBackend};
use crate::lifecycle::LifecycleState;
use crate::logs::{CoreLogsBackend, LogsBackend, LogsModel, SequenceClassification};
use crate::pulsations::{
    CorePulsationsBackend, PulsationDetailSection, PulsationsBackend, PulsationsModel,
};
use crate::session::{
    CoreSessionPersistence, HeaderNoticeAction, HeaderNoticeKind, SESSION_DEBOUNCE,
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
    WorkspaceEditorModel, WorkspaceTabModel,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppView {
    Forge,
    Workspace,
    Pulsations,
    Vault,
    Chat,
    Agents,
    Settings,
    Logs,
}

impl AppView {
    pub const ALL: [Self; 8] = [
        Self::Forge,
        Self::Workspace,
        Self::Pulsations,
        Self::Vault,
        Self::Chat,
        Self::Agents,
        Self::Settings,
        Self::Logs,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Pulsations => "pulsations",
            Self::Forge => "forge",
            Self::Vault => "vault",
            Self::Chat => "chat",
            Self::Agents => "agents",
            Self::Settings => "settings",
            Self::Logs => "logs",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Workspace => "Workspace & HITL",
            Self::Pulsations => "Pulsations & Pipeline",
            Self::Forge => "Forge — Tasks & Features",
            Self::Vault => "Vault & Graph",
            Self::Chat => "Chat / Companion",
            Self::Agents => "Agents & Terminais",
            Self::Settings => "Settings & AI Runtime",
            Self::Logs => "Logs & Observability",
        }
    }

    pub const fn short_label(self) -> &'static str {
        match self {
            Self::Workspace => "Workspace",
            Self::Pulsations => "Pulsations",
            Self::Forge => "Forge",
            Self::Vault => "Vault",
            Self::Chat => "Companion",
            Self::Agents => "Agents",
            Self::Settings => "Settings",
            Self::Logs => "Logs",
        }
    }

    pub const fn glyph(self) -> &'static str {
        match self {
            Self::Workspace => "W",
            Self::Pulsations => "P",
            Self::Forge => "F",
            Self::Vault => "V",
            Self::Chat => "C",
            Self::Agents => "A",
            Self::Settings => "S",
            Self::Logs => "L",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::Workspace => "Files, tabs, editor, preview, and HITL review",
            Self::Pulsations => "Executions, checkpoints, and human decisions",
            Self::Forge => "Task backlog, discovery, and feature planning",
            Self::Vault => "Markdown notes, links, and workspace context",
            Self::Chat => "Conversation, streaming, and read-only tools",
            Self::Agents => "CLI/API execution of the seven canonical agents",
            Self::Settings => "Providers, LLM routing, and local preferences",
            Self::Logs => "Typed events and runtime diagnostics",
        }
    }

    /// `"tasks"` and `"feature-forge"` are the pre-consolidation ids (see the UI flow plan Part
    /// 1.1); sessions persisted before the merge still restore correctly through the unified Forge
    /// view.
    pub fn from_id(id: &str) -> Self {
        match id {
            "tasks" | "feature-forge" => Self::Forge,
            _ => Self::ALL
                .into_iter()
                .find(|view| view.id() == id)
                .unwrap_or(Self::Workspace),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AppTab {
    Agent,
    Engine,
    Companion,
}

impl AppTab {
    pub const ALL: [Self; 3] = [Self::Agent, Self::Engine, Self::Companion];

    pub const fn sub_views(self) -> &'static [AppView] {
        match self {
            Self::Agent => &[AppView::Agents, AppView::Logs],
            Self::Engine => &[AppView::Forge, AppView::Pulsations, AppView::Vault],
            Self::Companion => &[AppView::Chat, AppView::Workspace],
        }
    }

    pub const fn default_view(self) -> AppView {
        match self {
            Self::Agent => AppView::Agents,
            Self::Engine => AppView::Vault,
            Self::Companion => AppView::Chat,
        }
    }

    pub fn contains(self, view: AppView) -> bool {
        self.sub_views().contains(&view)
    }

    pub fn from_view(view: AppView) -> Option<Self> {
        Self::ALL.into_iter().find(|tab| tab.contains(view))
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Agent => "Agent",
            Self::Engine => "Engine",
            Self::Companion => "Companion",
        }
    }

    pub const fn id(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Engine => "engine",
            Self::Companion => "companion",
        }
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

/// Per-tab attention counters shown as header badges and, unfolded, in the status bar. See the UI
/// flow plan Part 4.4.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TabBadges {
    pub agent_failed_runs: usize,
    pub engine_awaiting_hitl: usize,
    pub engine_running_jobs: usize,
    pub companion_streaming: usize,
}

pub struct AppModel {
    pub(crate) core: Option<Arc<AppCore>>,
    pub(crate) forge: crate::forge::ForgeUi,
    session_persistence: Option<Arc<dyn SessionPersistence>>,
    workspace_backend: Option<Arc<dyn WorkspaceBackend>>,
    pulsations_backend: Option<Arc<dyn PulsationsBackend>>,
    vault_backend: Option<Arc<dyn VaultBackend>>,
    pub(crate) settings_backend: Option<Arc<dyn SettingsBackend>>,
    pub(crate) agents_backend: Option<Arc<dyn AgentsBackend>>,
    logs_backend: Option<Arc<dyn LogsBackend>>,
    chat_backend: Option<Arc<dyn ChatBackend>>,
    pub navigation: Entity<NavigationModel>,
    pub workspace: Entity<WorkspaceEditorModel>,
    pub pulsations: Entity<PulsationsModel>,
    pub(crate) pulsations_split: Entity<crate::split_pane::SplitPaneState>,
    pub(crate) workspace_tree_split: Entity<crate::split_pane::SplitPaneState>,
    pub(crate) workspace_preview_split: Entity<crate::split_pane::SplitPaneState>,
    pub(crate) workspace_context_split: Entity<crate::split_pane::SplitPaneState>,
    /// Feature list against the section body, shared by every Forge section.
    pub(crate) forge_feature_split: Entity<crate::split_pane::SplitPaneState>,
    /// Incubation only: conversation against the proposed specification.
    pub(crate) forge_incubation_split: Entity<crate::split_pane::SplitPaneState>,
    /// Triage only: the evaluation form against the agent's analysis. Separate
    /// from the incubation split because a form and a transcript want different
    /// proportions.
    pub(crate) forge_triage_split: Entity<crate::split_pane::SplitPaneState>,
    pub vault: Entity<VaultModel>,
    #[cfg(test)]
    pub(crate) pulsation_feature_input: Entity<NativeTextInput>,
    pub(crate) reconciliation_input: Entity<NativeTextInput>,
    pub(crate) description_review_input: Entity<NativeTextInput>,
    pub(crate) specification_answer_inputs: Vec<(String, Entity<NativeTextInput>)>,
    pub(crate) stage_feedback_input: Entity<NativeTextInput>,
    pub(crate) preparation_threshold_input: Entity<NativeTextInput>,
    pub(crate) context_limit_input: Entity<NativeTextInput>,
    preparation_scope: Option<(String, u64, Option<String>)>,
    pub(crate) learning_graph_decisions:
        HashMap<String, hvc_core::orchestration::LearningGraphDecision>,
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
    pub(crate) cli_model_search_input: Entity<NativeTextInput>,
    pub(crate) agent_prompt_inputs: HashMap<hvc_core::llm::AgentRole, Entity<NativeTextInput>>,
    pub(crate) agent_model_inputs: HashMap<hvc_core::llm::AgentRole, Entity<NativeTextInput>>,
    pub(crate) agent_custom_executable_inputs:
        HashMap<hvc_core::llm::AgentRole, Entity<NativeTextInput>>,
    pub(crate) agent_custom_args_inputs: HashMap<hvc_core::llm::AgentRole, Entity<NativeTextInput>>,
    pub(crate) free_form_prompt_inputs: HashMap<String, Entity<NativeTextInput>>,
    pub(crate) free_form_label_inputs: HashMap<String, Entity<NativeTextInput>>,
    pub(crate) free_form_model_inputs: HashMap<String, Entity<NativeTextInput>>,
    pub(crate) free_form_custom_executable_inputs: HashMap<String, Entity<NativeTextInput>>,
    pub(crate) free_form_custom_args_inputs: HashMap<String, Entity<NativeTextInput>>,
    pub chat: Entity<ChatModel>,
    pub(crate) agents: Entity<AgentsModel>,
    pub(crate) settings: Entity<SettingsModel>,
    pub(crate) logs: Entity<LogsModel>,
    active_view: AppView,
    active_tab: AppTab,
    last_view_per_tab: HashMap<AppTab, AppView>,
    status: SharedString,
    event_count: u64,
    needs_resync: bool,
    pub(crate) indexing_status: Option<hvc_core::orchestration::IndexingEvent>,
    pub(crate) model_pull_status: Option<hvc_core::embedding_service::ModelPullEvent>,
    pub(crate) notifications: crate::notifications::Notifications,
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
    hitl_documents_task: Option<Task<()>>,
    failure_task: Option<Task<()>>,
    failure_detail_task: Option<Task<()>>,
    backend_override_task: Option<Task<()>>,
    vault_refresh_task: Option<Task<()>>,
    vault_search_task: Option<Task<()>>,
    vault_rename_task: Option<Task<()>>,
    vault_context_task: Option<Task<()>>,
    vault_document_tasks: HashMap<String, Task<()>>,
    settings_task: Option<Task<()>>,
    embedding_readiness_task: Option<Task<()>>,
    model_pull_task: Option<Task<()>>,
    agents_task: Option<Task<()>>,
    agents_notice_timer: Option<Task<()>>,
    model_catalog_task: Option<Task<()>>,
    logs_task: Option<Task<()>>,
    logs_resync_tasks: HashMap<String, Task<()>>,
    chat_task: Option<Task<()>>,
    chat_resync_tasks: HashMap<String, Task<()>>,
    session_task: Option<Task<()>>,
    performance_probe: Option<crate::performance::FrameLatencyProbe>,
    pub(crate) confirm_workspace_overwrite: Option<String>,
    pub(crate) confirm_hitl_reject: bool,
    pub(crate) confirm_stage_reject: bool,
    pub(crate) confirm_failure_abort: bool,
    pub(crate) confirm_brownfield_revoke: bool,
    pub(crate) active_dropdown: Option<String>,
    pub(crate) choice_menu: Option<gpui::Entity<crate::select_menu::SelectMenu>>,
    /// Opt-in (default off): jump straight to a pulsation's Review section the moment a checkpoint
    /// appears, instead of only surfacing the "review(s) waiting" notice. See the UI flow plan
    /// Part 4.3; toggled from Settings > Runtime.
    pub(crate) auto_open_review: bool,
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
        let agents_backend: Arc<dyn AgentsBackend> =
            Arc::new(CoreAgentsBackend::new(Arc::clone(&core)));
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
        model.agents_backend = Some(agents_backend);
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
        let active_tab = AppTab::from_view(active_view).unwrap_or(AppTab::Companion);
        let last_view_per_tab = AppTab::ALL
            .into_iter()
            .map(|tab| {
                let view = if tab.contains(active_view) {
                    active_view
                } else {
                    tab.default_view()
                };
                (tab, view)
            })
            .collect::<HashMap<_, _>>();
        let navigation = cx.new(|_| NavigationModel {
            active: active_view,
            active_tab,
            sidebar_collapsed: false,
        });
        let workspace = cx.new(WorkspaceEditorModel::new);
        let editor = workspace.read(cx).editor.clone();
        #[cfg(test)]
        let pulsation_feature_input = cx.new(|cx| NativeTextInput::new("Feature name", cx));
        let reconciliation_input =
            cx.new(|cx| NativeTextInput::new("What did you verify or fix in the base?", cx));
        let description_review_input =
            cx.new(|cx| NativeTextInput::new("Describe the action and expected result", cx));
        let stage_feedback_input =
            cx.new(|cx| NativeTextInput::new("Feedback to request stage review", cx));
        let preparation_threshold_input =
            cx.new(|cx| NativeTextInput::new("Threshold from 0 to 100", cx));
        let context_limit_input =
            cx.new(|cx| NativeTextInput::new("Context budget: 2,000 to 80,000 characters", cx));
        let vault_search_input = cx.new(|cx| NativeTextInput::new("Text or regex", cx));
        let vault_rename_input = cx.new(|cx| NativeTextInput::new("New note name", cx));
        let settings_connection_name_input =
            cx.new(|cx| NativeTextInput::new("Connection name", cx));
        let settings_connection_model_input =
            cx.new(|cx| NativeTextInput::new("Default model", cx));
        let settings_connection_endpoint_input =
            cx.new(|cx| NativeTextInput::new("Custom endpoint", cx));
        let settings_connection_secret_input = cx
            .new(|cx| NativeTextInput::new("API key (will not be displayed again)", cx).obscured());
        let settings_route_name_input = cx.new(|cx| NativeTextInput::new("Route name", cx));
        let settings_embedding_model_input =
            cx.new(|cx| NativeTextInput::new("Embedding model", cx));
        let chat_draft_input =
            cx.new(|cx| NativeTextInput::new("Message to Companion", cx).multiline());
        let cli_model_search_input =
            cx.new(|cx| NativeTextInput::new("Search model ID, name, provider or capability", cx));
        let agent_prompt_inputs = hvc_core::llm::AgentRole::ALL
            .into_iter()
            .map(|role| {
                let descriptor = hvc_core::terminals::canonical_agent(role);
                (
                    role,
                    cx.new(|cx| {
                        NativeTextInput::new(format!("Manual prompt for {}", descriptor.name), cx)
                    }),
                )
            })
            .collect::<HashMap<_, _>>();
        let agent_model_inputs = hvc_core::llm::AgentRole::ALL
            .into_iter()
            .map(|role| {
                (
                    role,
                    cx.new(|cx| NativeTextInput::new("Optional CLI model", cx)),
                )
            })
            .collect::<HashMap<_, _>>();
        let agent_custom_executable_inputs = hvc_core::llm::AgentRole::ALL
            .into_iter()
            .map(|role| {
                (
                    role,
                    cx.new(|cx| NativeTextInput::new("Absolute executable path", cx)),
                )
            })
            .collect::<HashMap<_, _>>();
        let agent_custom_args_inputs = hvc_core::llm::AgentRole::ALL
            .into_iter()
            .map(|role| {
                (
                    role,
                    cx.new(|cx| NativeTextInput::new("JSON arguments: [\"--flag\",\"value\"]", cx)),
                )
            })
            .collect::<HashMap<_, _>>();
        let pulsations_split =
            cx.new(|_| crate::split_pane::SplitPaneState::new(292.0, 220.0, 480.0, 420.0));
        let workspace_tree_split =
            cx.new(|_| crate::split_pane::SplitPaneState::new(220.0, 160.0, 400.0, 480.0));
        let workspace_preview_split =
            cx.new(|_| crate::split_pane::SplitPaneState::proportional(0.6, 240.0, 220.0));
        let workspace_context_split =
            cx.new(|_| crate::split_pane::SplitPaneState::proportional(0.78, 480.0, 180.0));
        let forge_feature_split =
            cx.new(|_| crate::split_pane::SplitPaneState::new(245.0, 180.0, 420.0, 460.0));
        let forge_incubation_split =
            cx.new(|_| crate::split_pane::SplitPaneState::proportional(0.55, 300.0, 260.0));
        let forge_triage_split =
            cx.new(|_| crate::split_pane::SplitPaneState::proportional(0.58, 300.0, 260.0));
        let mut subscriptions = vec![
            cx.observe(&pulsations_split, |_, _, cx| cx.notify()),
            cx.observe(&workspace_tree_split, |_, _, cx| cx.notify()),
            cx.observe(&workspace_preview_split, |_, _, cx| cx.notify()),
            cx.observe(&workspace_context_split, |_, _, cx| cx.notify()),
            cx.observe(&forge_feature_split, |_, _, cx| cx.notify()),
            cx.observe(&forge_incubation_split, |_, _, cx| cx.notify()),
            cx.observe(&forge_triage_split, |_, _, cx| cx.notify()),
            cx.observe(&editor, |model, _, cx| {
                model.handle_editor_update(cx);
                model.schedule_vault_context_documents(cx);
                model.schedule_session_persistence(cx);
            }),
            #[cfg(test)]
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
            cx.subscribe(
                &chat_draft_input,
                |model, _, _: &crate::text_input::Submit, cx| model.send_chat_message(cx),
            ),
            cx.observe(&cli_model_search_input, |_, _, cx| cx.notify()),
        ];
        subscriptions.extend(
            agent_prompt_inputs
                .values()
                .map(|input| cx.observe(input, |_, _, cx| cx.notify())),
        );
        subscriptions.extend(
            agent_model_inputs
                .values()
                .chain(agent_custom_executable_inputs.values())
                .chain(agent_custom_args_inputs.values())
                .map(|input| cx.observe(input, |_, _, cx| cx.notify())),
        );
        Self {
            core,
            forge: crate::forge::ForgeUi::new(cx),
            session_persistence,
            workspace_backend,
            pulsations_backend: None,
            vault_backend: None,
            settings_backend: None,
            agents_backend: None,
            logs_backend: None,
            chat_backend: None,
            navigation,
            workspace,
            pulsations: cx.new(|_| PulsationsModel::default()),
            pulsations_split,
            workspace_tree_split,
            workspace_preview_split,
            workspace_context_split,
            forge_feature_split,
            forge_incubation_split,
            forge_triage_split,
            vault: cx.new(|_| VaultModel::default()),
            #[cfg(test)]
            pulsation_feature_input,
            reconciliation_input,
            description_review_input,
            stage_feedback_input,
            preparation_threshold_input,
            context_limit_input,
            specification_answer_inputs: Vec::new(),
            preparation_scope: None,
            learning_graph_decisions: HashMap::new(),
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
            cli_model_search_input,
            agent_prompt_inputs,
            agent_model_inputs,
            agent_custom_executable_inputs,
            agent_custom_args_inputs,
            free_form_prompt_inputs: HashMap::new(),
            free_form_label_inputs: HashMap::new(),
            free_form_model_inputs: HashMap::new(),
            free_form_custom_executable_inputs: HashMap::new(),
            free_form_custom_args_inputs: HashMap::new(),
            chat: cx.new(|_| ChatModel::default()),
            agents: cx.new(|_| AgentsModel::default()),
            settings: cx.new(|_| SettingsModel::default()),
            logs: cx.new(|_| LogsModel::default()),
            active_view,
            active_tab,
            last_view_per_tab,
            status: "Ready".into(),
            event_count: 0,
            needs_resync: false,
            indexing_status: None,
            model_pull_status: None,
            notifications: crate::notifications::Notifications::default(),
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
            hitl_documents_task: None,
            failure_task: None,
            failure_detail_task: None,
            backend_override_task: None,
            vault_refresh_task: None,
            vault_search_task: None,
            vault_rename_task: None,
            vault_context_task: None,
            vault_document_tasks: HashMap::new(),
            settings_task: None,
            embedding_readiness_task: None,
            model_pull_task: None,
            agents_task: None,
            agents_notice_timer: None,
            model_catalog_task: None,
            logs_task: None,
            logs_resync_tasks: HashMap::new(),
            chat_task: None,
            chat_resync_tasks: HashMap::new(),
            session_task: None,
            performance_probe: None,
            confirm_workspace_overwrite: None,
            confirm_hitl_reject: false,
            confirm_stage_reject: false,
            confirm_failure_abort: false,
            confirm_brownfield_revoke: false,
            active_dropdown: None,
            choice_menu: None,
            auto_open_review: false,
            _subscriptions: subscriptions,
        }
    }

    pub fn active_view(&self) -> AppView {
        self.active_view
    }

    pub fn active_tab(&self) -> AppTab {
        self.active_tab
    }

    pub fn select_tab(&mut self, tab: AppTab, cx: &mut Context<Self>) {
        let view = self
            .last_view_per_tab
            .get(&tab)
            .copied()
            .unwrap_or_else(|| tab.default_view());
        self.select_view(view, cx);
    }

    /// `alt-1`/`alt-2`/`alt-3`: jump to the Nth sub-view of the active header tab (see the UI flow
    /// plan Part 4.5). A no-op when the active tab has fewer sub-views than `index + 1`.
    pub(crate) fn select_nth_sub_view(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(view) = self.active_tab.sub_views().get(index).copied() {
            self.select_view(view, cx);
        }
    }

    /// `ctrl-shift-[` / `ctrl-shift-]`: cycles the current view's own section strip. Each view keeps
    /// its own section enum (Pulsations/Forge/Settings), so this is a small per-view dispatch rather
    /// than a single shared cursor — see the UI flow plan Part 4.5.
    pub(crate) fn cycle_current_section(&mut self, direction: i32, cx: &mut Context<Self>) {
        fn cycle<T: Copy + PartialEq>(all: &[T], current: T, direction: i32) -> T {
            let position = all.iter().position(|item| *item == current).unwrap_or(0) as i32;
            let len = all.len() as i32;
            all[(position + direction).rem_euclid(len) as usize]
        }

        match self.active_view {
            AppView::Pulsations => {
                self.pulsations.update(cx, |pulsations, _| {
                    pulsations.detail_section = cycle(
                        &[
                            PulsationDetailSection::Review,
                            PulsationDetailSection::Overview,
                            PulsationDetailSection::Preparation,
                            PulsationDetailSection::Artifacts,
                        ],
                        pulsations.detail_section,
                        direction,
                    );
                });
                cx.notify();
            }
            AppView::Forge => {
                self.forge.section = cycle(
                    &crate::forge::ForgeSection::ALL,
                    self.forge.section,
                    direction,
                );
                cx.notify();
            }
            AppView::Settings => {
                self.settings.update(cx, |settings, _| {
                    settings.settings_section = cycle(
                        &[
                            crate::settings::SettingsSection::Connections,
                            crate::settings::SettingsSection::Routes,
                            crate::settings::SettingsSection::Runtime,
                            crate::settings::SettingsSection::Usage,
                        ],
                        settings.settings_section,
                        direction,
                    );
                });
                if self.settings.read(cx).settings_section
                    == crate::settings::SettingsSection::Usage
                {
                    self.refresh_usage(cx);
                }
                cx.notify();
            }
            _ => {}
        }
    }

    pub fn status(&self) -> &SharedString {
        &self.status
    }

    pub fn event_count(&self) -> u64 {
        self.event_count
    }

    pub(crate) fn is_disposed(&self) -> bool {
        self.lifecycle.is_disposed()
    }

    pub fn needs_resync(&self) -> bool {
        self.needs_resync
    }

    pub fn indexing_status(&self) -> Option<&hvc_core::orchestration::IndexingEvent> {
        self.indexing_status.as_ref()
    }

    pub fn model_pull_status(&self) -> Option<&hvc_core::embedding_service::ModelPullEvent> {
        self.model_pull_status.as_ref()
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

    #[cfg(test)]
    pub(crate) fn current_notice(&self) -> Option<&crate::session::HeaderNotice> {
        self.notifications.queue.current()
    }

    /// Per-tab attention counters (see the UI flow plan Part 4.4). Reviews and running jobs belong
    /// to Engine (where Pulsations lives) rather than Companion, and Agent/Companion each get their
    /// own signal instead of always showing zero.
    pub fn sidebar_badges(&self, cx: &gpui::App) -> TabBadges {
        let pulsations = self.pulsations.read(cx);
        TabBadges {
            agent_failed_runs: self.agents.read(cx).failed_agent_count(),
            engine_awaiting_hitl: pulsations.awaiting_hitl,
            engine_running_jobs: pulsations.running_jobs,
            companion_streaming: usize::from(self.chat.read(cx).streaming),
        }
    }

    pub fn select_view(&mut self, view: AppView, cx: &mut Context<Self>) {
        if self.active_view != view {
            self.close_cli_model_picker(cx);
        }
        self.active_view = view;
        if let Some(tab) = AppTab::from_view(view) {
            self.active_tab = tab;
            self.last_view_per_tab.insert(tab, view);
        }
        self.navigation.update(cx, |navigation, _| {
            navigation.active = view;
            if let Some(tab) = AppTab::from_view(view) {
                navigation.active_tab = tab;
            }
        });
        self.pulsations.update(cx, |pulsations, _| {
            pulsations.diff_expanded = false;
        });
        self.confirm_workspace_overwrite = None;
        self.confirm_hitl_reject = false;
        self.confirm_stage_reject = false;
        self.confirm_failure_abort = false;
        self.confirm_brownfield_revoke = false;
        self.active_dropdown = None;
        self.choice_menu = None;
        self.status = format!("{} view active", view.short_label()).into();
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
            AppView::Pulsations => self.refresh_pulsation_usage(cx),
            AppView::Forge => self.refresh_forge(cx),
            AppView::Agents => self.refresh_agents(cx),
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

    /// Settings > Runtime > "Auto-open review" (default off; recommended on). See the UI flow
    /// plan Part 4.3.
    pub fn set_auto_open_review(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.auto_open_review = enabled;
        self.schedule_session_persistence(cx);
        cx.notify();
    }

    pub fn dismiss_header_notice(&mut self, cx: &mut Context<Self>) {
        self.notifications.queue.dismiss();
        cx.notify();
    }

    #[track_caller]
    pub(crate) fn push_header_notice(
        &mut self,
        kind: HeaderNoticeKind,
        message: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        self.push_actionable_notice(kind, message, None, cx);
    }

    #[track_caller]
    pub(crate) fn push_actionable_notice(
        &mut self,
        kind: HeaderNoticeKind,
        message: impl Into<String>,
        action: Option<HeaderNoticeAction>,
        cx: &mut Context<Self>,
    ) {
        let project = self
            .workspace
            .read(cx)
            .project_id
            .as_ref()
            .map(ToString::to_string);
        self.notifications.queue.set_project(project);
        self.notifications
            .queue
            .push_actionable(kind, message, action);
        cx.notify();
    }

    pub fn choose_workspace(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(core) = self.core.clone() else {
            self.status = "Picker unavailable in test environment".into();
            cx.notify();
            return;
        };

        let selected = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Select workspace".into()),
        });
        self.status = "Waiting for workspace selection…".into();
        cx.notify();

        self.workspace_task = Some(cx.spawn(async move |this, cx| {
            let path = match selected.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                _ => None,
            };
            let Some(path) = path else {
                let _ = this.update(cx, |model, cx| {
                    model.status = "Workspace selection canceled".into();
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
        self.confirm_brownfield_revoke = false;
        let Some(backend) = self.workspace_backend.clone() else {
            self.status = "Brownfield onboarding unavailable in test environment".into();
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
            "Running limited brownfield inventory…".into()
        } else {
            "Revoking brownfield consent…".into()
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
                    Some(error) => format!("Brownfield onboarding failed: {error}").into(),
                    None if consent => "Inventory completed; structural mapping scheduled".into(),
                    None => "Brownfield mapping disabled".into(),
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
            format!("Ignoring `{relative_path}` in brownfield map…").into()
        } else {
            format!("Re-enabling `{relative_path}` in brownfield map…").into()
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
                    Some(error) => format!("Failed to update brownfield ignore: {error}").into(),
                    None if ignored => {
                        format!("`{relative_path}` will be ignored in future scans.").into()
                    }
                    None => {
                        format!("`{relative_path}` was re-enabled and queued for incremental scan.")
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
            self.status = "File reading unavailable in test environment".into();
            cx.notify();
            return;
        };
        self.status = format!("Opening {relative_path}…").into();
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
                    model.status = format!("File opened: {requested_path}").into();
                    model.schedule_session_persistence(cx);
                    cx.notify();
                }
                Err(error) => {
                    model.status = format!("Failed to open {requested_path}").into();
                    model.push_header_notice(HeaderNoticeKind::Error, error.message, cx);
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
            self.status = format!("Active tab: {relative_path}").into();
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

    /// Expands/collapses the Review section's diff surface in Pulsations (hides the pulsation list
    /// for full-width reading). Formerly a Workspace concept; moved alongside the HITL surface — see
    /// the UI flow plan Part 1.2.
    pub fn toggle_pulsation_diff(&mut self, cx: &mut Context<Self>) {
        self.pulsations.update(cx, |pulsations, _| {
            pulsations.diff_expanded = !pulsations.diff_expanded;
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
        self.confirm_workspace_overwrite = None;
        self.file_tasks.insert(task_key, task);
    }

    pub fn set_confirm_workspace_overwrite(
        &mut self,
        path: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.confirm_workspace_overwrite = path;
        cx.notify();
    }

    pub fn set_confirm_hitl_reject(&mut self, value: bool, cx: &mut Context<Self>) {
        self.confirm_hitl_reject = value;
        cx.notify();
    }

    pub fn set_confirm_failure_abort(&mut self, value: bool, cx: &mut Context<Self>) {
        self.confirm_failure_abort = value;
        cx.notify();
    }

    pub fn set_confirm_stage_reject(&mut self, value: bool, cx: &mut Context<Self>) {
        self.confirm_stage_reject = value;
        cx.notify();
    }

    pub fn set_confirm_brownfield_revoke(&mut self, value: bool, cx: &mut Context<Self>) {
        self.confirm_brownfield_revoke = value;
        cx.notify();
    }

    pub fn close_dropdown(&mut self, cx: &mut Context<Self>) {
        if self.active_dropdown.is_some() {
            self.active_dropdown = None;
            self.choice_menu = None;
            cx.notify();
        }
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
                    model.push_header_notice(HeaderNoticeKind::Warning, error, cx);
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
        if self
            .workspace
            .read(cx)
            .project_id
            .as_ref()
            .map(AsRef::as_ref)
            != Some(project_id.as_str())
        {
            return None;
        }
        let forge_changed = crate::forge::task_runtime_changed(
            &self.pulsations.read(cx).pulsations,
            &snapshot.pulsations,
        );
        let previously_awaiting_hitl = self.pulsations.read(cx).awaiting_hitl;
        let previously_awaiting_failure = self.pulsations.read(cx).awaiting_failure;
        let previous_failure = self.pulsations.read(cx).failure.checkpoint.clone();
        let active_id = self.pulsations.update(cx, |pulsations, _| {
            pulsations.apply_snapshot(project_id, snapshot, preferred_id)
        });
        let current_failure = &self.pulsations.read(cx).failure.checkpoint;
        if previous_failure
            .as_ref()
            .map(|c| (&c.checkpoint_id, c.state_version))
            != current_failure
                .as_ref()
                .map(|c| (&c.checkpoint_id, c.state_version))
        {
            self.confirm_failure_abort = false;
        }
        self.refresh_hitl_documents(cx);
        self.refresh_failure_detail(cx);
        self.refresh_pulsation_usage(cx);
        self.sync_preparation_inputs(cx);
        if let Some(active_id) = active_id.clone() {
            self.refresh_backend_override(active_id, cx);
        }
        if forge_changed && self.active_view() == AppView::Forge {
            self.refresh_forge(cx);
        }
        self.notify_review_waiting(previously_awaiting_hitl, cx);
        self.notify_failure_waiting(previously_awaiting_failure, cx);
        cx.notify();
        active_id
    }

    /// Pushes the actionable "review(s) waiting" notice the moment a checkpoint newly appears
    /// (`awaiting_hitl` going from 0 to more than 0), and — only when the developer has opted into
    /// `auto_open_review` and nothing suggests they're mid-composition elsewhere — jumps straight
    /// there. See the UI flow plan Part 4.2.
    /// The failure twin of `notify_review_waiting`. Kept separate rather than folded into
    /// the review counter: a parked failure raising `awaiting_hitl` would trip that
    /// function's `previously_awaiting_hitl != 0` guard and silently swallow the notice
    /// for a genuine artifact review opening afterwards.
    fn notify_failure_waiting(
        &mut self,
        previously_awaiting_failure: usize,
        cx: &mut Context<Self>,
    ) {
        let (awaiting, failure_pulsation_id) = {
            let pulsations = self.pulsations.read(cx);
            let id = pulsations
                .active()
                .filter(|pulsation| {
                    pulsation.status == PulsationStatus::Checkpoint
                        && pulsations
                            .failure_checkpoints
                            .iter()
                            .any(|c| c.pulsation_id == pulsation.id)
                })
                .map(|pulsation| pulsation.id.clone());
            (pulsations.awaiting_failure, id)
        };
        if previously_awaiting_failure != 0 || awaiting == 0 {
            return;
        }
        let Some(pulsation_id) = failure_pulsation_id else {
            return;
        };
        let message = if awaiting == 1 {
            "1 stage failure waiting on a decision".to_string()
        } else {
            format!("{awaiting} stage failures waiting on a decision")
        };
        self.push_actionable_notice(
            HeaderNoticeKind::Error,
            message,
            Some(HeaderNoticeAction {
                label: "Decide now".into(),
                pulsation_id: pulsation_id.clone(),
            }),
            cx,
        );
        if self.auto_open_review && !self.auto_open_review_blocked(cx) {
            self.open_pulsation_review(pulsation_id, cx);
        }
    }

    fn notify_review_waiting(&mut self, previously_awaiting_hitl: usize, cx: &mut Context<Self>) {
        let (awaiting_hitl, checkpoint_pulsation_id) = {
            let pulsations = self.pulsations.read(cx);
            let checkpoint_id = pulsations
                .active()
                .filter(|pulsation| {
                    pulsation.status == PulsationStatus::Checkpoint
                        && pulsations
                            .hitl_checkpoints
                            .iter()
                            .any(|c| c.pulsation_id == pulsation.id)
                })
                .map(|pulsation| pulsation.id.clone());
            (pulsations.awaiting_hitl, checkpoint_id)
        };
        if previously_awaiting_hitl != 0 || awaiting_hitl == 0 {
            return;
        }
        let Some(pulsation_id) = checkpoint_pulsation_id else {
            return;
        };
        let message = if awaiting_hitl == 1 {
            "1 review waiting".to_string()
        } else {
            format!("{awaiting_hitl} reviews waiting")
        };
        self.push_actionable_notice(
            HeaderNoticeKind::Warning,
            message,
            Some(HeaderNoticeAction {
                label: "Review now".into(),
                pulsation_id: pulsation_id.clone(),
            }),
            cx,
        );
        if self.auto_open_review && !self.auto_open_review_blocked(cx) {
            self.open_pulsation_review(pulsation_id, cx);
        }
    }

    /// A deliberately conservative proxy for "the developer looks mid-composition somewhere else":
    /// an overlay is open, or the Companion composer has a draft. Checking whether an arbitrary
    /// `NativeTextInput` currently holds keyboard focus would need a `Window` reference this
    /// background poll callback doesn't have.
    fn auto_open_review_blocked(&self, cx: &Context<Self>) -> bool {
        self.forge.new_open
            || self.active_dropdown.is_some()
            || self.notifications.history_open
            || self.agents.read(cx).active_model_picker.is_some()
            || (self.active_view() == AppView::Chat
                && !self.chat_draft_input.read(cx).value().trim().is_empty())
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

    /// Fetches whether the given pulsation's current stage is silently
    /// running on a failover backend (`elect_next_backend` in hvc-core),
    /// so the detail panel can offer an explicit way back to the project's
    /// configured default. Never applied to a pulsation the user has since
    /// navigated away from.
    fn refresh_backend_override(&mut self, pulsation_id: String, cx: &mut Context<Self>) {
        let Some(core) = self.core.clone() else {
            return;
        };
        let runtime = core.runtime_handle().clone();
        let scope = pulsation_id.clone();
        self.backend_override_task = Some(cx.spawn(async move |this, cx| {
            let result = runtime
                .spawn_blocking(move || {
                    core.terminals()
                        .repository()
                        .backend_override_status(&pulsation_id)
                })
                .await;
            let status = match result {
                Ok(Ok(status)) => status,
                _ => None,
            };
            let _ = this.update(cx, |model, cx| {
                model.pulsations.update(cx, |pulsations, _| {
                    if pulsations.active_id().as_deref() == Some(scope.as_str()) {
                        pulsations.backend_override = status;
                    }
                });
                cx.notify();
            });
        }));
    }

    /// Explicit undo for an automatic CLI backend failover: clears the
    /// pinned override on the active pulsation's current stage so its next
    /// job pickup falls back to the project's configured default backend.
    /// Deliberately a user-triggered button rather than something a
    /// background CLI-discovery refresh does on its own.
    pub(crate) fn clear_backend_override_and_retry(&mut self, cx: &mut Context<Self>) {
        let Some(pulsation_id) = self.pulsations.read(cx).active_id() else {
            return;
        };
        let Some(agent_role) = self
            .pulsations
            .read(cx)
            .backend_override
            .as_ref()
            .map(|status| status.agent_role)
        else {
            return;
        };
        let Some(core) = self.core.clone() else {
            return;
        };
        self.pulsations.update(cx, |pulsations, _| {
            pulsations.backend_override_busy = true;
        });
        cx.notify();
        let runtime = core.runtime_handle().clone();
        self.backend_override_task = Some(cx.spawn(async move |this, cx| {
            let result = runtime
                .spawn_blocking(move || {
                    core.persistence()
                        .clear_execution_override_and_retry(ClearExecutionOverrideRequest {
                            pulsation_id,
                            agent_role,
                        })
                        .map_err(|error| error.to_string())
                })
                .await
                .map_err(|error| format!("Backend override worker interrupted: {error}"))
                .and_then(|result| result);
            let _ = this.update(cx, |model, cx| {
                model.pulsations.update(cx, |pulsations, _| {
                    pulsations.backend_override_busy = false;
                });
                match result {
                    Ok(response) => {
                        model.pulsations.update(cx, |pulsations, _| {
                            pulsations.backend_override = None;
                            pulsations.notice = Some(if response.retried {
                                "Override cleared; the stage will retry on the default backend."
                                    .to_string()
                            } else {
                                "No active override to clear.".to_string()
                            });
                        });
                        model.refresh_pulsations(cx);
                    }
                    Err(error) => {
                        model.push_header_notice(HeaderNoticeKind::Error, error, cx);
                    }
                }
                cx.notify();
            });
        }));
    }

    pub(crate) fn refresh_hitl_documents(&mut self, cx: &mut Context<Self>) {
        let Some(backend) = self.pulsations_backend.clone() else {
            return;
        };
        let Some((generation, checkpoint)) = self
            .pulsations
            .update(cx, |state, _| state.hitl.begin_documents())
        else {
            return;
        };
        let project_id = self.workspace.read(cx).project_id.clone();
        self.hitl_documents_task = Some(cx.spawn(async move |this, cx| {
            let result = backend.review_documents(checkpoint.clone()).await;
            let _ = this.update(cx, |model, cx| {
                if model.workspace.read(cx).project_id != project_id {
                    return;
                }
                model.pulsations.update(cx, |state, _| {
                    state.hitl.finish_documents(generation, &checkpoint, result);
                });
                cx.notify();
            });
        }));
    }

    pub(crate) fn refresh_failure_detail(&mut self, cx: &mut Context<Self>) {
        let Some(backend) = self.pulsations_backend.clone() else {
            return;
        };
        let Some((generation, checkpoint_id)) = self
            .pulsations
            .update(cx, |state, _| state.failure.begin_detail())
        else {
            return;
        };
        let project_id = self.workspace.read(cx).project_id.clone();
        self.failure_detail_task = Some(cx.spawn(async move |this, cx| {
            let result = backend.stage_failure_detail(checkpoint_id.clone()).await;
            let _ = this.update(cx, |model, cx| {
                if model.workspace.read(cx).project_id != project_id {
                    return;
                }
                model.pulsations.update(cx, |state, _| {
                    state
                        .failure
                        .finish_detail(generation, &checkpoint_id, result);
                });
                cx.notify();
            });
        }));
    }

    pub(crate) fn resolve_stage_failure(
        &mut self,
        decision: StageFailureDecision,
        cx: &mut Context<Self>,
    ) {
        self.confirm_failure_abort = false;
        let request = self
            .pulsations
            .update(cx, |pulsations, _| pulsations.failure.begin(decision));
        let request = request.map(|mut request| {
            if decision == StageFailureDecision::RequestRevision {
                request.feedback = Some(self.stage_feedback_input.read(cx).value().to_owned());
            }
            request
        });
        let Some(request) = request else {
            return;
        };
        let Some(core) = self.core.clone() else {
            self.pulsations.update(cx, |pulsations, _| {
                pulsations.failure.finish(Err(
                    "Stage failure decision unavailable in test environment".into(),
                ));
            });
            cx.notify();
            return;
        };
        self.status = "Applying failure decision…".into();
        cx.notify();
        let runtime = core.runtime_handle().clone();
        let decision_scope = (
            request.checkpoint_id.clone(),
            request.expected_state_version,
        );
        self.failure_task = Some(cx.spawn(async move |this, cx| {
            let result = runtime
                .spawn_blocking(move || {
                    core.persistence()
                        .resolve_stage_failure(request)
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                })
                .await
                .map_err(|error| format!("Failure decision worker interrupted: {error}"))
                .and_then(|result| result);
            let _ = this.update(cx, |model, cx| {
                if model.lifecycle.is_disposed() {
                    return;
                }
                model.pulsations.update(cx, |pulsations, _| {
                    // A snapshot may have replaced the checkpoint while the decision was
                    // in flight; applying the result to a different one would lie.
                    let same_checkpoint = pulsations.failure.checkpoint.as_ref().is_some_and(|c| {
                        c.checkpoint_id == decision_scope.0 && c.state_version == decision_scope.1
                    });
                    if same_checkpoint {
                        pulsations.failure.finish(result.clone());
                        if result.is_ok() {
                            pulsations.awaiting_failure =
                                pulsations.awaiting_failure.saturating_sub(1);
                            pulsations.diff_expanded = false;
                        }
                    }
                });
                match result {
                    Ok(()) => {
                        model.status = "Failure decision applied".into();
                        model.schedule_session_persistence(cx);
                        model.refresh_pulsations(cx);
                    }
                    Err(error) => {
                        model.status = "Failed to apply failure decision".into();
                        model.push_header_notice(HeaderNoticeKind::Error, error, cx);
                    }
                }
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
        let selected = self
            .pulsations
            .update(cx, |pulsations, _| pulsations.select(&pulsation_id));
        if !selected {
            return;
        }
        self.confirm_failure_abort = false;
        self.refresh_hitl_documents(cx);
        self.refresh_failure_detail(cx);
        self.refresh_pulsation_usage(cx);
        self.sync_preparation_inputs(cx);
        self.pulsations.update(cx, |pulsations, _| {
            pulsations.diff_expanded = false;
        });
        self.refresh_pulsation_artifacts(pulsation_id.clone(), cx);
        self.refresh_backend_override(pulsation_id, cx);
        self.refresh_logs(cx);
        self.schedule_session_persistence(cx);
        cx.notify();
    }

    /// Jumps straight to a pulsation's Review section, from anywhere in the app. Used by the
    /// "review(s) waiting" notice's action button and by auto-open-review (see the UI flow plan
    /// Part 4).
    pub(crate) fn open_pulsation_review(&mut self, pulsation_id: String, cx: &mut Context<Self>) {
        self.select_pulsation(pulsation_id, cx);
        self.pulsations.update(cx, |pulsations, _| {
            pulsations.detail_section = PulsationDetailSection::Review;
        });
        self.select_view(AppView::Pulsations, cx);
    }

    #[cfg(test)]
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
                            Some(format!("Pulsation “{}” started.", created.feature_name));
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
                    model.push_header_notice(HeaderNoticeKind::Error, error, cx);
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
            let _ =
                this.update(cx, |model, cx| {
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
                                "Current base adopted. Previous outputs and pulsations preserved."
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
                            Some(format!("{cancelled} job(s) marked for cancellation."));
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
                    model.push_header_notice(HeaderNoticeKind::Error, error, cx);
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
                vault.error =
                    Some("Save the open note before renaming it to avoid losing changes.".into());
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
                    model.push_header_notice(HeaderNoticeKind::Error, error, cx);
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
        self.confirm_hitl_reject = false;
        self.confirm_stage_reject = false;
        self.confirm_failure_abort = false;
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
                    .finish(Err("HITL decision unavailable in test environment".into()));
            });
            cx.notify();
            return;
        };
        self.status = "Applying HITL decision…".into();
        cx.notify();
        let runtime = core.runtime_handle().clone();
        let decision_scope = (
            request.pulsation_id.clone(),
            request.checkpoint_id.clone(),
            request.expected_state_version,
        );
        self.hitl_task = Some(cx.spawn(async move |this, cx| {
            let result = runtime
                .spawn_blocking(move || {
                    core.persistence()
                        .apply_hitl_decision(request)
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                })
                .await
                .map_err(|error| format!("HITL worker interrupted: {error}"))
                .and_then(|result| result);
            let _ =
                this.update(cx, |model, cx| {
                    if model.lifecycle.is_disposed() {
                        return;
                    }
                    model.pulsations.update(cx, |pulsations, _| {
                        let same_review = pulsations.hitl.expected_checkpoint.as_ref().is_some_and(
                            |checkpoint| {
                                checkpoint.pulsation_id == decision_scope.0
                                    && checkpoint.checkpoint_id == decision_scope.1
                                    && checkpoint.state_version == decision_scope.2
                            },
                        );
                        if same_review {
                            pulsations.hitl.finish(result.clone());
                            if result.is_ok() {
                                pulsations.awaiting_hitl =
                                    pulsations.awaiting_hitl.saturating_sub(1);
                                pulsations.diff_expanded = false;
                            }
                        }
                    });
                    match result {
                        Ok(()) => {
                            model.status = "HITL decision applied".into();
                            model.schedule_session_persistence(cx);
                            model.refresh_pulsations(cx);
                        }
                        Err(error) => {
                            model.status = "Failed to apply HITL decision".into();
                            model.push_header_notice(HeaderNoticeKind::Error, error, cx);
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
                    .expect("default selection must be valid");
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
                self.status = format!("File saved: {relative_path}").into();
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
                    format!("Revision conflict: {relative_path}")
                } else {
                    format!("Save failed: {relative_path}")
                }
                .into();
                self.push_header_notice(
                    if conflict {
                        HeaderNoticeKind::Warning
                    } else {
                        HeaderNoticeKind::Error
                    },
                    message,
                    cx,
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

    pub(crate) fn set_chat_transport(
        &mut self,
        transport: ChatTransportKind,
        cx: &mut Context<Self>,
    ) {
        self.chat.update(cx, |chat, _| {
            chat.selected_transport = transport;
        });
        self.close_cli_model_picker(cx);
        self.close_dropdown(cx);
    }

    pub(crate) fn set_chat_cli_backend(&mut self, backend: CliBackend, cx: &mut Context<Self>) {
        if self.chat.read(cx).selected_cli_backend == backend {
            self.close_dropdown(cx);
            return;
        }
        self.chat.update(cx, |chat, _| {
            chat.selected_cli_backend = backend;
            chat.selected_cli_model = None;
            chat.selected_cli_model_profile = None;
            chat.selected_cli_model_source = CliModelSelectionSource::Default;
        });
        self.close_cli_model_picker(cx);
        self.close_dropdown(cx);
        if !self.agents.read(cx).model_catalogs.contains_key(&backend) {
            self.load_cli_model_catalog(backend, false, cx);
        }
    }

    pub(crate) fn set_chat_cli_profile(&mut self, profile: Option<String>, cx: &mut Context<Self>) {
        self.chat.update(cx, |chat, _| {
            chat.selected_cli_model_profile = profile;
        });
        self.close_dropdown(cx);
    }

    pub(crate) fn set_chat_connection(&mut self, connection_id: String, cx: &mut Context<Self>) {
        self.chat.update(cx, |chat, _| {
            chat.selected_connection_id = Some(connection_id.into());
        });
        self.close_dropdown(cx);
    }

    pub(crate) fn open_companion_model_picker(&mut self, cx: &mut Context<Self>) {
        let backend = self.chat.read(cx).selected_cli_backend;
        self.open_cli_model_picker(CliModelPickerTarget::Companion, backend, cx);
    }

    pub(crate) fn create_chat_conversation(&mut self, cx: &mut Context<Self>) {
        let Some(backend) = self.chat_backend.clone() else {
            return;
        };
        let (project_id, target) = {
            let chat = self.chat.read(cx);
            let Some(project_id) = chat.project_id.as_ref().map(ToString::to_string) else {
                return;
            };
            let target = match chat.selected_transport {
                ChatTransportKind::Api => {
                    let Some(connection_id) = chat
                        .selected_connection_id
                        .as_ref()
                        .map(ToString::to_string)
                    else {
                        return;
                    };
                    ChatExecutionTargetRequest::Api { connection_id }
                }
                ChatTransportKind::Cli => ChatExecutionTargetRequest::Cli {
                    backend: chat.selected_cli_backend,
                    model: chat.selected_cli_model.clone(),
                    model_profile: chat.selected_cli_model_profile.clone(),
                },
            };
            (project_id, target)
        };
        if !self.chat.read(cx).new_conversation_target_available() {
            return;
        }
        if !self.chat.update(cx, |chat, _| chat.begin_busy()) {
            return;
        }
        cx.notify();
        self.chat_task = Some(cx.spawn(async move |this, cx| {
            let result = backend.create_conversation(project_id, target).await;
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
                                "Conversation not found.",
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
        self.status = "Diff copied; no changes were applied".into();
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

    pub(crate) fn set_agents_tab(&mut self, tab: AgentsTab, cx: &mut Context<Self>) {
        self.agents.update(cx, |agents, _| {
            agents.tab = tab;
            if tab == AgentsTab::Api
                && !matches!(
                    agents.selected_agent,
                    Some(crate::agents::AgentIdentifier::Canonical(_))
                )
            {
                agents.selected_agent = agents
                    .snapshot
                    .as_ref()
                    .and_then(|snapshot| snapshot.agents.first())
                    .map(|agent| {
                        crate::agents::AgentIdentifier::Canonical(agent.config.agent_role)
                    });
            }
            agents.active_model_picker = None;
        });
        if tab == AgentsTab::Api && self.settings.read(cx).snapshot.is_none() {
            self.refresh_settings(cx);
        }
        cx.notify();
    }

    pub(crate) fn toggle_agents_viewer(&mut self, cx: &mut Context<Self>) {
        self.agents.update(cx, |agents, _| {
            agents.viewer_enabled = !agents.viewer_enabled
        });
        cx.notify();
    }

    pub(crate) fn configure_agent_api(
        &mut self,
        role: hvc_core::llm::AgentRole,
        cx: &mut Context<Self>,
    ) {
        let descriptor = hvc_core::terminals::canonical_agent(role);
        let (route_id, connection_id) = {
            let agents = self.agents.read(cx);
            let settings = self.settings.read(cx);
            let project_id = agents
                .snapshot
                .as_ref()
                .map(|snapshot| snapshot.project_id.as_str());
            let route = settings.snapshot.as_ref().and_then(|snapshot| {
                let bound_route = project_id.and_then(|project_id| {
                    snapshot.bindings.iter().find(|binding| {
                        binding.project_id == project_id
                            && binding.agent_role == role.as_str()
                            && binding.stage.eq_ignore_ascii_case(descriptor.stage)
                    })
                });
                bound_route
                    .and_then(|binding| {
                        snapshot
                            .routes
                            .iter()
                            .find(|route| route.id == binding.route_id)
                    })
                    .or_else(|| snapshot.routes.first())
            });
            let connection_id = route
                .and_then(|route| {
                    route
                        .targets
                        .iter()
                        .filter(|target| target.enabled)
                        .min_by_key(|target| target.position)
                })
                .map(|target| target.connection_id.clone())
                .or_else(|| {
                    settings
                        .snapshot
                        .as_ref()
                        .and_then(|snapshot| snapshot.connections.first())
                        .map(|connection| connection.id.clone())
                });
            (route.map(|route| route.id.clone()), connection_id)
        };
        self.agents.update(cx, |agents, _| {
            agents.selected_api_role = Some(role);
        });
        self.settings.update(cx, |settings, _| {
            settings.binding_agent_role = role.as_str().into();
            settings.binding_stage = descriptor.stage.into();
        });
        if let Some(route_id) = route_id {
            self.select_settings_route(route_id, cx);
        }
        if let Some(connection_id) = connection_id {
            self.select_settings_connection(connection_id, cx);
        }
        cx.notify();
    }

    pub(crate) fn save_agent_api_binding(
        &mut self,
        role: hvc_core::llm::AgentRole,
        cx: &mut Context<Self>,
    ) {
        self.configure_agent_api(role, cx);
        self.save_settings_binding(cx);
    }

    pub(crate) fn open_advanced_api_settings(
        &mut self,
        role: hvc_core::llm::AgentRole,
        cx: &mut Context<Self>,
    ) {
        self.configure_agent_api(role, cx);
        self.select_view(AppView::Settings, cx);
    }

    pub(crate) fn refresh_agents(&mut self, cx: &mut Context<Self>) {
        self.refresh_agents_inner(false, cx);
    }

    pub(crate) fn refresh_agent_diagnostics(&mut self, cx: &mut Context<Self>) {
        self.refresh_agents_inner(true, cx);
    }

    pub(crate) fn toggle_agent_follow_tail(&mut self, key: String, cx: &mut Context<Self>) {
        self.agents.update(cx, |agents, _| {
            let enabled = agents.follow_tail.get(&key).copied().unwrap_or(true);
            agents.follow_tail.insert(key, !enabled);
        });
        cx.notify();
    }

    pub(crate) fn copy_agent_history(&mut self, run_id: String, cx: &mut Context<Self>) {
        let content = self
            .agents
            .read(cx)
            .events_by_run
            .get(&run_id)
            .map(|events| {
                events
                    .iter()
                    .map(|event| {
                        format!(
                            "{}\t{}\t{}",
                            event.sequence,
                            match event.kind {
                                hvc_core::terminals::TerminalEventKind::System => "system",
                                hvc_core::terminals::TerminalEventKind::Message => "message",
                                hvc_core::terminals::TerminalEventKind::ToolUse => "tool_use",
                                hvc_core::terminals::TerminalEventKind::ToolResult => "tool_result",
                                hvc_core::terminals::TerminalEventKind::Stdout => "stdout",
                                hvc_core::terminals::TerminalEventKind::Stderr => "stderr",
                                hvc_core::terminals::TerminalEventKind::Warning => "warning",
                                hvc_core::terminals::TerminalEventKind::Error => "error",
                                hvc_core::terminals::TerminalEventKind::Completed => "completed",
                            },
                            event.content
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        cx.write_to_clipboard(ClipboardItem::new_string(content));
        self.notifications
            .copied
            .insert(run_id, std::time::Instant::now());
        cx.notify();
    }

    pub(crate) fn export_agent_history(&mut self, run_id: String, cx: &mut Context<Self>) {
        let Some(backend) = self.agents_backend.clone() else {
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
        let generation = self.agents.update(cx, |agents, _| agents.begin(false));
        cx.notify();
        self.agents_task = Some(cx.spawn(async move |this, cx| {
            let result = backend.export_history(project_id, run_id).await;
            let _ = this.update(cx, |model, cx| {
                match result {
                    Ok(path) => {
                        model.agents.update(cx, |agents, _| {
                            if agents.generation == generation {
                                agents.busy = false;
                                agents.notice =
                                    Some(format!("Redacted history exported to {path}"));
                            }
                        });
                        model.schedule_agents_notice_dismiss(cx);
                    }
                    Err(error) => {
                        model.agents.update(cx, |agents, _| {
                            agents.fail(generation, error);
                        });
                    }
                }
                cx.notify();
            });
        }));
    }

    fn refresh_agents_inner(&mut self, force_discovery: bool, cx: &mut Context<Self>) {
        if self.lifecycle.is_disposed() {
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
        let Some(backend) = self.agents_backend.clone() else {
            return;
        };
        let generation = self.agents.update(cx, |agents, _| agents.begin(true));
        cx.notify();
        self.agents_task = Some(cx.spawn(async move |this, cx| {
            let result = if force_discovery {
                backend.refresh_snapshot(project_id).await
            } else {
                backend.snapshot(project_id).await
            };
            let _ = this.update(cx, |model, cx| {
                if model.lifecycle.is_disposed() {
                    return;
                }
                match result {
                    Ok(snapshot) => {
                        model.sync_agent_config_inputs(&snapshot, cx);
                        model.agents.update(cx, |agents, _| {
                            agents.apply_snapshot(generation, snapshot);
                        });
                    }
                    Err(error) => {
                        let message = error.presentable();
                        if model
                            .agents
                            .update(cx, |agents, _| agents.fail(generation, error))
                        {
                            model.push_header_notice(HeaderNoticeKind::Error, message, cx);
                        }
                    }
                }
                cx.notify();
            });
        }));
    }

    pub(crate) fn current_agent_config(
        &self,
        role: hvc_core::llm::AgentRole,
        cx: &gpui::App,
    ) -> Option<CliAgentConfig> {
        self.agents
            .read(cx)
            .snapshot
            .as_ref()?
            .agents
            .iter()
            .find(|agent| agent.config.agent_role == role)
            .map(|agent| agent.config.clone())
    }

    fn sync_agent_config_inputs(
        &mut self,
        snapshot: &AgentsDashboardSnapshot,
        cx: &mut Context<Self>,
    ) {
        for agent in &snapshot.terminals.agents {
            let role = agent.config.agent_role;
            if let Some(input) = self.agent_model_inputs.get(&role) {
                input.update(cx, |input, cx| {
                    input.set_value(agent.config.model.clone().unwrap_or_default(), cx)
                });
            }
            if let Some(input) = self.agent_custom_executable_inputs.get(&role) {
                input.update(cx, |input, cx| {
                    input.set_value(
                        agent.config.custom_executable.clone().unwrap_or_default(),
                        cx,
                    )
                });
            }
            if let Some(input) = self.agent_custom_args_inputs.get(&role) {
                let args = serde_json::to_string(&agent.config.custom_args)
                    .unwrap_or_else(|_| "[]".to_string());
                input.update(cx, |input, cx| input.set_value(args, cx));
            }
        }
        for free_form in &snapshot.terminals.free_form_slots {
            let slot_id = free_form.slot.id.clone();
            if !self.free_form_prompt_inputs.contains_key(&slot_id) {
                let prompt = cx.new(|cx| NativeTextInput::new("Manual prompt", cx));
                let label = cx.new(|cx| NativeTextInput::new("Free-form agent name", cx));
                let model = cx.new(|cx| NativeTextInput::new("Optional CLI model", cx));
                let executable = cx.new(|cx| NativeTextInput::new("Absolute executable path", cx));
                let args =
                    cx.new(|cx| NativeTextInput::new("JSON arguments: [\"--flag\",\"value\"]", cx));
                for input in [&prompt, &label, &model, &executable, &args] {
                    self._subscriptions
                        .push(cx.observe(input, |_, _, cx| cx.notify()));
                }
                self.free_form_prompt_inputs.insert(slot_id.clone(), prompt);
                self.free_form_label_inputs.insert(slot_id.clone(), label);
                self.free_form_model_inputs.insert(slot_id.clone(), model);
                self.free_form_custom_executable_inputs
                    .insert(slot_id.clone(), executable);
                self.free_form_custom_args_inputs
                    .insert(slot_id.clone(), args);
            }
            if let Some(input) = self.free_form_label_inputs.get(&slot_id) {
                input.update(cx, |input, cx| {
                    input.set_value(free_form.slot.label.clone(), cx)
                });
            }
            if let Some(input) = self.free_form_model_inputs.get(&slot_id) {
                input.update(cx, |input, cx| {
                    input.set_value(free_form.slot.model.clone().unwrap_or_default(), cx)
                });
            }
            if let Some(input) = self.free_form_custom_executable_inputs.get(&slot_id) {
                input.update(cx, |input, cx| {
                    input.set_value(
                        free_form.slot.custom_executable.clone().unwrap_or_default(),
                        cx,
                    )
                });
            }
            if let Some(input) = self.free_form_custom_args_inputs.get(&slot_id) {
                let args = serde_json::to_string(&free_form.slot.custom_args)
                    .unwrap_or_else(|_| "[]".to_string());
                input.update(cx, |input, cx| input.set_value(args, cx));
            }
        }
    }

    pub(crate) fn save_agent_config(&mut self, config: CliAgentConfig, cx: &mut Context<Self>) {
        let Some(backend) = self.agents_backend.clone() else {
            return;
        };
        let request = UpdateCliAgentConfigRequest {
            project_id: config.project_id,
            agent_role: config.agent_role,
            execution_mode: config.execution_mode,
            backend: config.backend,
            model: config.model,
            model_profile: config.model_profile,
            model_source: config.model_source,
            thinking_level: config.thinking_level,
            custom_executable: config.custom_executable,
            custom_args: config.custom_args,
            custom_risk_ack: config.custom_risk_ack,
            expected_version: config.version,
        };
        let generation = self.agents.update(cx, |agents, _| agents.begin(false));
        cx.notify();
        self.agents_task = Some(cx.spawn(async move |this, cx| {
            let result = backend.update_config(request).await;
            let _ = this.update(cx, |model, cx| {
                match result {
                    Ok(updated) => {
                        model.agents.update(cx, |agents, _| {
                            agents.apply_config(generation, updated);
                        });
                        model.schedule_agents_notice_dismiss(cx);
                    }
                    Err(error) => {
                        let message = error.presentable();
                        if model
                            .agents
                            .update(cx, |agents, _| agents.fail(generation, error))
                        {
                            model.push_header_notice(HeaderNoticeKind::Error, message, cx);
                        }
                    }
                }
                cx.notify();
            });
        }));
    }

    pub(crate) fn schedule_agents_notice_dismiss(&mut self, cx: &mut Context<Self>) {
        let timer = cx.background_executor().clone();
        self.agents_notice_timer = Some(cx.spawn(async move |this, cx| {
            timer.timer(std::time::Duration::from_secs(4)).await;
            let _ = this.update(cx, |model, cx| {
                model.agents.update(cx, |agents, _| {
                    agents.notice = None;
                });
                cx.notify();
            });
        }));
    }

    fn load_cli_model_catalog(
        &mut self,
        backend_kind: CliBackend,
        refresh: bool,
        cx: &mut Context<Self>,
    ) {
        if backend_kind == CliBackend::Custom {
            return;
        }
        let Some(backend) = self.agents_backend.clone() else {
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
        self.agents.update(cx, |agents, _| {
            agents.catalog_loading_backend = Some(backend_kind);
            agents.catalog_error = None;
        });
        cx.notify();
        self.model_catalog_task = Some(cx.spawn(async move |this, cx| {
            let result = backend
                .model_catalog(project_id, backend_kind, refresh)
                .await;
            let _ = this.update(cx, |model, cx| {
                model.agents.update(cx, |agents, _| match result {
                    Ok(catalog) => agents.apply_model_catalog(catalog),
                    Err(error) => {
                        agents.catalog_loading_backend = None;
                        agents.catalog_error = Some(error.presentable());
                    }
                });
                cx.notify();
            });
        }));
    }

    fn open_cli_model_picker(
        &mut self,
        target: CliModelPickerTarget,
        backend: CliBackend,
        cx: &mut Context<Self>,
    ) {
        if backend == CliBackend::Custom {
            return;
        }
        self.cli_model_search_input
            .update(cx, |input, cx| input.clear(cx));
        let needs_catalog = self.agents.update(cx, |agents, _| {
            agents.active_model_picker = Some(CliModelPickerState { target, backend });
            agents.catalog_error = None;
            !agents.model_catalogs.contains_key(&backend)
        });
        if needs_catalog {
            self.load_cli_model_catalog(backend, false, cx);
        }
        cx.notify();
    }

    pub(crate) fn open_agent_model_picker(
        &mut self,
        role: hvc_core::llm::AgentRole,
        cx: &mut Context<Self>,
    ) {
        let Some(config) = self.current_agent_config(role, cx) else {
            return;
        };
        self.open_cli_model_picker(CliModelPickerTarget::Canonical(role), config.backend, cx);
    }

    pub(crate) fn open_free_form_model_picker(&mut self, slot_id: String, cx: &mut Context<Self>) {
        let Some(slot) = self.current_free_form_slot(&slot_id, cx) else {
            return;
        };
        self.open_cli_model_picker(CliModelPickerTarget::FreeForm(slot_id), slot.backend, cx);
    }

    pub(crate) fn close_cli_model_picker(&mut self, cx: &mut Context<Self>) {
        self.agents.update(cx, |agents, _| {
            agents.active_model_picker = None;
            agents.catalog_error = None;
        });
        cx.notify();
    }

    pub(crate) fn refresh_cli_model_catalog(&mut self, cx: &mut Context<Self>) {
        let backend = self
            .agents
            .read(cx)
            .active_model_picker
            .as_ref()
            .map(|picker| picker.backend);
        if let Some(backend) = backend {
            self.load_cli_model_catalog(backend, true, cx);
        }
    }

    pub(crate) fn select_cli_model(
        &mut self,
        model_id: Option<String>,
        source: CliModelSelectionSource,
        cx: &mut Context<Self>,
    ) {
        let target = self.agents.read(cx).active_model_picker.clone();
        let Some(target) = target else {
            return;
        };
        match target.target {
            CliModelPickerTarget::Canonical(role) => {
                let Some(mut config) = self.current_agent_config(role, cx) else {
                    return;
                };
                config.model = model_id.clone();
                config.model_profile = None;
                config.model_source = source;
                config.thinking_level = ThinkingLevel::Default;
                if let Some(input) = self.agent_model_inputs.get(&role) {
                    input.update(cx, |input, cx| {
                        input.set_value(model_id.unwrap_or_default(), cx);
                    });
                }
                self.save_agent_config(config, cx);
            }
            CliModelPickerTarget::FreeForm(slot_id) => {
                let Some(mut slot) = self.current_free_form_slot(&slot_id, cx) else {
                    return;
                };
                slot.model = model_id.clone();
                slot.model_profile = None;
                slot.model_source = source;
                slot.thinking_level = ThinkingLevel::Default;
                if let Some(input) = self.free_form_model_inputs.get(&slot_id) {
                    input.update(cx, |input, cx| {
                        input.set_value(model_id.unwrap_or_default(), cx);
                    });
                }
                self.save_free_form_slot(slot, cx);
            }
            CliModelPickerTarget::Companion => {
                self.chat.update(cx, |chat, _| {
                    chat.selected_cli_model = model_id;
                    chat.selected_cli_model_profile = None;
                    chat.selected_cli_model_source = source;
                });
            }
        }
        self.close_cli_model_picker(cx);
    }

    pub(crate) fn select_manual_cli_model(&mut self, cx: &mut Context<Self>) {
        let model_id = self
            .cli_model_search_input
            .read(cx)
            .value()
            .trim()
            .to_string();
        if model_id.is_empty() {
            return;
        }
        self.select_cli_model(Some(model_id), CliModelSelectionSource::Manual, cx);
    }

    pub(crate) fn model_profiles(
        &self,
        backend: CliBackend,
        model_id: Option<&str>,
        cx: &gpui::App,
    ) -> Vec<String> {
        let Some(model_id) = model_id else {
            return Vec::new();
        };
        self.agents
            .read(cx)
            .model_catalogs
            .get(&backend)
            .and_then(|catalog| {
                catalog
                    .models
                    .iter()
                    .find(|entry| entry.model_id.as_deref() == Some(model_id))
            })
            .map(|entry| entry.profiles.clone())
            .unwrap_or_default()
    }

    pub(crate) fn toggle_agent_custom_risk_ack(
        &mut self,
        role: hvc_core::llm::AgentRole,
        cx: &mut Context<Self>,
    ) {
        let Some(mut config) = self.current_agent_config(role, cx) else {
            return;
        };
        config.custom_risk_ack = !config.custom_risk_ack;
        self.save_agent_config(config, cx);
    }

    pub(crate) fn save_agent_text_config(
        &mut self,
        role: hvc_core::llm::AgentRole,
        cx: &mut Context<Self>,
    ) {
        let Some(mut config) = self.current_agent_config(role, cx) else {
            return;
        };
        if config.backend != CliBackend::Custom {
            return;
        }
        config.model = self
            .agent_model_inputs
            .get(&role)
            .map(|input| input.read(cx).value().trim().to_string())
            .filter(|value| !value.is_empty());
        config.model_source = if config.model.is_some() {
            CliModelSelectionSource::Manual
        } else {
            CliModelSelectionSource::Default
        };
        config.model_profile = None;
        config.thinking_level = ThinkingLevel::Default;
        config.custom_executable = self
            .agent_custom_executable_inputs
            .get(&role)
            .map(|input| input.read(cx).value().trim().to_string())
            .filter(|value| !value.is_empty());
        let raw_args = self
            .agent_custom_args_inputs
            .get(&role)
            .map(|input| input.read(cx).value().trim().to_string())
            .unwrap_or_default();
        config.custom_args = if raw_args.is_empty() {
            Vec::new()
        } else {
            match serde_json::from_str::<Vec<String>>(&raw_args) {
                Ok(args) => args,
                Err(error) => {
                    self.agents.update(cx, |agents, _| {
                        agents.error = Some(crate::errors::UiCommandError::from_chat_parts(
                            "invalid_custom_args",
                            format!(
                                "Argumentos Custom devem ser um array JSON de strings: {error}"
                            ),
                        ));
                    });
                    cx.notify();
                    return;
                }
            }
        };
        self.save_agent_config(config, cx);
    }

    pub(crate) fn start_manual_agent(
        &mut self,
        role: hvc_core::llm::AgentRole,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.agents_backend.clone() else {
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
        let Some(input) = self.agent_prompt_inputs.get(&role).cloned() else {
            return;
        };
        let prompt = input.read(cx).value().to_string();
        if prompt.trim().is_empty() {
            return;
        }
        let generation = self.agents.update(cx, |agents, _| agents.begin(false));
        cx.notify();
        self.agents_task = Some(cx.spawn(async move |this, cx| {
            let result = backend
                .start_manual(StartManualRunRequest {
                    project_id,
                    agent_role: role,
                    prompt,
                })
                .await;
            let _ = this.update(cx, |model, cx| {
                match result {
                    Ok(run) => {
                        input.update(cx, |input, cx| input.set_value("", cx));
                        model.agents.update(cx, |agents, _| {
                            agents.apply_started_run(generation, run);
                        });
                        model.schedule_agents_notice_dismiss(cx);
                    }
                    Err(error) => {
                        let message = error.presentable();
                        if model
                            .agents
                            .update(cx, |agents, _| agents.fail(generation, error))
                        {
                            model.push_header_notice(HeaderNoticeKind::Error, message, cx);
                        }
                    }
                }
                cx.notify();
            });
        }));
    }

    pub(crate) fn current_free_form_slot(
        &self,
        slot_id: &str,
        cx: &gpui::App,
    ) -> Option<TerminalSlot> {
        let snapshot = self.agents.read(cx).snapshot.as_ref()?;
        snapshot
            .free_form_slots
            .iter()
            .chain(snapshot.archived_free_form_slots.iter())
            .find(|item| item.slot.id == slot_id)
            .map(|item| item.slot.clone())
    }

    fn run_free_form_operation(
        &mut self,
        operation: crate::agents::AgentsFuture<String>,
        cx: &mut Context<Self>,
    ) {
        let generation = self.agents.update(cx, |agents, _| agents.begin(false));
        cx.notify();
        self.agents_task = Some(cx.spawn(async move |this, cx| {
            let result = operation.await;
            let _ = this.update(cx, |model, cx| match result {
                Ok(message) => {
                    if model.agents.read(cx).generation == generation {
                        model.agents.update(cx, |agents, _| {
                            agents.busy = false;
                            agents.notice = Some(message);
                            agents.pending_free_form_history_clear = None;
                            agents.pending_free_form_delete = None;
                        });
                        model.refresh_agents(cx);
                        model.schedule_agents_notice_dismiss(cx);
                    }
                }
                Err(error) => {
                    let message = error.presentable();
                    if model
                        .agents
                        .update(cx, |agents, _| agents.fail(generation, error))
                    {
                        model.push_header_notice(HeaderNoticeKind::Error, message, cx);
                    }
                    cx.notify();
                }
            });
        }));
    }

    pub(crate) fn create_free_form_agent(&mut self, cx: &mut Context<Self>) {
        let Some(backend) = self.agents_backend.clone() else {
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
        let ordinal = self
            .agents
            .read(cx)
            .snapshot
            .as_ref()
            .map_or(1, |snapshot| snapshot.free_form_slots.len() + 1);
        self.run_free_form_operation(
            Box::pin(async move {
                backend
                    .create_free_form(CreateFreeFormSlotRequest {
                        project_id,
                        label: format!("Agente livre {ordinal}"),
                    })
                    .await?;
                Ok("Agente livre criado.".into())
            }),
            cx,
        );
    }

    pub(crate) fn save_free_form_slot(&mut self, slot: TerminalSlot, cx: &mut Context<Self>) {
        let Some(backend) = self.agents_backend.clone() else {
            return;
        };
        self.run_free_form_operation(
            Box::pin(async move {
                backend
                    .update_free_form(UpdateFreeFormSlotRequest {
                        id: slot.id,
                        project_id: slot.project_id,
                        label: slot.label,
                        execution_mode: slot.execution_mode,
                        backend: slot.backend,
                        model: slot.model,
                        model_profile: slot.model_profile,
                        model_source: slot.model_source,
                        thinking_level: slot.thinking_level,
                        session_policy: slot.session_policy,
                        custom_executable: slot.custom_executable,
                        custom_args: slot.custom_args,
                        custom_risk_ack: slot.custom_risk_ack,
                        position: slot.position,
                        archived: slot.archived,
                        expected_version: slot.version,
                    })
                    .await?;
                Ok("Free-form agent configuration updated.".into())
            }),
            cx,
        );
    }

    pub(crate) fn toggle_free_form_risk(&mut self, slot_id: String, cx: &mut Context<Self>) {
        let Some(mut slot) = self.current_free_form_slot(&slot_id, cx) else {
            return;
        };
        slot.custom_risk_ack = !slot.custom_risk_ack;
        self.save_free_form_slot(slot, cx);
    }

    pub(crate) fn save_free_form_text_config(&mut self, slot_id: String, cx: &mut Context<Self>) {
        let Some(mut slot) = self.current_free_form_slot(&slot_id, cx) else {
            return;
        };
        slot.label = self
            .free_form_label_inputs
            .get(&slot_id)
            .map(|input| input.read(cx).value().trim().to_string())
            .unwrap_or_default();
        if slot.backend == CliBackend::Custom {
            slot.model = self
                .free_form_model_inputs
                .get(&slot_id)
                .map(|input| input.read(cx).value().trim().to_string())
                .filter(|value| !value.is_empty());
            slot.model_source = if slot.model.is_some() {
                CliModelSelectionSource::Manual
            } else {
                CliModelSelectionSource::Default
            };
            slot.model_profile = None;
            slot.thinking_level = ThinkingLevel::Default;
            slot.custom_executable = self
                .free_form_custom_executable_inputs
                .get(&slot_id)
                .map(|input| input.read(cx).value().trim().to_string())
                .filter(|value| !value.is_empty());
            let raw_args = self
                .free_form_custom_args_inputs
                .get(&slot_id)
                .map(|input| input.read(cx).value().trim().to_string())
                .unwrap_or_default();
            slot.custom_args = if raw_args.is_empty() {
                Vec::new()
            } else {
                match serde_json::from_str::<Vec<String>>(&raw_args) {
                    Ok(args) => args,
                    Err(error) => {
                        self.agents.update(cx, |agents, _| {
                            agents.error = Some(crate::errors::UiCommandError::from_chat_parts(
                                "invalid_custom_args",
                                format!(
                                    "Argumentos Custom devem ser um array JSON de strings: {error}"
                                ),
                            ));
                        });
                        cx.notify();
                        return;
                    }
                }
            };
        }
        self.save_free_form_slot(slot, cx);
    }

    pub(crate) fn start_free_form_agent(&mut self, slot_id: String, cx: &mut Context<Self>) {
        let Some(backend) = self.agents_backend.clone() else {
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
        let Some(input) = self.free_form_prompt_inputs.get(&slot_id).cloned() else {
            return;
        };
        let prompt = input.read(cx).value().to_string();
        if prompt.trim().is_empty() {
            return;
        }
        let generation = self.agents.update(cx, |agents, _| agents.begin(false));
        cx.notify();
        self.agents_task = Some(cx.spawn(async move |this, cx| {
            let result = backend
                .start_free_form(StartFreeFormRunRequest {
                    project_id,
                    slot_id,
                    prompt,
                })
                .await;
            let _ = this.update(cx, |model, cx| {
                match result {
                    Ok(run) => {
                        input.update(cx, |input, cx| input.set_value("", cx));
                        model.agents.update(cx, |agents, _| {
                            agents.apply_started_run(generation, run);
                        });
                    }
                    Err(error) => {
                        let message = error.presentable();
                        if model
                            .agents
                            .update(cx, |agents, _| agents.fail(generation, error))
                        {
                            model.push_header_notice(HeaderNoticeKind::Error, message, cx);
                        }
                    }
                }
                cx.notify();
            });
        }));
    }

    pub(crate) fn resume_agent_run(
        &mut self,
        source_run_id: String,
        slot_id: String,
        role: Option<hvc_core::llm::AgentRole>,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.agents_backend.clone() else {
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
        let input = role
            .and_then(|role| self.agent_prompt_inputs.get(&role).cloned())
            .or_else(|| self.free_form_prompt_inputs.get(&slot_id).cloned());
        let Some(input) = input else {
            return;
        };
        let prompt = input.read(cx).value().to_string();
        if prompt.trim().is_empty() {
            return;
        }
        let generation = self.agents.update(cx, |agents, _| agents.begin(false));
        cx.notify();
        self.agents_task = Some(cx.spawn(async move |this, cx| {
            let result = backend
                .resume_manual(ResumeManualRunRequest {
                    project_id,
                    source_run_id,
                    prompt,
                })
                .await;
            let _ = this.update(cx, |model, cx| {
                match result {
                    Ok(run) => {
                        input.update(cx, |input, cx| input.set_value("", cx));
                        model.agents.update(cx, |agents, _| {
                            agents.apply_started_run(generation, run);
                        });
                        model.schedule_agents_notice_dismiss(cx);
                    }
                    Err(error) => {
                        let message = error.presentable();
                        if model
                            .agents
                            .update(cx, |agents, _| agents.fail(generation, error))
                        {
                            model.push_header_notice(HeaderNoticeKind::Error, message, cx);
                        }
                    }
                }
                cx.notify();
            });
        }));
    }

    pub(crate) fn request_delete_free_form_agent(
        &mut self,
        slot_id: String,
        cx: &mut Context<Self>,
    ) {
        let confirmed = self.agents.update(cx, |agents, _| {
            if agents.pending_free_form_delete.as_deref() == Some(slot_id.as_str()) {
                agents.pending_free_form_delete = None;
                true
            } else {
                agents.pending_free_form_delete = Some(slot_id.clone());
                agents.notice = Some(
                    "Confirm again: the local history of this free-form agent will be removed; the provider session will not be deleted."
                        .into(),
                );
                false
            }
        });
        if !confirmed {
            cx.notify();
            return;
        }
        let Some(slot) = self.current_free_form_slot(&slot_id, cx) else {
            return;
        };
        let Some(backend) = self.agents_backend.clone() else {
            return;
        };
        self.run_free_form_operation(
            Box::pin(async move {
                backend
                    .delete_free_form(slot.project_id, slot.id, slot.version)
                    .await?;
                Ok("Free-form agent and local history deleted.".into())
            }),
            cx,
        );
    }

    pub(crate) fn archive_free_form_agent(&mut self, slot_id: String, cx: &mut Context<Self>) {
        let Some(mut slot) = self.current_free_form_slot(&slot_id, cx) else {
            return;
        };
        slot.archived = true;
        self.save_free_form_slot(slot, cx);
    }

    pub(crate) fn restore_free_form_agent(&mut self, slot_id: String, cx: &mut Context<Self>) {
        let Some(mut slot) = self.current_free_form_slot(&slot_id, cx) else {
            return;
        };
        slot.archived = false;
        self.save_free_form_slot(slot, cx);
    }

    pub(crate) fn request_clear_free_form_history(
        &mut self,
        slot_id: String,
        cx: &mut Context<Self>,
    ) {
        let confirmed = self.agents.update(cx, |agents, _| {
            if agents.pending_free_form_history_clear.as_deref() == Some(slot_id.as_str()) {
                agents.pending_free_form_history_clear = None;
                true
            } else {
                agents.pending_free_form_history_clear = Some(slot_id.clone());
                agents.notice = Some(
                    "Confirm again to permanently erase local history of this free-form agent."
                        .into(),
                );
                false
            }
        });
        if !confirmed {
            cx.notify();
            return;
        }
        let Some(slot) = self.current_free_form_slot(&slot_id, cx) else {
            return;
        };
        let Some(backend) = self.agents_backend.clone() else {
            return;
        };
        self.run_free_form_operation(
            Box::pin(async move {
                let count = backend.clear_slot_history(slot.project_id, slot.id).await?;
                Ok(format!("History removed: {count} execution(s)."))
            }),
            cx,
        );
    }

    pub(crate) fn move_free_form_agent(
        &mut self,
        slot_id: String,
        direction: isize,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.agents_backend.clone() else {
            return;
        };
        let Some((project_id, mut ids)) = self.agents.read(cx).snapshot.as_ref().map(|snapshot| {
            (
                snapshot.project_id.clone(),
                snapshot
                    .free_form_slots
                    .iter()
                    .map(|item| item.slot.id.clone())
                    .collect::<Vec<_>>(),
            )
        }) else {
            return;
        };
        let Some(index) = ids.iter().position(|id| id == &slot_id) else {
            return;
        };
        let target = index
            .saturating_add_signed(direction)
            .min(ids.len().saturating_sub(1));
        if target == index {
            return;
        }
        ids.swap(index, target);
        self.run_free_form_operation(
            Box::pin(async move {
                backend.reorder_free_form(project_id, ids).await?;
                Ok("Free-form agent order updated.".into())
            }),
            cx,
        );
    }

    pub(crate) fn request_cancel_agent_run(
        &mut self,
        run_id: String,
        pulsation_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if let Some(pulsation_id) = pulsation_id {
            let confirmed = self.agents.update(cx, |agents, _| {
                if agents.pending_pipeline_cancel.as_deref() == Some(pulsation_id.as_str()) {
                    agents.pending_pipeline_cancel = None;
                    true
                } else {
                    agents.pending_pipeline_cancel = Some(pulsation_id.clone());
                    agents.notice = Some(
                        "Automatic stop cancels the entire pulse. Click again to confirm.".into(),
                    );
                    false
                }
            });
            if confirmed {
                self.cancel_agent_pipeline(pulsation_id, cx);
            } else {
                cx.notify();
            }
            return;
        }

        self.cancel_agent_run(run_id, cx);
    }

    fn cancel_agent_pipeline(&mut self, pulsation_id: String, cx: &mut Context<Self>) {
        let Some(backend) = self.pulsations_backend.clone() else {
            return;
        };
        let generation = self.agents.update(cx, |agents, _| agents.begin(false));
        self.agents_task = Some(cx.spawn(async move |this, cx| {
            let result = backend.cancel_jobs(pulsation_id).await;
            let _ = this.update(cx, |model, cx| {
                match result {
                    Ok(cancelled) => {
                        model.agents.update(cx, |agents, _| {
                            if generation == agents.generation {
                                agents.busy = false;
                                agents.notice = Some(format!(
                                    "Pulse canceled; {cancelled} job(s) were interrupted."
                                ));
                            }
                        });
                        model.refresh_pulsations(cx);
                        model.refresh_agents(cx);
                        model.schedule_agents_notice_dismiss(cx);
                    }
                    Err(error) => {
                        let error = crate::errors::UiCommandError::from_chat_parts(
                            "cancel_pulsation",
                            error,
                        );
                        let message = error.presentable();
                        if model
                            .agents
                            .update(cx, |agents, _| agents.fail(generation, error))
                        {
                            model.push_header_notice(HeaderNoticeKind::Error, message, cx);
                        }
                    }
                }
                cx.notify();
            });
        }));
    }

    fn cancel_agent_run(&mut self, run_id: String, cx: &mut Context<Self>) {
        let Some(backend) = self.agents_backend.clone() else {
            return;
        };
        let generation = self.agents.update(cx, |agents, _| agents.begin(false));
        self.agents_task = Some(cx.spawn(async move |this, cx| {
            let result = backend.cancel(run_id).await;
            let _ = this.update(cx, |model, cx| {
                match result {
                    Ok(true) => {
                        model.agents.update(cx, |agents, _| {
                            if generation == agents.generation {
                                agents.busy = false;
                                agents.notice = Some("Cancellation requested.".into());
                            }
                        });
                        model.schedule_agents_notice_dismiss(cx);
                    }
                    Ok(false) => {
                        let error = crate::errors::UiCommandError::from_chat_parts(
                            "not_running",
                            "The run is no longer active.",
                        );
                        model.agents.update(cx, |agents, _| {
                            agents.fail(generation, error);
                        });
                    }
                    Err(error) => {
                        model.agents.update(cx, |agents, _| {
                            agents.fail(generation, error);
                        });
                    }
                }
                cx.notify();
            });
        }));
    }

    pub(crate) fn select_agent_history(
        &mut self,
        role: hvc_core::llm::AgentRole,
        direction: i32,
        cx: &mut Context<Self>,
    ) {
        let target = {
            let agents = self.agents.read(cx);
            let Some(snapshot) = agents.snapshot.as_ref() else {
                return;
            };
            let Some(agent) = snapshot
                .agents
                .iter()
                .find(|agent| agent.config.agent_role == role)
            else {
                return;
            };
            let current = agents
                .selected_run_by_role
                .get(&role)
                .and_then(|id| agent.recent_runs.iter().position(|run| &run.id == id))
                .unwrap_or(0);
            let target_index = if direction > 0 {
                current.saturating_add(1)
            } else {
                current.saturating_sub(1)
            };
            agent
                .recent_runs
                .get(target_index)
                .map(|run| run.id.clone())
        };
        let Some(run_id) = target else {
            return;
        };
        if self.agents.read(cx).events_by_run.contains_key(&run_id) {
            self.agents.update(cx, |agents, _| {
                agents.selected_run_by_role.insert(role, run_id);
            });
            cx.notify();
            return;
        }
        let Some(backend) = self.agents_backend.clone() else {
            return;
        };
        let generation = self.agents.update(cx, |agents, _| agents.begin(false));
        cx.notify();
        self.agents_task = Some(cx.spawn(async move |this, cx| {
            let result = backend.events(run_id.clone(), 0, 500).await;
            let _ = this.update(cx, |model, cx| {
                match result {
                    Ok(events) => {
                        model.agents.update(cx, |agents, _| {
                            agents.apply_run_events(generation, role, run_id, events);
                        });
                    }
                    Err(error) => {
                        model.agents.update(cx, |agents, _| {
                            agents.fail(generation, error);
                        });
                    }
                }
                cx.notify();
            });
        }));
    }

    pub(crate) fn select_free_form_history(
        &mut self,
        slot_id: String,
        direction: i32,
        cx: &mut Context<Self>,
    ) {
        let target = {
            let agents = self.agents.read(cx);
            let Some(snapshot) = agents.snapshot.as_ref() else {
                return;
            };
            let Some(slot) = snapshot
                .free_form_slots
                .iter()
                .find(|item| item.slot.id == slot_id)
            else {
                return;
            };
            let current = agents
                .selected_run_by_slot
                .get(&slot_id)
                .and_then(|id| slot.recent_runs.iter().position(|run| &run.id == id))
                .unwrap_or(0);
            let target_index = if direction > 0 {
                current.saturating_add(1)
            } else {
                current.saturating_sub(1)
            };
            slot.recent_runs.get(target_index).map(|run| run.id.clone())
        };
        let Some(run_id) = target else {
            return;
        };
        if self.agents.read(cx).events_by_run.contains_key(&run_id) {
            self.agents.update(cx, |agents, _| {
                agents.selected_run_by_slot.insert(slot_id, run_id);
            });
            cx.notify();
            return;
        }
        let Some(backend) = self.agents_backend.clone() else {
            return;
        };
        let generation = self.agents.update(cx, |agents, _| agents.begin(false));
        cx.notify();
        self.agents_task = Some(cx.spawn(async move |this, cx| {
            let result = backend.events(run_id.clone(), 0, 500).await;
            let _ = this.update(cx, |model, cx| {
                match result {
                    Ok(events) => {
                        model.agents.update(cx, |agents, _| {
                            agents.apply_slot_run_events(generation, slot_id, run_id, events);
                        });
                    }
                    Err(error) => {
                        model.agents.update(cx, |agents, _| {
                            agents.fail(generation, error);
                        });
                    }
                }
                cx.notify();
            });
        }));
    }

    pub(crate) fn request_clear_agent_history(
        &mut self,
        role: hvc_core::llm::AgentRole,
        cx: &mut Context<Self>,
    ) {
        let confirmed = self.agents.update(cx, |agents, _| {
            if agents.pending_history_clear == Some(role) {
                agents.pending_history_clear = None;
                true
            } else {
                agents.pending_history_clear = Some(role);
                agents.notice =
                    Some("Confirm again to permanently erase this agent's history.".to_string());
                false
            }
        });
        if !confirmed {
            cx.notify();
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
        let Some(backend) = self.agents_backend.clone() else {
            return;
        };
        let generation = self.agents.update(cx, |agents, _| agents.begin(false));
        cx.notify();
        self.agents_task = Some(cx.spawn(async move |this, cx| {
            let result = backend.clear_history(project_id, Some(role)).await;
            let _ = this.update(cx, |model, cx| {
                match result {
                    Ok(count) => {
                        model.agents.update(cx, |agents, _| {
                            if agents.generation == generation {
                                agents.busy = false;
                                agents.notice =
                                    Some(format!("History removed: {count} execution(s)."));
                            }
                        });
                        model.refresh_agents(cx);
                        model.schedule_agents_notice_dismiss(cx);
                    }
                    Err(error) => {
                        model.agents.update(cx, |agents, _| {
                            agents.fail(generation, error);
                        });
                    }
                }
                cx.notify();
            });
        }));
    }

    pub(crate) fn resolve_agent_cli_fallback(
        &mut self,
        checkpoint_id: String,
        expected_state_version: u64,
        decision: CliFallbackDecision,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.agents_backend.clone() else {
            return;
        };
        let generation = self.agents.update(cx, |agents, _| agents.begin(false));
        cx.notify();
        self.agents_task = Some(cx.spawn(async move |this, cx| {
            let result = backend
                .resolve_fallback(ResolveCliFallbackRequest {
                    checkpoint_id,
                    expected_state_version,
                    decision,
                })
                .await;
            let _ = this.update(cx, |model, cx| {
                match result {
                    Ok(_) => {
                        model.agents.update(cx, |agents, _| {
                            if agents.generation == generation {
                                agents.busy = false;
                                agents.notice = Some(
                                    match decision {
                                        CliFallbackDecision::UseApi => {
                                            "API fallback authorized for this pulsation."
                                        }
                                        CliFallbackDecision::RetryCli => "CLI retry scheduled.",
                                        CliFallbackDecision::Cancel => "Pulsation canceled.",
                                    }
                                    .to_string(),
                                );
                            }
                        });
                        model.refresh_agents(cx);
                        model.refresh_pulsations(cx);
                        model.schedule_agents_notice_dismiss(cx);
                    }
                    Err(error) => {
                        let message = error.presentable();
                        if model
                            .agents
                            .update(cx, |agents, _| agents.fail(generation, error))
                        {
                            model.push_header_notice(HeaderNoticeKind::Error, message, cx);
                        }
                    }
                }
                cx.notify();
            });
        }));
    }

    pub(crate) fn request_cancel_agent_cli_fallback(
        &mut self,
        checkpoint_id: String,
        expected_state_version: u64,
        cx: &mut Context<Self>,
    ) {
        let confirmed = self.agents.update(cx, |agents, _| {
            if agents.pending_fallback_cancel.as_deref() == Some(checkpoint_id.as_str()) {
                agents.pending_fallback_cancel = None;
                true
            } else {
                agents.pending_fallback_cancel = Some(checkpoint_id.clone());
                agents.notice = Some(
                    "Confirm again to cancel the entire pulsation; this will not revert already published files."
                        .to_string(),
                );
                false
            }
        });
        if confirmed {
            self.resolve_agent_cli_fallback(
                checkpoint_id,
                expected_state_version,
                CliFallbackDecision::Cancel,
                cx,
            );
        } else {
            cx.notify();
        }
    }

    pub(crate) fn refresh_settings(&mut self, cx: &mut Context<Self>) {
        if self.settings.read(cx).settings_section == crate::settings::SettingsSection::Usage {
            self.refresh_usage(cx);
        }
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
                        let previous_connection =
                            model.settings.read(cx).selected_connection_id.clone();
                        let previous_route = model.settings.read(cx).selected_route_id.clone();
                        model.fill_preparation_threshold(&snapshot, cx);
                        let applied = model.settings.update(cx, |settings, _| {
                            settings.apply_snapshot(generation, snapshot)
                        });
                        if applied {
                            let connection = model.settings.read(cx).selected_connection_id.clone();
                            let route = model.settings.read(cx).selected_route_id.clone();
                            if connection != previous_connection
                                && let Some(id) = connection
                            {
                                model.select_settings_connection(id.to_string(), cx);
                            }
                            if route != previous_route
                                && let Some(id) = route
                            {
                                model.select_settings_route(id.to_string(), cx);
                            }
                        }
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
                        if applied && model.settings.read(cx).embedding_readiness.is_none() {
                            model.refresh_embedding_readiness(cx);
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

    pub(crate) fn run_settings_operation(
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
                            model.push_header_notice(HeaderNoticeKind::Error, message, cx);
                        }
                        cx.notify();
                    }
                }
            });
        }));
    }

    pub(crate) fn update_runtime_config(
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

    pub(crate) fn set_routing_mode(&mut self, mode: RoutingMode, cx: &mut Context<Self>) {
        self.update_runtime_config(
            runtime_config_update().with_routing_mode(mode),
            "Modo de roteamento atualizado.",
            cx,
        );
        self.close_dropdown(cx);
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
            "Context strategy updated.",
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
            "Embeddings configuration updated.",
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
            self.push_header_notice(
                HeaderNoticeKind::Warning,
                "Open a workspace before reindexing embeddings.",
                cx,
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
                Ok(format!("Embedding reconciliation requested for {}. Supervisor will run deltas in the background.", response.workspace_path))
            }),
            cx,
        );
    }

    /// Runs the 3-step Ollama readiness check independently of `run_settings_operation`
    /// (which is for a single mutation → one notice): this is a silent diagnostic that can run
    /// alongside other settings actions and is re-triggered automatically whenever indexing
    /// reports a failure (see `apply_indexing_event`), so the checklist stays current without
    /// the developer having to ask for it.
    pub(crate) fn refresh_embedding_readiness(&mut self, cx: &mut Context<Self>) {
        let Some(backend) = self.settings_backend.clone() else {
            return;
        };
        self.settings.update(cx, |settings, _| {
            settings.embedding_readiness_loading = true;
        });
        cx.notify();
        self.embedding_readiness_task = Some(cx.spawn(async move |this, cx| {
            let result = backend.check_embedding_readiness().await;
            let _ = this.update(cx, |model, cx| {
                model.settings.update(cx, |settings, _| {
                    settings.embedding_readiness_loading = false;
                    if let Ok(report) = result {
                        settings.embedding_readiness = Some(report);
                    }
                });
                cx.notify();
            });
        }));
    }

    /// Downloads the model named in the current readiness report (the one the checklist just
    /// flagged as missing) via Ollama's own pull API — no terminal required. Progress streams
    /// in through `model_pull_status`; completion re-checks readiness automatically.
    pub(crate) fn pull_embedding_model(&mut self, cx: &mut Context<Self>) {
        let Some(backend) = self.settings_backend.clone() else {
            return;
        };
        let model = self
            .settings
            .read(cx)
            .embedding_readiness
            .as_ref()
            .map(|report| report.model.clone());
        self.model_pull_task = Some(cx.spawn(async move |this, cx| {
            let result = backend.pull_embedding_model(model).await;
            let _ = this.update(cx, |model, cx| {
                if let Err(error) = result {
                    model.push_header_notice(HeaderNoticeKind::Error, error.presentable(), cx);
                }
                model.refresh_embedding_readiness(cx);
            });
        }));
    }

    pub(crate) fn update_dynamic_context_policy(
        &mut self,
        update: impl FnOnce(&mut DynamicContextPolicy),
        cx: &mut Context<Self>,
    ) {
        let mut policy = self
            .settings
            .read(cx)
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.config.dynamic_context_policy.clone())
            .unwrap_or_default();
        update(&mut policy);
        self.update_runtime_config(
            runtime_config_update().with_dynamic_context_policy(policy.sanitized()),
            "Adaptive policy updated.",
            cx,
        );
    }

    pub(crate) fn select_settings_provider(&mut self, provider_id: String, cx: &mut Context<Self>) {
        self.settings.update(cx, |settings, _| {
            settings.connection_provider_id = provider_id.into();
        });
        self.close_dropdown(cx);
        cx.notify();
    }

    pub(crate) fn select_settings_connection(
        &mut self,
        connection_id: String,
        cx: &mut Context<Self>,
    ) {
        let connection = self.settings.update(cx, |settings, _| {
            settings.selected_connection_id = Some(connection_id.into());
            settings.creating_connection = false;
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
            settings.creating_connection = true;
            settings.settings_section = crate::settings::SettingsSection::Connections;
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
                "LLM Connection".to_string()
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
                Ok("Connection and credential securely saved.".to_string())
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
                    "Connection {} in {} ms: {}",
                    if response.reachable {
                        "reachable"
                    } else {
                        "unavailable"
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
                Ok("Connection deleted.".to_string())
            }),
            cx,
        );
    }

    pub(crate) fn select_settings_route(&mut self, route_id: String, cx: &mut Context<Self>) {
        let route = self.settings.update(cx, |settings, _| {
            settings.selected_route_id = Some(route_id.into());
            settings.creating_route = false;
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
            settings.creating_route = true;
            settings.settings_section = crate::settings::SettingsSection::Routes;
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

    pub(crate) fn set_route_strategy(&mut self, strategy: String, cx: &mut Context<Self>) {
        self.settings.update(cx, |settings, _| {
            settings.route_strategy = strategy.into();
        });
        self.close_dropdown(cx);
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
                "LLM Route".into()
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
                Ok("Route saved with current target order.".to_string())
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
                Ok("Route deleted.".to_string())
            }),
            cx,
        );
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
            self.push_header_notice(
                HeaderNoticeKind::Warning,
                "Open a project before creating the binding.",
                cx,
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
                Ok("Exact binding saved.".to_string())
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
                Ok(format!("{count} resilience state(s) removed."))
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
                            model.status = "LLM execution resynchronized".into();
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
                                model.refresh_forge(cx);
                                if model.active_view == AppView::Agents {
                                    model.refresh_agents(cx);
                                }
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
                            Err(error) => model.push_header_notice(
                                HeaderNoticeKind::Warning,
                                format!("Failed to update runtime: {error}"),
                                cx,
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
        self.status = "Restoring persisted session…".into();
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
                self.status = "Ready — select a workspace".into();
            }
            Ok(Err(error)) => {
                self.hydrating = false;
                self.status = format!("Failed to restore session: {error}").into();
                self.push_header_notice(HeaderNoticeKind::Error, error, cx);
            }
            Err(error) => {
                self.hydrating = false;
                self.status = format!("Restore worker failed: {error}").into();
                self.push_header_notice(
                    HeaderNoticeKind::Error,
                    format!("Restore worker interrupted: {error}"),
                    cx,
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
        self.auto_open_review = session.is_some_and(|value| value.auto_open_review);
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

        // A restored project starts a new in-memory navigation history.
        self.active_tab = AppTab::from_view(active_view).unwrap_or(AppTab::Companion);
        self.last_view_per_tab = AppTab::ALL
            .into_iter()
            .map(|tab| {
                (
                    tab,
                    if tab.contains(active_view) {
                        active_view
                    } else {
                        tab.default_view()
                    },
                )
            })
            .collect();
        self.navigation.update(cx, |navigation, _| {
            navigation.active = active_view;
            navigation.active_tab = self.active_tab;
            navigation.sidebar_collapsed = session.is_some_and(|value| value.sidebar_collapsed);
        });
        self.indexing_status = None;
        self.workspace.update(cx, |workspace, cx| {
            workspace.project_id = Some(restored.project_id.clone().into());
            workspace.root_path = Some(restored.root_path.clone().into());
            workspace.root_path_canonical = std::path::Path::new(&restored.root_path)
                .canonicalize()
                .ok()
                .map(|path| path.to_string_lossy().into_owned());
            workspace.tree = restored.tree.clone();
            workspace.active_relative_path = active_path.clone().map(Into::into);
            workspace.tabs = tabs;
            workspace.context_drawer_open = session.is_none_or(|value| value.context_drawer_open);
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
                            .expect("default selection must be valid");
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
        if let Some(tab) = AppTab::from_view(active_view) {
            self.active_tab = tab;
            self.last_view_per_tab.insert(tab, active_view);
        }
        self.needs_resync = false;
        self.session_dirty = false;
        self.status = format!("Session restored: {}", restored.root_path).into();
        if let Some(notice) = restored.notice {
            self.push_header_notice(HeaderNoticeKind::Notice, notice, cx);
        }
        self.hydrating = false;
        self.refresh_pulsations_with_preferred(preferred_pulsation_id, cx);
        self.refresh_vault(true, cx);
        match active_view {
            AppView::Agents => self.refresh_agents(cx),
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
                self.status = "Session saved".into();
            }
            Err(error) => {
                self.session_dirty = true;
                self.push_header_notice(
                    HeaderNoticeKind::Warning,
                    format!("Persistence: {error}"),
                    cx,
                );
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
            // The Workspace view is editor-only; HITL review now lives in Pulsations' Review
            // section (see the UI flow plan Part 1.2). The field is kept for the persisted-session
            // contract and older rows still written with "hitl".
            workspace_view_mode: "editor".to_string(),
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
            auto_open_review: self.auto_open_review,
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
        self.hitl_documents_task.take();
        self.vault_refresh_task.take();
        self.vault_search_task.take();
        self.vault_rename_task.take();
        self.vault_context_task.take();
        self.vault_document_tasks.clear();
        let settings = self.settings_task.take();
        self.agents_task.take();
        self.agents_notice_timer.take();
        self.model_catalog_task.take();
        self.logs_task.take();
        self.logs_resync_tasks.clear();
        self.chat_task.take();
        self.chat_resync_tasks.clear();
        self.session_task.take();
        self.performance_probe.take();
        self.notifications.wake.take();
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
                        Err(error) => model.push_header_notice(
                            HeaderNoticeKind::Warning,
                            format!("Persistence on shutdown: {error}"),
                            cx,
                        ),
                    }
                    cx.notify();
                });
            }
        })
    }

    fn handle_event(&mut self, event: AppEvent, cx: &mut Context<Self>) {
        // Indexing progress ticks at supervisor cadence and must not steal the status bar's
        // main text (see the generic "Event received: ..." below) on every delta.
        if let AppEvent::Indexing(event) = event {
            self.apply_indexing_event(event, cx);
            return;
        }
        if let AppEvent::ModelPull(event) = event {
            self.model_pull_status = Some(event);
            cx.notify();
            return;
        }
        let usage_changed = match &event {
            AppEvent::Llm(envelope) => matches!(
                envelope.event,
                hvc_core::llm::LlmStreamEvent::RouteCompleted { .. }
                    | hvc_core::llm::LlmStreamEvent::RouteFailed { .. }
                    | hvc_core::llm::LlmStreamEvent::AttemptFailed { .. }
            ),
            AppEvent::Chat(envelope) => matches!(
                envelope.kind,
                hvc_core::chat_service::ChatEventKind::TurnCompleted
                    | hvc_core::chat_service::ChatEventKind::TurnFailed
                    | hvc_core::chat_service::ChatEventKind::TurnCancelled
            ),
            AppEvent::Terminal(envelope) => envelope.run.status.is_terminal(),
            _ => false,
        };
        if usage_changed {
            self.schedule_usage_refresh(cx);
        }
        self.event_count = self.event_count.saturating_add(1);
        self.status = format!("Event received: {}", event.event_name()).into();
        match event {
            AppEvent::Forge(event) => {
                if self
                    .workspace
                    .read(cx)
                    .project_id
                    .as_ref()
                    .is_some_and(|p| p.as_ref() == event.project_id)
                {
                    self.refresh_forge(cx);
                }
            }
            AppEvent::WorkspaceChanged(event) => {
                self.logs.update(cx, |logs, _| {
                    logs.record_workspace(event.clone());
                });
                self.handle_workspace_change(event, cx);
            }
            AppEvent::Llm(envelope) => {
                let execution_id = envelope.execution_id.clone();
                self.agents
                    .update(cx, |agents, _| agents.record_llm(&envelope));
                let classification = self.logs.update(cx, |logs, _| logs.record_llm(envelope));
                if classification == SequenceClassification::Gap {
                    self.needs_resync = true;
                    self.status = "LLM gap detected; reloading persisted execution".into();
                    self.resync_llm_execution(execution_id, cx);
                }
            }
            AppEvent::Terminal(envelope) => {
                let terminal = envelope.run.status.is_terminal()
                    || envelope.run.status == TerminalRunStatus::AwaitingFallback;
                let gap = self
                    .agents
                    .update(cx, |agents, _| agents.record_event(*envelope));
                if gap {
                    self.needs_resync = true;
                    self.status = "Terminal gap detected; reloading persisted history".into();
                }
                if (terminal || gap) && self.active_view == AppView::Agents {
                    self.refresh_agents(cx);
                }
            }
            AppEvent::Chat(envelope) => self.handle_chat_event(envelope, cx),
            // Returned early above, before the generic event-count/status preamble.
            AppEvent::Indexing(_) => unreachable!(),
            AppEvent::ModelPull(_) => unreachable!(),
        }
        cx.notify();
    }

    /// Applies a background embedding milestone, filtered to the workspace currently open in
    /// the UI (the supervisor iterates every registered project, not just the active one).
    fn apply_indexing_event(&mut self, event: IndexingEvent, cx: &mut Context<Self>) {
        let active = self.workspace.read(cx).root_path_canonical.clone();
        if active.as_deref() != Some(event.workspace_path()) {
            return;
        }
        // A real failure is exactly when the developer needs to know *why* — re-run the
        // readiness checklist so it points at the actual blocking step instead of a raw error.
        if matches!(event, IndexingEvent::Failed { .. }) {
            self.refresh_embedding_readiness(cx);
        }
        self.indexing_status = Some(event);
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

pub(crate) fn runtime_config_update() -> UpdateAgentRuntimeConfigRequest {
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
        adaptive_context_mode: None,
        dynamic_context_policy: None,
    }
}

pub(crate) trait RuntimeConfigUpdateExt {
    fn with_routing_mode(self, mode: RoutingMode) -> Self;
    fn with_context_strategy(self, strategy: ContextStrategy) -> Self;
    fn with_embedding_enabled(self, enabled: bool) -> Self;
    fn with_embedding_model(self, model: String) -> Self;
    fn with_embedding_top_k(self, top_k: usize) -> Self;
    fn with_adaptive_context_mode(self, mode: AdaptiveContextMode) -> Self;
    fn with_dynamic_context_policy(self, policy: DynamicContextPolicy) -> Self;
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

    fn with_adaptive_context_mode(mut self, mode: AdaptiveContextMode) -> Self {
        self.adaptive_context_mode = Some(mode);
        self
    }

    fn with_dynamic_context_policy(mut self, policy: DynamicContextPolicy) -> Self {
        self.dynamic_context_policy = Some(policy);
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
            "Existing workspace detected; choose whether to start gradual mapping.".to_string()
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
                "Could not load {}: {}",
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
                    message: "The file revision has changed on disk.".into(),
                    details: None,
                    remediation: Some("Reload the file.".into()),
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
                    failure_checkpoints: Vec::new(),
                    failure_waiting_count: 0,
                    background_agents_count: 1,
                    brownfield_coverage: None,
                })),
                artifacts: Arc::new(Mutex::new(vec![test_artifact("Plan")])),
            }
        }
    }

    impl PulsationsBackend for RecordingPulsationsBackend {
        fn review_documents(
            &self,
            _checkpoint: hvc_core::persistence_service::HitlCheckpoint,
        ) -> PulsationsFuture<Vec<hvc_core::persistence_service::HitlReviewDocument>> {
            Box::pin(async { Ok(Vec::new()) })
        }
        fn stage_failure_detail(
            &self,
            checkpoint_id: String,
        ) -> PulsationsFuture<hvc_core::persistence_service::StageFailureDetail> {
            Box::pin(async move {
                Ok(hvc_core::persistence_service::StageFailureDetail {
                    checkpoint_id,
                    raw_output: None,
                    validation_error: None,
                })
            })
        }
        fn action(&self, action: crate::pulsations::PulsationAction) -> PulsationsFuture<()> {
            self.actions.lock().unwrap().push(action);
            Box::pin(async { Ok(()) })
        }
        fn artifact_content(&self, _pulse: String, _artifact: String) -> PulsationsFuture<String> {
            Box::pin(async { Ok("Persisted fixture content".into()) })
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
                source_files: Vec::new(),
                test_files: Vec::new(),
            }],
            edges: Vec::new(),
            broken_links: Vec::new(),
            broken_file_references: Vec::new(),
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
    fn navigates_every_view(cx: &mut TestAppContext) {
        let model = cx.new(AppModel::new_for_test);
        for view in AppView::ALL {
            model.update(cx, |model, cx| model.select_view(view, cx));
            assert_eq!(cx.read(|cx| model.read(cx).active_view()), view);
            cx.read(|cx| {
                let model = model.read(cx);
                assert_eq!(model.navigation.read(cx).active, view);
                assert_eq!(model.navigation.read(cx).active_tab, model.active_tab());
                if let Some(tab) = AppTab::from_view(view) {
                    assert_eq!(model.active_tab(), tab);
                }
            });
        }
    }

    #[gpui::test]
    fn alt_number_jumps_to_the_nth_sub_view_of_the_active_tab(cx: &mut TestAppContext) {
        let model = cx.new(AppModel::new_for_test);
        model.update(cx, |model, cx| {
            model.select_tab(AppTab::Engine, cx);
            model.select_nth_sub_view(0, cx);
            assert_eq!(model.active_view(), AppTab::Engine.sub_views()[0]);
            model.select_nth_sub_view(1, cx);
            assert_eq!(model.active_view(), AppTab::Engine.sub_views()[1]);
            // Engine has three sub-views; Companion only has two, so index 2 is a no-op there.
            model.select_nth_sub_view(2, cx);
            assert_eq!(model.active_view(), AppTab::Engine.sub_views()[2]);
            model.select_tab(AppTab::Companion, cx);
            let before = model.active_view();
            model.select_nth_sub_view(2, cx);
            assert_eq!(model.active_view(), before);
        });
    }

    #[gpui::test]
    fn ctrl_shift_bracket_cycles_the_current_views_own_sections(cx: &mut TestAppContext) {
        let model = cx.new(AppModel::new_for_test);
        model.update(cx, |model, cx| {
            model.select_view(AppView::Pulsations, cx);
            model.pulsations.update(cx, |pulsations, _| {
                pulsations.detail_section = PulsationDetailSection::Review;
            });
            model.cycle_current_section(1, cx);
            assert_eq!(
                model.pulsations.read(cx).detail_section,
                PulsationDetailSection::Overview
            );
            model.cycle_current_section(-1, cx);
            assert_eq!(
                model.pulsations.read(cx).detail_section,
                PulsationDetailSection::Review
            );
            // Wraps around in both directions.
            model.cycle_current_section(-1, cx);
            assert_eq!(
                model.pulsations.read(cx).detail_section,
                PulsationDetailSection::Artifacts
            );

            model.select_view(AppView::Settings, cx);
            model.cycle_current_section(1, cx);
            assert_eq!(
                model.settings.read(cx).settings_section,
                crate::settings::SettingsSection::Routes
            );

            // A view without a section strip (Vault) ignores the shortcut instead of panicking.
            model.select_view(AppView::Vault, cx);
            model.cycle_current_section(1, cx);
        });
    }

    #[gpui::test]
    fn tabs_restore_their_last_view_after_settings(cx: &mut TestAppContext) {
        let model = cx.new(AppModel::new_for_test);
        model.update(cx, |model, cx| {
            for view in [AppView::Logs, AppView::Pulsations, AppView::Chat] {
                model.select_view(view, cx);
            }
            model.select_view(AppView::Settings, cx);
            assert_eq!(model.active_tab(), AppTab::Companion);
            assert_eq!(AppTab::from_view(AppView::Settings), None);
            for (tab, view) in [
                (AppTab::Agent, AppView::Logs),
                (AppTab::Engine, AppView::Pulsations),
                (AppTab::Companion, AppView::Chat),
            ] {
                model.select_tab(tab, cx);
                assert_eq!(model.active_view(), view);
                assert_eq!(model.navigation.read(cx).active_tab, tab);
            }
        });
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
            assert!(model.agents.read(cx).snapshot.is_none());
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
        assert_eq!(request.workspace_view_mode, "editor");
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
            auto_open_review: true,
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
            assert!(model.auto_open_review);
            let workspace = model.workspace.read(cx);
            assert!(!workspace.context_drawer_open);
            // Legacy sessions persisted with `workspace_view_mode: "hitl"` still restore cleanly;
            // the Workspace view is editor-only now, and the round-tripped request always reports
            // "editor" (see the UI flow plan Part 1.2).
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
            assert_eq!(model.active_tab(), AppTab::Companion);
            assert_eq!(model.navigation.read(cx).active_tab, AppTab::Companion);
            assert_eq!(round_trip.workspace_view_mode, "editor");
            assert!(round_trip.auto_open_review);
            assert_eq!(round_trip.tabs[0].cursor_line, Some(7));
            assert_eq!(round_trip.tabs[0].cursor_column, Some(3));
        });
    }

    #[gpui::test]
    fn hydrates_persisted_layout_and_resets_transient_state(cx: &mut TestAppContext) {
        let model = cx.new(AppModel::new_for_test);
        model.update(cx, |model, cx| {
            model.pulsations.update(cx, |pulsations, _| {
                pulsations.diff_expanded = true;
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
                        auto_open_review: false,
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
            assert_eq!(model.active_tab(), AppTab::Engine);
            assert_eq!(model.navigation.read(cx).active_tab, AppTab::Engine);
            assert_eq!(model.last_view_per_tab[&AppTab::Engine], AppView::Vault);
            assert!(model.navigation.read(cx).sidebar_collapsed);
            assert!(!model.workspace.read(cx).context_drawer_open);
            assert!(!model.pulsations.read(cx).diff_expanded);
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
                Some("The file revision has changed on disk.")
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
            context_audit: vec![],
            graph_proposals: vec![],
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
    fn learning_graph_ui_requires_and_submits_each_individual_decision(cx: &mut TestAppContext) {
        use hvc_core::orchestration::{
            LearningGraphDecision, LearningGraphProposalSnapshot, LearningRunSnapshot,
            LearningState,
        };
        let backend = Arc::new(RecordingPulsationsBackend::new());
        let proposal = |id: &str| LearningGraphProposalSnapshot {
            id: id.into(),
            candidate_id: format!("candidate-{id}"),
            source_path: "knowledge/obsidian/features/root.md".into(),
            target_path: format!("knowledge/obsidian/tests/{id}.md"),
            relation: "tested-by".into(),
            preview_diff: format!("+ {id}"),
            rationale: "operational evidence".into(),
            evidence_ids: vec![format!("evidence-{id}")],
            decision: "pending".into(),
            publication_state: "not_requested".into(),
            diagnostic: None,
        };
        let mut detail = preparation_detail(false);
        detail.learning = LearningState::Published;
        detail.learning_run = Some(LearningRunSnapshot {
            job_id: "learning-graph".into(),
            generation: 11,
            budget_calls: 1,
            total_calls: 1,
            budget_grants: 1,
            artifact_id: Some("report".into()),
            diagnostic: None,
            context_audit: vec![],
            graph_proposals: vec![proposal("one"), proposal("two")],
        });
        backend.snapshot.lock().unwrap().details = vec![detail];
        let model = cx.new(|cx| {
            AppModel::new_for_test_with_domain_backends(
                backend.clone(),
                Arc::new(RecordingVaultBackend::new()),
                cx,
            )
        });
        model.update(cx, |model, cx| {
            configure_domain_workspace(model, cx);
            model.refresh_pulsations(cx);
        });
        cx.run_until_parked();

        model.update(cx, |model, cx| model.submit_learning_graph_review(cx));
        assert!(backend.actions.lock().unwrap().is_empty());
        model.update(cx, |model, cx| {
            model.choose_learning_graph_decision("one".into(), LearningGraphDecision::Approve, cx);
            model.choose_learning_graph_decision("two".into(), LearningGraphDecision::Reject, cx);
            model.submit_learning_graph_review(cx);
            model.submit_learning_graph_review(cx);
        });
        cx.run_until_parked();

        let actions = backend.actions.lock().unwrap();
        assert_eq!(actions.len(), 1);
        let crate::pulsations::PulsationAction::ReviewLearningGraph(request) = &actions[0] else {
            panic!("graph review action")
        };
        assert_eq!(request.job_id, "learning-graph");
        assert_eq!(request.expected_generation, 11);
        assert_eq!(request.decisions.len(), 2);
        assert_eq!(
            request.decisions[0].decision,
            LearningGraphDecision::Approve
        );
        assert_eq!(request.decisions[1].decision, LearningGraphDecision::Reject);
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
                .update(cx, |i, cx| i.set_value("  Create action test  ", cx));
            m.revise_description(cx);
        });
        cx.run_until_parked();
        let actions = backend.actions.lock().unwrap();
        let crate::pulsations::PulsationAction::Revise(request) = &actions[0] else {
            panic!("description action")
        };
        assert_eq!(request.expected_input_revision, 3);
        assert_eq!(request.description, "  Create action test  ");
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
                "Persisted fixture content"
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
                Some("Pulsation “Login” started.")
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
                Some("1 job(s) marked for cancellation.")
            );
        });
    }

    #[gpui::test]
    fn automatic_agent_stop_requires_confirmation_and_cancels_the_whole_pulse(
        cx: &mut TestAppContext,
    ) {
        let pulsations_backend = Arc::new(RecordingPulsationsBackend::new());
        let cancel_calls = Arc::clone(&pulsations_backend.cancel_calls);
        let model = cx.new(|cx| {
            AppModel::new_for_test_with_domain_backends(
                pulsations_backend,
                Arc::new(RecordingVaultBackend::new()),
                cx,
            )
        });

        model.update(cx, |model, cx| {
            model.request_cancel_agent_run("run-1".into(), Some("pulse-1".into()), cx);
        });
        assert!(cancel_calls.lock().unwrap().is_empty());
        cx.read(|cx| {
            let agents = model.read(cx).agents.read(cx);
            assert_eq!(agents.pending_pipeline_cancel.as_deref(), Some("pulse-1"));
            assert_eq!(
                agents.notice.as_deref(),
                Some("Automatic stop cancels the entire pulse. Click again to confirm.")
            );
        });

        model.update(cx, |model, cx| {
            model.request_cancel_agent_run("run-1".into(), Some("pulse-1".into()), cx);
        });
        cx.run_until_parked();

        assert_eq!(*cancel_calls.lock().unwrap(), vec!["pulse-1"]);
        cx.read(|cx| {
            let agents = model.read(cx).agents.read(cx);
            assert_eq!(agents.pending_pipeline_cancel, None);
            assert_eq!(
                agents.notice.as_deref(),
                Some("Pulse canceled; 1 job(s) were interrupted.")
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
