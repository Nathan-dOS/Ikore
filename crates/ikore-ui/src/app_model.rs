use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

#[path = "exchange_follow.rs"]
mod exchange_follow;
#[path = "preparation_ui.rs"]
mod preparation_ui;
#[path = "resolution_ui.rs"]
mod resolution_ui;

use gpui::{
    AppContext as _, ClipboardItem, Context, Entity, PathPromptOptions, SharedString, Subscription,
    Task, Window,
};
use ikore_core::agent_runtime::{AdaptiveContextMode, ContextStrategy, DynamicContextPolicy};
use ikore_core::chat_service::{ChatEventEnvelope, ChatTransportKind};
use ikore_core::llm::{
    DeleteProviderConnectionRequest, DeleteProviderRouteRequest, ResetLlmResilienceStateRequest,
    RouteTarget, RoutingMode, TestProviderConnectionRequest,
    UpsertFreeFormProviderRouteBindingRequest, UpsertProviderConnectionRequest,
    UpsertProviderRouteBindingRequest, UpsertProviderRouteRequest,
};
use ikore_core::orchestration::brownfield::BrownfieldOnboardingStatus;
use ikore_core::orchestration::{CleanupFailedPulsationFilesRequest, IndexingEvent};
use ikore_core::persistence_service::{
    ClearExecutionOverrideRequest, CliFallbackDecision, EnsureProjectForWorkspaceRequest,
    HitlDecision, LoadUiSessionRequest, PulsationRuntimeSnapshot, PulsationStage, PulsationStatus,
    ResolveCliFallbackRequest, SaveUiSessionRequest, StageFailureDecision,
    StartFreeFormApiRunRequest, UiSessionState, UiTabSnapshot, UpdateAgentRuntimeConfigRequest,
};
use ikore_core::terminals::{
    CliAgentConfig, CliBackend, CliModelSelectionSource, CreateFreeFormSlotRequest,
    ResumeManualRunRequest, StartFreeFormRunRequest, StartManualRunRequest, TerminalRunStatus,
    TerminalSlot, ThinkingLevel, UpdateCliAgentConfigRequest, UpdateFreeFormSlotRequest,
};
use ikore_core::workspace_service::{
    FileNode, OpenWorkspaceRequest, ReadWorkspaceFileRequest, SearchMode, WorkspaceChangedEvent,
    WorkspaceKind,
};
use ikore_core::{AppCore, AppEvent};
use ikore_editor::{EditorSelection, EditorSnapshot, TextPosition};
use tokio::sync::broadcast::error::RecvError;

use crate::activity::{ActivityEntry, ActivityKind};
use crate::agents::{
    AgentIdentifier, AgentsBackend, AgentsDashboardSnapshot, AgentsModel, ApiActivityFilter,
    CliModelPickerState, CliModelPickerTarget, CoreAgentsBackend,
};
use crate::chat::{ChatBackend, ChatEventOutcome, ChatModel, ChatOperation, CoreChatBackend};
use crate::errors::UiCommandError;
use crate::flow::FlowTarget;
use crate::icons::Icon;
use crate::lifecycle::LifecycleState;
use crate::logs::{CoreLogsBackend, LogsBackend, LogsModel, SequenceClassification};
use crate::pulsations::{
    CorePulsationsBackend, PulsationDetailSection, PulsationsBackend, PulsationsModel,
    ReportNotice, report_available,
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
use ikore_core::embedding_service::ModelPullEvent;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppView {
    Home,
    Forge,
    Workspace,
    Pulsations,
    Vault,
    Chat,
    Notes,
    Canvas,
    Agents,
    Settings,
    Logs,
}

impl AppView {
    pub const ALL: [Self; 11] = [
        Self::Home,
        Self::Forge,
        Self::Workspace,
        Self::Pulsations,
        Self::Vault,
        Self::Chat,
        Self::Notes,
        Self::Canvas,
        Self::Agents,
        Self::Settings,
        Self::Logs,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::Workspace => "workspace",
            Self::Pulsations => "pulsations",
            Self::Forge => "forge",
            Self::Vault => "vault",
            Self::Chat => "chat",
            Self::Notes => "notes",
            Self::Canvas => "canvas",
            Self::Agents => "agents",
            Self::Settings => "settings",
            Self::Logs => "logs",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Home => "Home",
            Self::Workspace => "Workspace & HITL",
            Self::Pulsations => "Pulsations & Pipeline",
            Self::Forge => "Forge — Tasks & Features",
            Self::Vault => "Vault & Graph",
            Self::Chat => "Chat / Companion",
            Self::Notes => "Notes / Scribe",
            Self::Canvas => "Canvas / Architecture",
            Self::Agents => "Agents & Terminais",
            Self::Settings => "Settings & AI Runtime",
            Self::Logs => "Logs & Observability",
        }
    }

    pub const fn short_label(self) -> &'static str {
        match self {
            Self::Home => "Home",
            Self::Workspace => "Workspace",
            Self::Pulsations => "Pulsations",
            Self::Forge => "Forge",
            Self::Vault => "Vault",
            Self::Chat => "Companion",
            Self::Notes => "Notes",
            Self::Canvas => "Canvas",
            Self::Agents => "Agents",
            Self::Settings => "Settings",
            Self::Logs => "Logs",
        }
    }

    /// Rail / tooltip glyph for the view.
    pub(crate) const fn icon(self) -> Icon {
        match self {
            Self::Home => Icon::House,
            Self::Workspace => Icon::FolderOpen,
            Self::Pulsations => Icon::Activity,
            Self::Forge => Icon::Hammer,
            Self::Vault => Icon::Vault,
            Self::Chat => Icon::MessageSquare,
            Self::Notes => Icon::NotebookPen,
            Self::Canvas => Icon::Network,
            Self::Agents => Icon::Bot,
            Self::Settings => Icon::Settings,
            Self::Logs => Icon::ScrollText,
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::Home => "Active pulsation, next step, and what is waiting on you",
            Self::Workspace => "Files, tabs, editor, preview, and HITL review",
            Self::Pulsations => "Executions, checkpoints, and human decisions",
            Self::Forge => "Task backlog, discovery, and feature planning",
            Self::Vault => "Markdown notes, links, and workspace context",
            Self::Chat => "Conversation, streaming, and read-only tools",
            Self::Notes => "Loose ideas the Notes Scribe turns into structured notes",
            Self::Canvas => "Native diagrams, connected shapes, and project drawings",
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

/// Visual grouping of the workbench views on the nav rail (a divider separates each group). It
/// used to be the first level of a two-level tab navigation; since the rail redesign it carries no
/// navigation state of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AppTab {
    Agent,
    Engine,
    Companion,
}

impl AppTab {
    pub const ALL: [Self; 3] = [Self::Agent, Self::Engine, Self::Companion];

    /// Rail order: the Engine group (where work is composed and runs) sits first.
    pub const RAIL_ORDER: [Self; 3] = [Self::Engine, Self::Agent, Self::Companion];

    pub const fn sub_views(self) -> &'static [AppView] {
        match self {
            Self::Agent => &[AppView::Agents, AppView::Logs],
            Self::Engine => &[AppView::Forge, AppView::Pulsations, AppView::Vault],
            Self::Companion => &[
                AppView::Chat,
                AppView::Notes,
                AppView::Canvas,
                AppView::Workspace,
            ],
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

/// Repeated Forge/watcher events inside this window collapse into one activity line.
const ACTIVITY_COALESCE_WINDOW: std::time::Duration = std::time::Duration::from_secs(2);
/// How often Review re-reads a Visualizer note still being written (ADR-027 M5).
const VISUALIZER_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3);
/// ADR-034 M3: shown when Approve needs an acknowledgement and the feedback is empty.
pub(crate) const RISK_REASON_REQUIRED: &str = "This is a high-risk review with unverified or unrun items. Write why you approve it in the feedback, then approve again.";
/// ADR-032: shown when a plain revision is asked for beyond the threshold.
const THRESHOLD_CHOICE: &str = "This pulsation reached its revision-round threshold. Write the reason in the feedback, then choose Continue revising or Replan from Specification, or reject.";
const REASON_REQUIRED: &str =
    "A decision beyond the revision threshold needs a reason: write it in the feedback first.";

/// Per-tab attention counters shown as header badges and, unfolded, in the status bar. See the UI
/// flow plan Part 4.4.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TabBadges {
    pub agent_failed_runs: usize,
    pub engine_awaiting_hitl: usize,
    /// Distinct pulsations waiting for any developer decision.
    pub engine_pending_decisions: usize,
    pub engine_running_jobs: usize,
    pub companion_streaming: usize,
    /// Notes the Notes Scribe is working on.
    pub notes_running: usize,
    /// Open Specification clarification checkpoints across the project.
    pub clarifications: usize,
    /// CLI runs parked on a fallback decision.
    pub agent_fallbacks: usize,
}

pub struct AppModel {
    pub(crate) core: Option<Arc<AppCore>>,
    pub(crate) companion_dock: crate::companion_dock::CompanionDockUi,
    pub(crate) forge: crate::forge::ForgeUi,
    pub(crate) notes: crate::notes::NotesUi,
    pub(crate) canvas: crate::canvas::CanvasUi,
    pub(crate) canvas_authoring: crate::canvas_authoring_ui::CanvasAuthoringUi,
    pending_canvas_workspace: Option<String>,
    pub(crate) canvas_close_requested: bool,
    pub(crate) canvas_project_switching: bool,
    canvas_close_window: Option<gpui::AnyWindowHandle>,
    /// ADR-034 M5: notes whose linked code changed.
    pub(crate) doc_drift: crate::doc_drift::DocDriftUi,
    session_persistence: Option<Arc<dyn SessionPersistence>>,
    workspace_backend: Option<Arc<dyn WorkspaceBackend>>,
    pub(crate) pulsations_backend: Option<Arc<dyn PulsationsBackend>>,
    vault_backend: Option<Arc<dyn VaultBackend>>,
    pub(crate) settings_backend: Option<Arc<dyn SettingsBackend>>,
    pub(crate) agents_backend: Option<Arc<dyn AgentsBackend>>,
    logs_backend: Option<Arc<dyn LogsBackend>>,
    chat_backend: Option<Arc<dyn ChatBackend>>,
    pub navigation: Entity<NavigationModel>,
    pub workspace: Entity<WorkspaceEditorModel>,
    pub pulsations: Entity<PulsationsModel>,
    pub(crate) agents_split: Entity<crate::split_pane::SplitPaneState>,
    pub(crate) pulsations_split: Entity<crate::split_pane::SplitPaneState>,
    pub(crate) workspace_tree_split: Entity<crate::split_pane::SplitPaneState>,
    pub(crate) workspace_context_split: Entity<crate::split_pane::SplitPaneState>,
    /// Feature list against the section body, shared by every Forge section.
    pub(crate) forge_feature_split: Entity<crate::split_pane::SplitPaneState>,
    /// Incubation only: conversation against the proposed specification.
    pub(crate) forge_incubation_split: Entity<crate::split_pane::SplitPaneState>,
    /// Triage only: the evaluation form against the agent's analysis. Separate
    /// from the incubation split because a form and a transcript want different
    /// proportions.
    pub(crate) forge_triage_split: Entity<crate::split_pane::SplitPaneState>,
    /// Notes: the capture and list column against the selected note.
    pub(crate) notes_split: Entity<crate::split_pane::SplitPaneState>,
    /// Vault: files/search column | (graph | inspector).
    pub(crate) vault_files_split: Entity<crate::split_pane::SplitPaneState>,
    pub(crate) vault_detail_split: Entity<crate::split_pane::SplitPaneState>,
    pub vault: Entity<VaultModel>,
    #[cfg(test)]
    pub(crate) pulsation_feature_input: Entity<NativeTextInput>,
    pub(crate) reconciliation_input: Entity<NativeTextInput>,
    pub(crate) description_review_input: Entity<NativeTextInput>,
    pub(crate) specification_answer_inputs: Vec<(String, Entity<NativeTextInput>)>,
    pub(crate) stage_feedback_input: Entity<NativeTextInput>,
    /// Keeps the Review's C/F/Tab/Enter/R/D bindings reachable after a click on the brief.
    pub(crate) review_focus: gpui::FocusHandle,
    /// Notes of flagged brief items, by item index, for the checkpoint in
    /// `brief_notes_scope` (ADR-024 M2). Never persisted.
    pub(crate) brief_notes: HashMap<usize, Entity<NativeTextInput>>,
    brief_notes_scope: Option<(String, u64)>,
    /// `(pulsation, checkpoint, state version)` the free-text feedback belongs to.
    stage_feedback_scope: Option<(String, String, u64)>,
    /// The feedback of the last HITL decision request, for tests.
    #[cfg(test)]
    pub(crate) last_revision_feedback: Option<String>,
    pub(crate) preparation_threshold_input: Entity<NativeTextInput>,
    pub(crate) composition_override_model_input: Entity<NativeTextInput>,
    pub(crate) cargo_toolchain_input: Entity<NativeTextInput>,
    pub(crate) cargo_additional_inputs_input: Entity<NativeTextInput>,
    pub(crate) cargo_timeout_input: Entity<NativeTextInput>,
    pub(crate) sanitization_path_input: Entity<NativeTextInput>,
    pub(crate) sanitization_reason_input: Entity<NativeTextInput>,
    /// ADR-034 M3: the project's additions to the built-in risk policy.
    pub(crate) risk_globs_input: Entity<NativeTextInput>,
    pub(crate) risk_medium_checks_input: Entity<NativeTextInput>,
    pub(crate) risk_high_checks_input: Entity<NativeTextInput>,
    pub(crate) context_limit_input: Entity<NativeTextInput>,
    preparation_scope: Option<(String, u64, Option<String>)>,
    pub(crate) learning_graph_decisions:
        HashMap<String, ikore_core::orchestration::LearningGraphDecision>,
    question_subscriptions: Vec<Subscription>,
    pub(crate) artifact_preview_page: usize,
    pub(crate) vault_search_input: Entity<NativeTextInput>,
    pub(crate) vault_rename_input: Entity<NativeTextInput>,
    pub(crate) settings_connection_name_input: Entity<NativeTextInput>,
    pub(crate) settings_connection_model_input: Entity<NativeTextInput>,
    pub(crate) settings_connection_endpoint_input: Entity<NativeTextInput>,
    pub(crate) settings_connection_secret_input: Entity<NativeTextInput>,
    pub(crate) settings_route_name_input: Entity<NativeTextInput>,
    pub(crate) settings_project_delete_input: Entity<NativeTextInput>,
    pub(crate) settings_embedding_model_input: Entity<NativeTextInput>,
    pub(crate) chat_draft_input: Entity<NativeTextInput>,
    chat_initial_input: Entity<NativeTextInput>,
    pub(crate) chat_composer_inputs: HashMap<String, Entity<NativeTextInput>>,
    chat_composer_subscriptions: HashMap<String, Vec<Subscription>>,
    pub(crate) cli_model_search_input: Entity<NativeTextInput>,
    pub(crate) agent_prompt_inputs: HashMap<ikore_core::llm::AgentRole, Entity<NativeTextInput>>,
    pub(crate) agent_model_inputs: HashMap<ikore_core::llm::AgentRole, Entity<NativeTextInput>>,
    pub(crate) agent_custom_executable_inputs:
        HashMap<ikore_core::llm::AgentRole, Entity<NativeTextInput>>,
    pub(crate) agent_custom_args_inputs:
        HashMap<ikore_core::llm::AgentRole, Entity<NativeTextInput>>,
    pub(crate) free_form_prompt_inputs: HashMap<String, Entity<NativeTextInput>>,
    pub(crate) free_form_label_inputs: HashMap<String, Entity<NativeTextInput>>,
    pub(crate) free_form_model_inputs: HashMap<String, Entity<NativeTextInput>>,
    pub(crate) free_form_custom_executable_inputs: HashMap<String, Entity<NativeTextInput>>,
    pub(crate) free_form_custom_args_inputs: HashMap<String, Entity<NativeTextInput>>,
    pub chat: Entity<ChatModel>,
    pub resolution: Entity<crate::resolution::ResolutionModel>,
    pub(crate) resolution_draft_input: Entity<NativeTextInput>,
    pub(crate) resolution_synthesis_input: Entity<NativeTextInput>,
    resolution_chips_in_flight: bool,
    resolution_turn_in_flight: bool,
    resolution_chips_task: Option<Task<()>>,
    resolution_conversation_task: Option<Task<()>>,
    resolution_action_task: Option<Task<()>>,
    resolution_turn_task: Option<Task<()>>,
    pub(crate) agents: Entity<AgentsModel>,
    pub(crate) settings: Entity<SettingsModel>,
    /// The Settings modal (search, focus, scroll); page and drill-in state live in `settings`.
    pub(crate) settings_modal: crate::settings_modal::SettingsModalState,
    /// Home's seven-day usage read (the charts' other data come from existing snapshots).
    pub(crate) home_stats: crate::home_stats::HomeStatsState,
    pub(crate) logs: Entity<LogsModel>,
    active_view: AppView,
    /// Wall-clock `HH:MM` shown in the status bar; refreshed by the lifecycle poll.
    footer_clock: SharedString,
    /// Home's "recent activity": one line per milestone across every event family.
    pub(crate) activity: crate::activity::ActivityFeed,
    status: SharedString,
    event_count: u64,
    needs_resync: bool,
    pub(crate) indexing_status: Option<ikore_core::orchestration::IndexingEvent>,
    pub(crate) model_pull_status: Option<ikore_core::embedding_service::ModelPullEvent>,
    /// Voice dictation setup and push-to-talk, as the core last reported it (ADR-029).
    pub(crate) voice_status: Option<ikore_core::voice::VoiceStatus>,
    pub(crate) notifications: crate::notifications::Notifications,
    lifecycle: LifecycleState,
    hydrating: bool,
    session_generation: u64,
    session_dirty: bool,
    workspace_save_generations: HashMap<String, u64>,
    suppress_editor_sync: bool,
    /// The editor's edit generation last copied into the active tab. Cursor moves, scrolling
    /// and focus changes also notify the editor; only a new generation is worth materializing
    /// the whole document for.
    synced_edit_generation: u64,
    event_task: Option<Task<()>>,
    /// Token deltas are durable before they reach this model. We only coalesce their visual
    /// projection, leaving state changes, failures, and cancellation immediate.
    llm_delta_task: Option<Task<()>>,
    pending_llm_deltas: Vec<ikore_core::llm::LlmEventEnvelope>,
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
    pulsation_snapshot_task: Option<Task<()>>,
    snapshot_in_flight: bool,
    snapshot_pending: Option<Option<String>>,
    snapshot_revision: u64,
    hitl_documents_task: Option<Task<()>>,
    /// Re-reads a pending Visualizer note until it is written or omitted.
    visualizer_poll_task: Option<Task<()>>,
    failure_task: Option<Task<()>>,
    failure_detail_task: Option<Task<()>>,
    backend_override_task: Option<Task<()>>,
    backend_override_mutation_task: Option<Task<()>>,
    cleanup_preview_task: Option<Task<()>>,
    cleanup_mutation_task: Option<Task<()>>,
    report_task: Option<Task<()>>,
    /// The Attestation card's request (ADR-036 M3).
    pub(crate) attestation_task: Option<Task<()>>,
    /// "Verify a file…" on the Security page.
    pub(crate) attestation_file_task: Option<Task<()>>,
    /// Hands a written pulsation report to the system's default application
    /// for HTML. Tests replace it: their platform cannot open files.
    report_opener: ReportOpener,
    artifact_read: crate::lifecycle::RevisionRead,
    backend_read: crate::lifecycle::RevisionRead,
    cleanup_read: crate::lifecycle::RevisionRead,
    event_resync_pending: bool,
    /// A terminal/agents sequence gap raised `needs_resync`; the next applied Agents snapshot
    /// is the resync that clears it. Without this the header stayed red for the whole session.
    agents_resync_pending: bool,
    /// ADR-037 M2: the cursor over the open project's exchange log.
    pub(crate) exchange: Option<ikore_core::exchange::log::ExchangeFollower>,
    /// The last time the log answered `reset`; the doorbell is trusted again a minute later.
    pub(crate) exchange_reset_at: Option<std::time::Instant>,
    pub(crate) exchange_read_task: Option<Task<()>>,
    pub(crate) last_pulsation_poll: Option<std::time::Instant>,
    /// Coalesces the pulsation refreshes a burst of doorbells asks for.
    pub(crate) doorbell_refresh_task: Option<Task<()>>,
    /// Ranged re-reads of terminal runs that skipped events, by run.
    pub(crate) terminal_gap_tasks: HashMap<String, Task<()>>,
    vault_refresh_task: Option<Task<()>>,
    vault_search_task: Option<Task<()>>,
    vault_rename_task: Option<Task<()>>,
    vault_context_task: Option<Task<()>>,
    vault_document_tasks: HashMap<String, Task<()>>,
    settings_task: Option<Task<()>>,
    cli_connections_task: Option<Task<()>>,
    /// Plan usage of the subscription CLIs (ADR-028): header pill, popover and cards.
    pub(crate) quota: crate::quota_bar::QuotaUi,
    /// A connection test in flight; separate from `settings_task` so a refresh never drops it.
    settings_test_task: Option<Task<()>>,
    /// A quiet Settings refresh is already scheduled (see `schedule_settings_quiet_refresh`).
    settings_quiet_refresh_pending: bool,
    embedding_readiness_task: Option<Task<()>>,
    model_pull_task: Option<Task<()>>,
    agents_task: Option<Task<()>>,
    /// At most one snapshot request may follow the current Agents read. `false` means the panel
    /// needs a loud refresh; quiet chrome refreshes never displace it.
    agents_refresh_pending: Option<(bool, bool)>,
    agents_notice_timer: Option<Task<()>>,
    model_catalog_task: Option<Task<()>>,
    logs_task: Option<Task<()>>,
    logs_resync_tasks: HashMap<String, Task<()>>,
    chat_hydration_task: Option<Task<()>>,
    chat_create_task: Option<Task<()>>,
    chat_tasks: HashMap<(String, ChatOperation), Task<()>>,
    /// Applying or remembering the Companion panel's target; separate from
    /// project hydration so a conversation load is never cancelled by it.
    chat_target_task: Option<Task<()>>,
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
        let canvas_authoring_backend = Arc::new(
            crate::canvas_authoring_ui::CoreCanvasAuthoringBackend::new(Arc::clone(&core)),
        );
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
        model.canvas_authoring.backend = Some(canvas_authoring_backend);
        model.start_lifecycle(cx);
        model.start_quota_poller(cx);
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

    #[cfg(test)]
    pub(crate) fn new_for_test_with_resolution_backends(
        pulsations_backend: Arc<dyn PulsationsBackend>,
        chat_backend: Arc<dyn ChatBackend>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut model = Self::build(None, None, None, cx);
        model.pulsations_backend = Some(pulsations_backend);
        model.chat_backend = Some(chat_backend);
        model
    }

    fn build(
        core: Option<Arc<AppCore>>,
        session_persistence: Option<Arc<dyn SessionPersistence>>,
        workspace_backend: Option<Arc<dyn WorkspaceBackend>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let active_view = AppView::Home;
        let navigation = cx.new(|_| NavigationModel {
            active: active_view,
            context_rail_collapsed: false,
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
        let composition_override_model_input =
            cx.new(|cx| NativeTextInput::new("Model id (empty = inherit from the stage)", cx));
        let cargo_toolchain_input = cx
            .new(|cx| NativeTextInput::new("Installed toolchain (empty = project selection)", cx));
        let cargo_additional_inputs_input = cx
            .new(|cx| NativeTextInput::new("Extra input paths/globs, separated by semicolons", cx));
        let cargo_timeout_input =
            cx.new(|cx| NativeTextInput::new("Timeout in seconds (1–900)", cx));
        let sanitization_path_input =
            cx.new(|cx| NativeTextInput::new("Repository path, e.g. tests/fixtures/client.rs", cx));
        let sanitization_reason_input =
            cx.new(|cx| NativeTextInput::new("Why is this occurrence a synthetic fixture?", cx));
        let risk_globs_input = cx.new(|cx| {
            NativeTextInput::new(
                "More sensitive paths, one glob per line (e.g. billing/**)",
                cx,
            )
        });
        let risk_medium_checks_input = cx.new(|cx| {
            NativeTextInput::new("Checks required from medium risk, comma-separated", cx)
        });
        let risk_high_checks_input =
            cx.new(|cx| NativeTextInput::new("Checks required at high risk, comma-separated", cx));
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
        let settings_project_delete_input =
            cx.new(|cx| NativeTextInput::new("Type the exact project name", cx));
        let settings_embedding_model_input =
            cx.new(|cx| NativeTextInput::new("Embedding model", cx));
        let chat_draft_input =
            cx.new(|cx| NativeTextInput::new("Message to Companion", cx).multiline());
        let resolution_draft_input =
            cx.new(|cx| NativeTextInput::new("Reply to the Companion", cx).multiline());
        let resolution_synthesis_input = cx.new(|cx| {
            NativeTextInput::new("FILES_NEW:/FILES_MOD:/FILES: reply to confirm", cx).multiline()
        });
        let cli_model_search_input =
            cx.new(|cx| NativeTextInput::new("Search model ID, name, provider or capability", cx));
        let settings_modal = crate::settings_modal::SettingsModalState::new(cx);
        let agent_prompt_inputs = ikore_core::llm::AgentRole::ALL
            .into_iter()
            .map(|role| {
                let descriptor = ikore_core::terminals::canonical_agent(role);
                (
                    role,
                    cx.new(|cx| {
                        NativeTextInput::new(format!("Manual prompt for {}", descriptor.name), cx)
                    }),
                )
            })
            .collect::<HashMap<_, _>>();
        let agent_model_inputs = ikore_core::llm::AgentRole::ALL
            .into_iter()
            .map(|role| {
                (
                    role,
                    cx.new(|cx| NativeTextInput::new("Optional CLI model", cx)),
                )
            })
            .collect::<HashMap<_, _>>();
        let agent_custom_executable_inputs = ikore_core::llm::AgentRole::ALL
            .into_iter()
            .map(|role| {
                (
                    role,
                    cx.new(|cx| NativeTextInput::new("Absolute executable path", cx)),
                )
            })
            .collect::<HashMap<_, _>>();
        let agent_custom_args_inputs = ikore_core::llm::AgentRole::ALL
            .into_iter()
            .map(|role| {
                (
                    role,
                    cx.new(|cx| NativeTextInput::new("JSON arguments: [\"--flag\",\"value\"]", cx)),
                )
            })
            .collect::<HashMap<_, _>>();
        let agents_split =
            cx.new(|_| crate::split_pane::SplitPaneState::new(220.0, 180.0, 400.0, 420.0));
        let pulsations_split =
            cx.new(|_| crate::split_pane::SplitPaneState::new(292.0, 220.0, 480.0, 420.0));
        let workspace_tree_split =
            cx.new(|_| crate::split_pane::SplitPaneState::new(220.0, 160.0, 400.0, 480.0));
        let workspace_context_split =
            cx.new(|_| crate::split_pane::SplitPaneState::proportional(0.78, 480.0, 180.0));
        let forge_feature_split =
            cx.new(|_| crate::split_pane::SplitPaneState::new(245.0, 180.0, 420.0, 460.0));
        let forge_incubation_split =
            cx.new(|_| crate::split_pane::SplitPaneState::proportional(0.55, 300.0, 260.0));
        let forge_triage_split =
            cx.new(|_| crate::split_pane::SplitPaneState::proportional(0.58, 300.0, 260.0));
        let notes_split =
            cx.new(|_| crate::split_pane::SplitPaneState::new(300.0, 240.0, 440.0, 360.0));
        // Right minimum = graph + inspector minimums plus their sash, so the files column
        // never pushes the inspector out at the 960 px window.
        let vault_files_split =
            cx.new(|_| crate::split_pane::SplitPaneState::new(264.0, 200.0, 420.0, 486.0));
        let vault_detail_split =
            cx.new(|_| crate::split_pane::SplitPaneState::proportional(0.62, 260.0, 220.0));
        let mut subscriptions = vec![
            cx.observe(&agents_split, |_, _, cx| cx.notify()),
            cx.observe(&pulsations_split, |_, _, cx| cx.notify()),
            cx.observe(&workspace_tree_split, |_, _, cx| cx.notify()),
            cx.observe(&workspace_context_split, |_, _, cx| cx.notify()),
            cx.observe(&forge_feature_split, |_, _, cx| cx.notify()),
            cx.observe(&forge_incubation_split, |_, _, cx| cx.notify()),
            cx.observe(&forge_triage_split, |_, _, cx| cx.notify()),
            cx.observe(&notes_split, |_, _, cx| cx.notify()),
            cx.observe(&vault_files_split, |_, _, cx| cx.notify()),
            cx.observe(&vault_detail_split, |_, _, cx| cx.notify()),
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
            cx.observe(&composition_override_model_input, |_, _, cx| cx.notify()),
            cx.observe(&cargo_toolchain_input, |_, _, cx| cx.notify()),
            cx.observe(&cargo_additional_inputs_input, |_, _, cx| cx.notify()),
            cx.observe(&cargo_timeout_input, |_, _, cx| cx.notify()),
            cx.observe(&context_limit_input, |_, _, cx| cx.notify()),
            cx.observe(&vault_search_input, |_, _, cx| cx.notify()),
            cx.observe(&vault_rename_input, |_, _, cx| cx.notify()),
            cx.observe(&settings_connection_name_input, |_, _, cx| cx.notify()),
            cx.observe(&settings_connection_model_input, |_, _, cx| cx.notify()),
            cx.observe(&settings_connection_endpoint_input, |_, _, cx| cx.notify()),
            cx.observe(&settings_connection_secret_input, |_, _, cx| cx.notify()),
            cx.observe(&settings_route_name_input, |_, _, cx| cx.notify()),
            cx.observe(&settings_project_delete_input, |_, _, cx| cx.notify()),
            cx.observe(&sanitization_path_input, |_, _, cx| cx.notify()),
            cx.observe(&sanitization_reason_input, |_, _, cx| cx.notify()),
            cx.observe(&risk_globs_input, |_, _, cx| cx.notify()),
            cx.observe(&risk_medium_checks_input, |_, _, cx| cx.notify()),
            cx.observe(&risk_high_checks_input, |_, _, cx| cx.notify()),
            cx.observe(&settings_embedding_model_input, |_, _, cx| cx.notify()),
            cx.observe(&chat_draft_input, |model, input, cx| {
                model.chat_composer_changed(input, cx);
            }),
            cx.subscribe(
                &chat_draft_input,
                |model, input, _: &crate::text_input::Submit, cx| {
                    model.send_chat_composer_input(input, cx)
                },
            ),
            cx.observe(&resolution_draft_input, |_, _, cx| cx.notify()),
            cx.subscribe(
                &resolution_draft_input,
                |model, _, _: &crate::text_input::Submit, cx| model.send_resolution_message(cx),
            ),
            cx.observe(&resolution_synthesis_input, |model, _, cx| {
                model.update_resolution_synthesis_draft(cx);
                cx.notify()
            }),
            cx.observe(&cli_model_search_input, |_, _, cx| cx.notify()),
            cx.observe(&settings_modal.search, |model, _, cx| {
                model.settings_search_changed(cx)
            }),
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
            companion_dock: crate::companion_dock::CompanionDockUi::default(),
            forge: crate::forge::ForgeUi::new(cx),
            notes: crate::notes::NotesUi::new(cx),
            canvas: crate::canvas::CanvasUi::new(cx),
            canvas_authoring: crate::canvas_authoring_ui::CanvasAuthoringUi::default(),
            pending_canvas_workspace: None,
            canvas_close_requested: false,
            canvas_project_switching: false,
            canvas_close_window: None,
            doc_drift: crate::doc_drift::DocDriftUi::new(cx),
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
            agents_split,
            pulsations_split,
            workspace_tree_split,
            workspace_context_split,
            forge_feature_split,
            forge_incubation_split,
            forge_triage_split,
            notes_split,
            vault_files_split,
            vault_detail_split,
            vault: cx.new(|_| VaultModel::default()),
            #[cfg(test)]
            pulsation_feature_input,
            reconciliation_input,
            description_review_input,
            stage_feedback_input,
            review_focus: cx.focus_handle(),
            brief_notes: HashMap::new(),
            brief_notes_scope: None,
            stage_feedback_scope: None,
            #[cfg(test)]
            last_revision_feedback: None,
            preparation_threshold_input,
            composition_override_model_input,
            cargo_toolchain_input,
            cargo_additional_inputs_input,
            cargo_timeout_input,
            sanitization_path_input,
            sanitization_reason_input,
            risk_globs_input,
            risk_medium_checks_input,
            risk_high_checks_input,
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
            settings_project_delete_input,
            settings_embedding_model_input,
            chat_initial_input: chat_draft_input.clone(),
            chat_draft_input,
            chat_composer_inputs: HashMap::new(),
            chat_composer_subscriptions: HashMap::new(),
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
            resolution: cx.new(|_| crate::resolution::ResolutionModel::default()),
            resolution_draft_input,
            resolution_synthesis_input,
            resolution_chips_in_flight: false,
            resolution_turn_in_flight: false,
            resolution_chips_task: None,
            resolution_conversation_task: None,
            resolution_action_task: None,
            resolution_turn_task: None,
            agents: cx.new(|_| AgentsModel::default()),
            settings: cx.new(|_| SettingsModel::default()),
            settings_modal,
            home_stats: Default::default(),
            logs: cx.new(|_| LogsModel::default()),
            active_view,
            footer_clock: crate::time::current_clock_hhmm().into(),
            activity: crate::activity::ActivityFeed::default(),
            status: "Ready".into(),
            event_count: 0,
            needs_resync: false,
            indexing_status: None,
            model_pull_status: None,
            voice_status: None,
            notifications: crate::notifications::Notifications::default(),
            lifecycle: LifecycleState::default(),
            hydrating: false,
            session_generation: 0,
            session_dirty: false,
            workspace_save_generations: HashMap::new(),
            suppress_editor_sync: false,
            synced_edit_generation: 0,
            event_task: None,
            llm_delta_task: None,
            pending_llm_deltas: Vec::new(),
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
            pulsation_snapshot_task: None,
            snapshot_in_flight: false,
            snapshot_pending: None,
            snapshot_revision: 0,
            hitl_documents_task: None,
            visualizer_poll_task: None,
            failure_task: None,
            failure_detail_task: None,
            backend_override_task: None,
            backend_override_mutation_task: None,
            cleanup_preview_task: None,
            cleanup_mutation_task: None,
            report_task: None,
            attestation_task: None,
            attestation_file_task: None,
            report_opener: Rc::new(|path, cx| cx.open_with_system(path)),
            artifact_read: Default::default(),
            backend_read: Default::default(),
            cleanup_read: Default::default(),
            event_resync_pending: false,
            agents_resync_pending: false,
            exchange: None,
            exchange_reset_at: None,
            exchange_read_task: None,
            last_pulsation_poll: None,
            doorbell_refresh_task: None,
            terminal_gap_tasks: HashMap::new(),
            vault_refresh_task: None,
            vault_search_task: None,
            vault_rename_task: None,
            vault_context_task: None,
            vault_document_tasks: HashMap::new(),
            settings_task: None,
            cli_connections_task: None,
            quota: Default::default(),
            settings_test_task: None,
            settings_quiet_refresh_pending: false,
            embedding_readiness_task: None,
            model_pull_task: None,
            agents_task: None,
            agents_refresh_pending: None,
            agents_notice_timer: None,
            model_catalog_task: None,
            logs_task: None,
            logs_resync_tasks: HashMap::new(),
            chat_hydration_task: None,
            chat_create_task: None,
            chat_tasks: HashMap::new(),
            chat_target_task: None,
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

    pub(crate) fn footer_clock(&self) -> &SharedString {
        &self.footer_clock
    }

    /// Re-reads the wall clock; notifies only when the displayed minute changed.
    pub(crate) fn tick_footer_clock(&mut self, cx: &mut Context<Self>) {
        let now = crate::time::current_clock_hhmm();
        if self.footer_clock.as_ref() != now {
            self.footer_clock = now.into();
            cx.notify();
        }
    }

    /// `ctrl-shift-[` / `ctrl-shift-]`: cycles the current view's own section strip, or the Settings
    /// pages while the Settings modal is open. Each view keeps its own section enum
    /// (Pulsations/Forge), so this is a small per-view dispatch rather than a single shared
    /// cursor — see the UI flow plan Part 4.5.
    pub(crate) fn cycle_current_section(&mut self, direction: i32, cx: &mut Context<Self>) {
        fn cycle<T: Copy + PartialEq>(all: &[T], current: T, direction: i32) -> T {
            let position = all.iter().position(|item| *item == current).unwrap_or(0) as i32;
            let len = all.len() as i32;
            all[(position + direction).rem_euclid(len) as usize]
        }

        if self.settings_modal.open {
            let page = cycle(
                &crate::settings::SettingsPage::ALL,
                self.settings.read(cx).page,
                direction,
            );
            self.select_settings_nav(page, cx);
            return;
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
            _ => {}
        }
    }

    pub fn status(&self) -> &SharedString {
        &self.status
    }

    pub(crate) fn set_status_message(&mut self, message: impl Into<SharedString>) {
        self.status = message.into();
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

    pub fn indexing_status(&self) -> Option<&ikore_core::orchestration::IndexingEvent> {
        self.indexing_status.as_ref()
    }

    pub fn model_pull_status(&self) -> Option<&ikore_core::embedding_service::ModelPullEvent> {
        self.model_pull_status.as_ref()
    }

    pub(crate) fn observe_performance_frame(&mut self, window: &Window, cx: &mut Context<Self>) {
        if let Some(frame) = self
            .performance_probe
            .as_mut()
            .and_then(|probe| probe.fixture_frame())
        {
            let updates = crate::performance::apply_streaming_fixture(self, frame, cx);
            if let Some(probe) = self.performance_probe.as_mut() {
                probe.record_fixture_updates(updates);
            }
        }
        if self
            .performance_probe
            .as_mut()
            .is_some_and(|probe| probe.observe_render(window))
        {
            self.performance_probe = None;
        }
    }

    pub fn context_rail_collapsed(&self, cx: &gpui::App) -> bool {
        self.navigation.read(cx).context_rail_collapsed
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
        let agents = self.agents.read(cx);
        let resolution = self.resolution.read(cx);
        let workspace = self.workspace.read(cx);
        let project = workspace.project_id.as_ref().map(AsRef::as_ref);
        let pending: std::collections::HashSet<&str> = pulsations
            .pulsations
            .iter()
            .filter(|pulse| pulse.status == PulsationStatus::Checkpoint)
            .map(|pulse| pulse.id.as_str())
            .chain(
                pulsations
                    .hitl_checkpoints
                    .iter()
                    .map(|checkpoint| checkpoint.pulsation_id.as_str()),
            )
            .chain(
                pulsations
                    .failure_checkpoints
                    .iter()
                    .map(|checkpoint| checkpoint.pulsation_id.as_str()),
            )
            .chain(
                resolution
                    .chips
                    .iter()
                    .filter_map(|questions| questions.fence.pulsation_id.as_deref()),
            )
            .chain(
                agents
                    .fallbacks
                    .iter()
                    .map(|fallback| fallback.pulsation_id.as_str()),
            )
            .chain(
                self.companion_dock
                    .pending
                    .iter()
                    .filter(|decision| Some(decision.project_id.as_str()) == project)
                    .filter_map(|decision| decision.pulsation_id.as_deref()),
            )
            .collect();
        TabBadges {
            agent_failed_runs: agents.failed_agent_count(),
            engine_awaiting_hitl: pulsations.awaiting_hitl,
            engine_pending_decisions: if pending.is_empty() {
                pulsations.awaiting_hitl + pulsations.awaiting_failure
            } else {
                pending.len()
            },
            engine_running_jobs: pulsations.running_jobs,
            companion_streaming: self.chat.read(cx).streaming_count(),
            notes_running: self.notes.snapshot.running.len(),
            clarifications: self.resolution.read(cx).chips.len(),
            agent_fallbacks: agents.fallbacks.len(),
        }
    }

    pub fn select_view(&mut self, view: AppView, cx: &mut Context<Self>) {
        // Settings is a modal over the current view, not a view of its own.
        if view == AppView::Settings {
            self.open_settings(None, cx);
            return;
        }
        self.close_settings(false, cx);
        if self.active_view != view {
            self.close_cli_model_picker(cx);
        }
        self.active_view = view;
        self.navigation.update(cx, |navigation, _| {
            navigation.active = view;
        });
        self.pulsations.update(cx, |pulsations, _| {
            pulsations.diff_expanded = false;
        });
        self.confirm_workspace_overwrite = None;
        self.confirm_hitl_reject = false;
        self.confirm_stage_reject = false;
        self.confirm_failure_abort = false;
        self.confirm_brownfield_revoke = false;
        self.notes.confirm_delete = None;
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
            AppView::Pulsations => {
                self.refresh_pulsation_usage(cx);
                self.refresh_hitl_documents(cx);
                self.refresh_failure_detail(cx);
                if let Some(id) = self.pulsations.read(cx).active_id() {
                    self.refresh_pulsation_artifacts(id.clone(), cx);
                    self.refresh_backend_override(id, cx);
                }
            }
            AppView::Forge => self.refresh_forge(cx),
            AppView::Notes => self.refresh_notes(cx),
            AppView::Canvas => self.refresh_canvas(cx),
            // Both explorers show the same tree; either one opening consumes the stale marker.
            AppView::Vault | AppView::Workspace if self.vault.read(cx).stale => {
                self.schedule_vault_refresh(cx)
            }
            AppView::Agents => self.refresh_agents(cx),
            AppView::Logs => self.refresh_logs(cx),
            AppView::Home => self.refresh_home_stats(false, cx),
            _ => {}
        }
        self.schedule_session_persistence(cx);
        cx.notify();
    }

    pub fn toggle_context_rail(&mut self, cx: &mut Context<Self>) {
        let collapsed = !self.navigation.read(cx).context_rail_collapsed;
        self.set_context_rail_collapsed(collapsed, cx);
    }

    pub fn set_context_rail_collapsed(&mut self, collapsed: bool, cx: &mut Context<Self>) {
        self.navigation.update(cx, |navigation, _| {
            navigation.context_rail_collapsed = collapsed;
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

    /// Settings > General > "Open reviews automatically" (default off; recommended on). See the
    /// UI flow plan Part 4.3.
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
        let message = message.into();
        if matches!(kind, HeaderNoticeKind::Warning | HeaderNoticeKind::Error) {
            let target = action.as_ref().map(|action| {
                FlowTarget::PulsationSection(
                    action.pulsation_id.clone(),
                    PulsationDetailSection::Review,
                )
            });
            self.record_activity(ActivityKind::Notice, message.clone(), target, true);
        }
        self.notifications.queue.set_project(project);
        self.notifications
            .queue
            .push_actionable(kind, message, action);
        cx.notify();
    }

    /// Appends one line to Home's activity feed. Callers pass a finished milestone, never a
    /// delta; the feed is what the developer reads after looking away for a minute.
    pub(crate) fn record_activity(
        &mut self,
        kind: ActivityKind,
        summary: impl Into<SharedString>,
        target: Option<FlowTarget>,
        attention: bool,
    ) {
        self.activity.push(ActivityEntry {
            at: std::time::SystemTime::now(),
            kind,
            summary: summary.into(),
            target,
            attention,
        });
    }

    pub fn choose_workspace(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.core.is_none() {
            self.status = "Picker unavailable in test environment".into();
            cx.notify();
            return;
        }

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
            let _ = this.update(cx, |model, cx| {
                model.open_selected_workspace(path, cx);
            });
        }));
    }

    fn open_selected_workspace(&mut self, path: String, cx: &mut Context<Self>) {
        if !self.canvas_can_leave(cx) {
            self.pending_canvas_workspace = Some(path);
            self.status = "Saving the drawing before changing projects…".into();
            self.select_view(AppView::Canvas, cx);
            return;
        }
        self.pending_canvas_workspace = None;
        let Some(core) = self.core.clone() else {
            return;
        };
        self.canvas_project_switching = true;
        let runtime = core.runtime_handle().clone();
        self.workspace_task = Some(cx.spawn(async move |this, cx| {
            let restored = runtime
                .spawn_blocking(move || load_workspace(core, path))
                .await;
            let _ = this.update(cx, |model, cx| {
                model.finish_workspace_restore(restored.map(|result| result.map(Some)), cx);
            });
        }));
    }

    pub(crate) fn request_canvas_close(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.canvas_can_leave(cx) {
            self.canvas_close_requested = false;
            self.canvas_close_window = None;
            return true;
        }
        self.canvas_close_requested = true;
        self.canvas_close_window = Some(window.window_handle());
        self.select_view(AppView::Canvas, cx);
        false
    }

    pub(crate) fn complete_canvas_transition(&mut self, cx: &mut Context<Self>) {
        if self.canvas_has_pending() {
            return;
        }
        if let Some(path) = self.pending_canvas_workspace.take() {
            self.open_selected_workspace(path, cx);
        }
        if self.canvas_close_requested {
            self.canvas_close_requested = false;
            if let Some(window) = self.canvas_close_window.take() {
                cx.defer(move |cx| {
                    let _ = window.update(cx, |_, window, _| window.remove_window());
                });
            }
        }
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
        // Refuse before reading: at the tab cap with every tab holding unsaved work, the file
        // would be read only to be dropped.
        if !self.workspace.read(cx).can_open(&relative_path) {
            self.push_header_notice(
                HeaderNoticeKind::Warning,
                format!(
                    "{} tabs are open with unsaved changes. Save or close one to open {relative_path}.",
                    crate::workspace::MAX_OPEN_TABS
                ),
                cx,
            );
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
                    let evicted = model.workspace.update(cx, |workspace, _| {
                        workspace
                            .open_tab_evicting(
                                requested_path.clone(),
                                snapshot.content,
                                snapshot.revision,
                            )
                            .1
                    });
                    model.restore_editor_for_active(cx);
                    if let Some(cursor) = cursor {
                        model.apply_workspace_cursor(cursor, cx);
                    }
                    model.select_view(AppView::Workspace, cx);
                    model.status = match evicted {
                        Some(closed) => format!(
                            "File opened: {requested_path} · closed {closed} (tab limit {})",
                            crate::workspace::MAX_OPEN_TABS
                        ),
                        None => format!("File opened: {requested_path}"),
                    }
                    .into();
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
                    model.status = format!("File reloaded: {relative_path}").into();
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
        if self.snapshot_in_flight {
            self.snapshot_pending = Some(preferred_id);
            return;
        }
        self.snapshot_in_flight = true;
        let revision = self.snapshot_revision;
        let selected = self.pulsations.read(cx).active_id();
        self.pulsations.update(cx, |state, _| {
            state.runtime_loading = state.pulsations.is_empty();
            state.error = None;
        });
        self.pulsation_snapshot_task = Some(cx.spawn(async move |this, cx| {
            let result = backend
                .runtime_snapshot(project_id.clone(), selected.clone())
                .await;
            let _ = this.update(cx, |model, cx| {
                model.snapshot_in_flight = false;
                model.lifecycle.finish_poll();
                if model.is_disposed() {
                    return;
                }
                let mut pending = model.snapshot_pending.take();
                let current_selection = model.pulsations.read(cx).active_id();
                if current_selection != selected && pending.is_some() {
                    pending = Some(current_selection.clone());
                }
                let current_project = model
                    .workspace
                    .read(cx)
                    .project_id
                    .as_ref()
                    .map(ToString::to_string);
                if current_project.as_deref() != Some(&project_id)
                    || model.snapshot_revision != revision
                {
                    pending = Some(current_selection.clone());
                } else {
                    match result {
                        Ok(snapshot) => {
                            let preference = if model.pulsations.read(cx).active_id() == selected {
                                preferred_id.as_deref()
                            } else {
                                None
                            };
                            if let Some(id) =
                                model.apply_pulsation_snapshot(project_id, snapshot, preference, cx)
                            {
                                model.refresh_pulsation_artifacts(id, cx);
                            }
                            model.refresh_logs(cx);
                            model.finish_event_resync(cx);
                        }
                        Err(error) => {
                            model.pulsations.update(cx, |state, _| {
                                state.runtime_loading = false;
                                state.error = Some(error.clone());
                            });
                            model.push_header_notice(HeaderNoticeKind::Warning, error, cx);
                        }
                    }
                }
                if let Some(preferred) = pending {
                    model.refresh_pulsations_with_preferred(preferred, cx);
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(crate) fn apply_pulsation_snapshot(
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
        self.snapshot_revision = self.snapshot_revision.saturating_add(1);
        let previous_revision = self.pulsation_read_key(cx);
        let forge_changed = crate::forge::task_runtime_changed(
            &self.pulsations.read(cx).pulsations,
            &snapshot.pulsations,
        );
        let previously_awaiting_hitl = self.pulsations.read(cx).awaiting_hitl;
        let previously_awaiting_failure = self.pulsations.read(cx).awaiting_failure;
        let previous_failure = self.pulsations.read(cx).failure.checkpoint.clone();
        // Status transitions of pulsations already known to the UI become activity lines;
        // the initial load (nothing known yet) stays silent.
        let previous_statuses: HashMap<String, (PulsationStatus, PulsationStage)> = self
            .pulsations
            .read(cx)
            .pulsations
            .iter()
            .map(|pulsation| (pulsation.id.clone(), (pulsation.status, pulsation.stage)))
            .collect();
        let transitions: Vec<(
            String,
            String,
            PulsationStatus,
            PulsationStage,
            Option<String>,
        )> = if previous_statuses.is_empty() {
            Vec::new()
        } else {
            snapshot
                .pulsations
                .iter()
                .filter(|pulsation| {
                    previous_statuses
                        .get(&pulsation.id)
                        .is_some_and(|(status, stage)| {
                            *status != pulsation.status || *stage != pulsation.stage
                        })
                })
                .map(|pulsation| {
                    (
                        pulsation.id.clone(),
                        pulsation.feature_name.clone(),
                        pulsation.status,
                        pulsation.stage,
                        pulsation.terminal_reason.clone(),
                    )
                })
                .collect()
        };
        let active_id = self.pulsations.update(cx, |pulsations, _| {
            pulsations.apply_snapshot(project_id, snapshot, preferred_id)
        });
        if transitions
            .iter()
            .any(|(_, _, status, _, _)| *status == PulsationStatus::Checkpoint)
            && self.active_view != AppView::Agents
        {
            // A checkpoint may be a CLI fallback, which only the agents snapshot carries.
            self.refresh_agents_quiet(cx);
        }
        for (id, feature, status, stage, terminal_reason) in transitions {
            let section = match status {
                PulsationStatus::Checkpoint => PulsationDetailSection::Review,
                PulsationStatus::Done => PulsationDetailSection::Artifacts,
                _ => PulsationDetailSection::Overview,
            };
            self.record_activity(
                ActivityKind::Pulsation,
                format!(
                    "{} · {} · {}",
                    feature.trim(),
                    crate::pipeline_status::step_label(stage),
                    crate::pulsations::status_label(status, terminal_reason.as_deref())
                ),
                Some(FlowTarget::PulsationSection(id, section)),
                matches!(
                    status,
                    PulsationStatus::Failed | PulsationStatus::Checkpoint
                ),
            );
        }
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
        if previous_revision != self.pulsation_read_key(cx) {
            self.refresh_pulsation_usage(cx);
        }
        self.sync_preparation_inputs(cx);
        self.sync_stage_feedback_scope(cx);
        if let Some(active_id) = active_id.clone() {
            self.refresh_backend_override(active_id.clone(), cx);
            self.refresh_cleanup_preview(active_id, cx);
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
        let (awaiting, failure_pulsation_id, scope_notice) = {
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
            // ADR-035 M4: a scope request says what it needs.
            let scope_notice = id.as_ref().and_then(|id| {
                pulsations
                    .failure_checkpoints
                    .iter()
                    .find(|c| &c.pulsation_id == id)
                    .and_then(|c| c.scope_request.as_ref())
                    .filter(|request| request.state == "pending")
                    .map(crate::scope_request_view::scope_request_notice)
            });
            (pulsations.awaiting_failure, id, scope_notice)
        };
        if previously_awaiting_failure != 0 || awaiting == 0 {
            return;
        }
        let Some(pulsation_id) = failure_pulsation_id else {
            return;
        };
        // A scope request asks for a decision, like a review; it is not an error.
        let (kind, message) = match scope_notice {
            Some(notice) if awaiting == 1 => (HeaderNoticeKind::Warning, notice),
            _ if awaiting == 1 => (
                HeaderNoticeKind::Error,
                "1 stage failure waiting on a decision".to_string(),
            ),
            _ => (
                HeaderNoticeKind::Error,
                format!("{awaiting} stage failures waiting on a decision"),
            ),
        };
        self.push_actionable_notice(
            kind,
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
            || self.settings_modal.open
            || self.active_dropdown.is_some()
            || self.notifications.history_open
            || self.quota.popover_open
            || self.agents.read(cx).active_model_picker.is_some()
            || (self.active_view() == AppView::Chat
                && !self.chat_draft_input.read(cx).value().trim().is_empty())
    }

    fn pulsation_read_key(&self, cx: &gpui::App) -> String {
        let state = self.pulsations.read(cx);
        format!(
            "{:?}",
            (
                self.workspace.read(cx).project_id.as_ref(),
                state.active().map(|pulse| (&pulse.id, &pulse.updated_at))
            )
        )
    }

    fn refresh_pulsation_artifacts(&mut self, pulsation_id: String, cx: &mut Context<Self>) {
        self.artifact_read.want(self.pulsation_read_key(cx));
        if self.active_view != AppView::Pulsations {
            return;
        }
        let Some(backend) = self.pulsations_backend.clone() else {
            return;
        };
        let Some(key) = self.artifact_read.begin() else {
            return;
        };
        self.pulsations
            .update(cx, |state, _| state.artifacts_loading = true);
        self.pulsation_artifacts_task = Some(cx.spawn(async move |this, cx| {
            let result = backend.artifacts(pulsation_id.clone()).await;
            let _ = this.update(cx, |model, cx| {
                model.artifact_read.want(model.pulsation_read_key(cx));
                let current = model.artifact_read.finish(&key, result.is_ok());
                if current {
                    model
                        .pulsations
                        .update(cx, |state, _| state.finish_artifacts(&pulsation_id, result));
                } else if let Some(id) = model.pulsations.read(cx).active_id() {
                    model.refresh_pulsation_artifacts(id, cx);
                }
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
        self.backend_read.want(self.pulsation_read_key(cx));
        if self.active_view != AppView::Pulsations {
            return;
        }
        let Some(core) = self.core.clone() else {
            return;
        };
        let Some(key) = self.backend_read.begin() else {
            return;
        };
        let runtime = core.runtime_handle().clone();
        self.backend_override_task = Some(cx.spawn(async move |this, cx| {
            let result = runtime
                .spawn_blocking(move || {
                    core.terminals()
                        .repository()
                        .backend_override_status(&pulsation_id)
                })
                .await;
            let _ = this.update(cx, |model, cx| {
                model.backend_read.want(model.pulsation_read_key(cx));
                let success = matches!(result, Ok(Ok(_)));
                let current = model.backend_read.finish(&key, success);
                if current {
                    if let Ok(Ok(status)) = result {
                        model
                            .pulsations
                            .update(cx, |state, _| state.backend_override = status);
                    }
                } else if let Some(id) = model.pulsations.read(cx).active_id() {
                    model.refresh_backend_override(id, cx);
                }
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
        self.backend_override_mutation_task = Some(cx.spawn(async move |this, cx| {
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
                        model.backend_read.invalidate();
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

    /// Loads the "Leftover files" preview for a failed pulsation: what the
    /// cleanup would remove and what it keeps. A dry run on the workspace, so
    /// it is fetched once per selection (`cleanup_read`) and again only on an
    /// explicit refresh or after the cleanup itself; never for a live pulsation.
    fn refresh_cleanup_preview(&mut self, pulsation_id: String, cx: &mut Context<Self>) {
        let failed = self.pulsations.read(cx).active().is_some_and(|pulse| {
            pulse.id == pulsation_id && pulse.status == PulsationStatus::Failed
        });
        if !failed {
            return;
        }
        self.cleanup_read.want(self.pulsation_read_key(cx));
        if self.active_view != AppView::Pulsations {
            return;
        }
        let Some(backend) = self.pulsations_backend.clone() else {
            return;
        };
        let Some(key) = self.cleanup_read.begin() else {
            return;
        };
        self.pulsations.update(cx, |state, _| {
            state.cleanup.loading = true;
            state.cleanup.error = None;
        });
        self.cleanup_preview_task = Some(cx.spawn(async move |this, cx| {
            let result = backend.cleanup_preview(pulsation_id.clone()).await;
            let _ = this.update(cx, |model, cx| {
                model.cleanup_read.want(model.pulsation_read_key(cx));
                let current = model.cleanup_read.finish(&key, result.is_ok());
                if current {
                    model.pulsations.update(cx, |state, _| {
                        state.cleanup.loading = false;
                        state.cleanup.armed = false;
                        match result {
                            Ok(plan) => {
                                state.cleanup.plan = Some(plan);
                                state.cleanup.error = None;
                            }
                            Err(error) => state.cleanup.error = Some(error),
                        }
                    });
                } else if let Some(id) = model.pulsations.read(cx).active_id() {
                    model
                        .pulsations
                        .update(cx, |state, _| state.cleanup.loading = false);
                    model.refresh_cleanup_preview(id, cx);
                }
                cx.notify();
            });
        }));
    }

    /// The "Refresh" control of the Leftover files card: the preview is
    /// optimistic (external editors do not share the host lock), so the
    /// operator can re-read the workspace before confirming.
    pub(crate) fn reload_cleanup_preview(&mut self, cx: &mut Context<Self>) {
        let Some(pulsation_id) = self.pulsations.read(cx).active_id() else {
            return;
        };
        self.cleanup_read.invalidate();
        self.pulsations
            .update(cx, |state, _| state.cleanup.armed = false);
        self.refresh_cleanup_preview(pulsation_id, cx);
        cx.notify();
    }

    /// Two clicks remove the files a failed pulsation created: the first arms
    /// the control, the second sends the confirmed preview (bound by its
    /// `plan_token`) to the host. Kept files stay in the base review; adopting
    /// the base remains the operator's explicit step.
    pub(crate) fn request_cleanup_failed_files(&mut self, cx: &mut Context<Self>) {
        let Some(pulsation_id) = self.pulsations.read(cx).active_id() else {
            return;
        };
        let request = self.pulsations.update(cx, |state, _| {
            if !state.cleanup.can_remove() {
                return None;
            }
            if !state.cleanup.armed {
                state.cleanup.armed = true;
                state.notice =
                    Some("Confirm again to remove the files this pulsation created.".to_string());
                return None;
            }
            let plan = state.cleanup.plan.as_ref()?;
            state.cleanup.armed = false;
            state.cleanup.busy = true;
            state.cleanup.error = None;
            Some(CleanupFailedPulsationFilesRequest {
                pulsation_id: pulsation_id.clone(),
                expected_plan_token: plan.plan_token.clone(),
            })
        });
        cx.notify();
        let Some(request) = request else {
            return;
        };
        let Some(backend) = self.pulsations_backend.clone() else {
            self.pulsations
                .update(cx, |state, _| state.cleanup.busy = false);
            return;
        };
        self.cleanup_mutation_task = Some(cx.spawn(async move |this, cx| {
            let result = backend.cleanup_files(request).await;
            let _ = this.update(cx, |model, cx| {
                model.pulsations.update(cx, |state, _| state.cleanup.busy = false);
                match result {
                    Ok(report) => {
                        let notice = format!(
                            "{} file(s) removed, {} kept, {} staging dir(s) cleared. Kept files stay in the base review.",
                            report.removed_files.len(),
                            report.kept_files,
                            report.removed_candidate_dirs
                        );
                        model.pulsations.update(cx, |state, _| {
                            state.notice = Some(notice);
                        });
                        model.cleanup_read.invalidate();
                        model.refresh_pulsations(cx);
                        model.refresh_cleanup_preview(pulsation_id, cx);
                        model.schedule_vault_refresh(cx);
                    }
                    Err(error) => {
                        model.pulsations.update(cx, |state, _| {
                            state.cleanup.error = Some(error.clone());
                        });
                        model.push_header_notice(HeaderNoticeKind::Error, error, cx);
                    }
                }
                cx.notify();
            });
        }));
    }

    /// Writes the active pulsation's report and hands it to the system's
    /// default application for HTML (ADR-027 M6). The file lives under the
    /// data directory and is rewritten on each request.
    pub(crate) fn open_pulsation_report(&mut self, cx: &mut Context<Self>) {
        self.request_pulsation_report(None, cx);
    }

    /// Asks where to keep a copy of the active pulsation's report, then writes
    /// the report afresh and copies it there. The copy is the developer's; the
    /// app never touches it again.
    pub(crate) fn save_pulsation_report(&mut self, cx: &mut Context<Self>) {
        let state = self.pulsations.read(cx);
        if state.report.busy {
            return;
        }
        let Some(pulsation_id) = state
            .active()
            .filter(|pulsation| report_available(pulsation))
            .map(|pulsation| pulsation.id.clone())
        else {
            return;
        };
        let suggested = format!(
            "pulsation-report-{}.html",
            pulsation_id.chars().take(8).collect::<String>()
        );
        let chosen = cx.prompt_for_new_path(&report_save_directory(), Some(&suggested));
        self.report_task = Some(cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(target))) = chosen.await else {
                return;
            };
            let _ = this.update(cx, |model, cx| {
                // The developer may have moved to another pulsation meanwhile.
                if model.pulsations.read(cx).active_id().as_deref() == Some(pulsation_id.as_str()) {
                    model.request_pulsation_report(Some(target), cx);
                }
            });
        }));
    }

    fn request_pulsation_report(&mut self, copy_to: Option<PathBuf>, cx: &mut Context<Self>) {
        let Some(backend) = self.pulsations_backend.clone() else {
            return;
        };
        let pulsation_id = self.pulsations.update(cx, |state, _| {
            if state.report.busy {
                return None;
            }
            let id = state
                .active()
                .filter(|pulsation| report_available(pulsation))?
                .id
                .clone();
            state.report.busy = true;
            state.report.notice = None;
            Some(id)
        });
        let Some(pulsation_id) = pulsation_id else {
            return;
        };
        cx.notify();
        let saving = copy_to.is_some();
        self.report_task = Some(cx.spawn(async move |this, cx| {
            let result = backend.write_report(pulsation_id.clone(), copy_to).await;
            let _ = this.update(cx, |model, cx| {
                let notice = match result {
                    Ok(path) if saving => ReportNotice::Saved(path),
                    Ok(path) => {
                        let opener = Rc::clone(&model.report_opener);
                        opener(&path, cx);
                        ReportNotice::Opened(path)
                    }
                    Err(error) => ReportNotice::Failed(error),
                };
                model.pulsations.update(cx, |state, _| {
                    // Leaving the pulsation already reset its card.
                    if state.active_id().as_deref() == Some(pulsation_id.as_str()) {
                        state.report.busy = false;
                        state.report.notice = Some(notice);
                    }
                });
                cx.notify();
            });
        }));
    }

    pub(crate) fn refresh_hitl_documents(&mut self, cx: &mut Context<Self>) {
        if self.active_view != AppView::Pulsations {
            return;
        }
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
            // The brief is a derived view of the same checkpoint: read it only
            // when the documents are there, and apply both together so the
            // reader never flashes a document before switching to the brief.
            let brief = if result.is_ok() {
                Some(backend.review_brief(checkpoint.clone()).await)
            } else {
                None
            };
            // The Visualizer's note only exists next to a brief.
            let visualization = if matches!(brief, Some(Ok(Some(_)))) {
                Some(backend.checkpoint_visualization(checkpoint.clone()).await)
            } else {
                None
            };
            // ADR-032: what the Integrator reviewed and the round count. A
            // legacy pulsation has no brief but still gets its notice.
            let context = if brief.is_some() {
                Some((
                    backend.review_inputs(checkpoint.clone()).await,
                    backend.convergence(checkpoint.pulsation_id.clone()).await,
                    // ADR-034 M3: the review's risk level and coverage.
                    backend.risk_status(checkpoint.pulsation_id.clone()).await,
                    // ADR-035 M1: what approving the Specification grants.
                    backend
                        .mandate_preview(checkpoint.checkpoint_id.clone())
                        .await,
                ))
            } else {
                None
            };
            let _ = this.update(cx, |model, cx| {
                if model.workspace.read(cx).project_id != project_id {
                    return;
                }
                model.pulsations.update(cx, |state, _| {
                    if state.hitl.finish_documents(generation, &checkpoint, result)
                        && let Some(brief) = brief
                    {
                        state.hitl.finish_brief(generation, &checkpoint, brief);
                        if let Some(visualization) = visualization {
                            state
                                .hitl
                                .finish_visualization(generation, &checkpoint, visualization);
                        }
                        if let Some((review_inputs, convergence, risk, grants)) = context {
                            state.hitl.finish_review_context(
                                generation,
                                &checkpoint,
                                review_inputs,
                                convergence,
                            );
                            state.hitl.finish_risk(generation, &checkpoint, risk);
                            state.hitl.finish_grants(generation, &checkpoint, grants);
                        }
                    }
                });
                model.schedule_visualizer_poll(cx);
                cx.notify();
            });
        }));
    }

    /// While the Visualizer is writing the open checkpoint's note, reads it
    /// again every few seconds; the brief never waits for it.
    pub(crate) fn schedule_visualizer_poll(&mut self, cx: &mut Context<Self>) {
        let Some(checkpoint) = self.pulsations.read(cx).hitl.pending_visualization() else {
            self.visualizer_poll_task = None;
            return;
        };
        let Some(backend) = self.pulsations_backend.clone() else {
            return;
        };
        let timer = cx.background_executor().clone();
        self.visualizer_poll_task = Some(cx.spawn(async move |this, cx| {
            timer.timer(VISUALIZER_POLL_INTERVAL).await;
            let result = backend.checkpoint_visualization(checkpoint.clone()).await;
            let _ = this.update(cx, |model, cx| {
                let applied = model.pulsations.update(cx, |state, _| {
                    state.hitl.update_visualization(&checkpoint, result)
                });
                if applied {
                    model.schedule_visualizer_poll(cx);
                    cx.notify();
                }
            });
        }));
    }

    pub(crate) fn refresh_failure_detail(&mut self, cx: &mut Context<Self>) {
        if self.active_view != AppView::Pulsations {
            return;
        }
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
        let pulsation_id = self
            .pulsations
            .read(cx)
            .failure
            .checkpoint
            .as_ref()
            .map(|checkpoint| checkpoint.pulsation_id.clone());
        self.failure_detail_task = Some(cx.spawn(async move |this, cx| {
            let result = backend.stage_failure_detail(checkpoint_id.clone()).await;
            let convergence = match pulsation_id {
                Some(pulsation_id) => backend.convergence(pulsation_id).await,
                None => Ok(None),
            };
            let _ = this.update(cx, |model, cx| {
                if model.workspace.read(cx).project_id != project_id {
                    return;
                }
                model.pulsations.update(cx, |state, _| {
                    if state
                        .failure
                        .finish_detail(generation, &checkpoint_id, result)
                    {
                        state
                            .failure
                            .finish_convergence(generation, &checkpoint_id, convergence);
                    }
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
        // ADR-032: beyond the round threshold the plain revision is gone;
        // the developer picks Continue or Replan with a reason.
        if decision == StageFailureDecision::RequestRevision
            && self.pulsations.read(cx).failure.beyond_threshold()
        {
            self.pulsations.update(cx, |pulsations, _| {
                pulsations.failure.show_feedback = true;
                pulsations.failure.error = Some(THRESHOLD_CHOICE.into());
            });
            cx.notify();
            return;
        }
        self.resolve_stage_failure_with(decision, None, cx);
    }

    /// A failure-path revision beyond the round threshold (ADR-032): the
    /// guidance text is the decision's reason.
    pub(crate) fn revise_failure_beyond_threshold(&mut self, replan: bool, cx: &mut Context<Self>) {
        let Some(ack) = self.convergence_ack(replan, cx) else {
            self.pulsations.update(cx, |pulsations, _| {
                pulsations.failure.show_feedback = true;
                pulsations.failure.error = Some(REASON_REQUIRED.into());
            });
            cx.notify();
            return;
        };
        self.resolve_stage_failure_with(StageFailureDecision::RequestRevision, Some(ack), cx);
    }

    fn convergence_ack(
        &self,
        replan: bool,
        cx: &gpui::App,
    ) -> Option<ikore_core::persistence_service::ConvergenceAck> {
        let reason = self.stage_feedback_input.read(cx).value().trim().to_owned();
        if reason.is_empty() {
            return None;
        }
        Some(if replan {
            ikore_core::persistence_service::ConvergenceAck::Replan { reason }
        } else {
            ikore_core::persistence_service::ConvergenceAck::ContinueBeyondThreshold { reason }
        })
    }

    fn resolve_stage_failure_with(
        &mut self,
        decision: StageFailureDecision,
        ack: Option<ikore_core::persistence_service::ConvergenceAck>,
        cx: &mut Context<Self>,
    ) {
        self.confirm_failure_abort = false;
        let (request, revision_target) = self.pulsations.update(cx, |pulsations, _| {
            let target = pulsations
                .failure
                .checkpoint
                .as_ref()
                .filter(|c| !c.allowed_revision_targets.is_empty())
                .and_then(|_| pulsations.failure.selected_revision_target());
            (pulsations.failure.begin(decision), target)
        });
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
                        .resolve_stage_failure_with_convergence(request, revision_target, ack)
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

    /// "Grant and retry" on a failure checkpoint's scope request (ADR-035
    /// M4). A request the host finds out of date is refused with nothing
    /// granted; the snapshot is read again either way, so the card shows the
    /// request's state.
    pub(crate) fn grant_scope_and_retry(&mut self, cx: &mut Context<Self>) {
        self.confirm_failure_abort = false;
        let command = self
            .pulsations
            .update(cx, |pulsations, _| pulsations.failure.begin_grant());
        let Some(command) = command else {
            return;
        };
        let Some(core) = self.core.clone() else {
            self.pulsations.update(cx, |pulsations, _| {
                pulsations
                    .failure
                    .finish(Err("Scope grant unavailable in test environment".into()));
            });
            cx.notify();
            return;
        };
        self.status = "Granting the scope request…".into();
        cx.notify();
        let runtime = core.runtime_handle().clone();
        let decision_scope = (
            command.checkpoint_id.clone(),
            command.expected_state_version,
        );
        self.failure_task = Some(cx.spawn(async move |this, cx| {
            let result = runtime
                .spawn_blocking(move || {
                    core.persistence()
                        .grant_scope_and_retry(command)
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                })
                .await
                .map_err(|error| format!("Scope grant worker interrupted: {error}"))
                .and_then(|result| result);
            let _ = this.update(cx, |model, cx| {
                if model.lifecycle.is_disposed() {
                    return;
                }
                model.pulsations.update(cx, |pulsations, _| {
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
                        model.status = "Scope granted; the stage runs again".into();
                        model.schedule_session_persistence(cx);
                    }
                    Err(error) => {
                        model.status = "The scope request was not granted".into();
                        model.push_header_notice(HeaderNoticeKind::Error, error, cx);
                    }
                }
                model.refresh_pulsations(cx);
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
        self.artifact_read.invalidate();
        self.backend_read.invalidate();
        self.cleanup_read.invalidate();
        self.confirm_failure_abort = false;
        self.refresh_hitl_documents(cx);
        self.refresh_failure_detail(cx);
        self.refresh_pulsation_usage(cx);
        self.sync_preparation_inputs(cx);
        self.sync_stage_feedback_scope(cx);
        self.pulsations.update(cx, |pulsations, _| {
            pulsations.diff_expanded = false;
        });
        self.refresh_pulsation_artifacts(pulsation_id.clone(), cx);
        self.refresh_backend_override(pulsation_id.clone(), cx);
        self.refresh_cleanup_preview(pulsation_id, cx);
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
                    .runtime_snapshot(project_id.clone(), Some(created.id.clone()))
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
                    .runtime_snapshot(project_id.clone(), Some(pulsation_id.clone()))
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
        // The indicator follows the Vault: same moments, its own query.
        self.refresh_doc_drift(cx);
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
                if let Ok(nodes) = &tree {
                    // The Workspace explorer lists the same files; keep it in step.
                    let nodes = nodes.clone();
                    model
                        .workspace
                        .update(cx, |workspace, _| workspace.tree = nodes);
                }
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

    /// Workspace changes always invalidate the Vault model, but filesystem/index work is only
    /// useful while its surface is visible. Opening Vault consumes the retained `stale` marker.
    fn mark_vault_stale(&mut self, cx: &mut Context<Self>) {
        if self.vault.read(cx).workspace_root.is_none() {
            return;
        }
        self.vault.update(cx, |vault, _| vault.stale = true);
        if matches!(self.active_view, AppView::Vault | AppView::Workspace) {
            self.schedule_vault_refresh(cx);
        } else {
            cx.notify();
        }
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

    /// Opens or closes one folder of the Workspace explorer.
    pub(crate) fn toggle_workspace_folder(&mut self, relative_path: &str, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, _| workspace.folders.toggle(relative_path));
        cx.notify();
    }

    /// Opens or closes one folder of the Vault explorer.
    pub(crate) fn toggle_vault_folder(&mut self, relative_path: &str, cx: &mut Context<Self>) {
        self.vault
            .update(cx, |vault, _| vault.folders.toggle(relative_path));
        cx.notify();
    }

    /// Hand tool: a press on the graph background starts panning; a press on a node does not
    /// (the node's own click selects it).
    pub(crate) fn vault_graph_press(
        &mut self,
        position: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        let start = {
            let vault = self.vault.read(cx);
            let Some((transform, nodes)) = vault.camera.last.get() else {
                return;
            };
            let bounds = vault.camera.bounds.get();
            let local = (
                f32::from(position.x - bounds.left()),
                f32::from(position.y - bounds.top()),
            );
            let layout = vault.graph_layout(vault.selected_path.as_ref().map(AsRef::as_ref));
            let reach = crate::vault::graph_node_radius(transform.zoom) + 6.0;
            let on_node = layout.nodes.iter().any(|node| {
                let (x, y) = transform.to_panel(node.layout.x, node.layout.y);
                (x - local.0).hypot(y - local.1) <= reach
            });
            (!on_node).then_some((local, transform, nodes))
        };
        if let Some((local, transform, nodes)) = start {
            self.vault.update(cx, |vault, _| {
                vault.camera.adjusted = Some((transform, nodes));
                vault.camera.drag = Some((local, (transform.pan_x, transform.pan_y)));
            });
            cx.notify();
        }
    }

    pub(crate) fn vault_graph_drag(
        &mut self,
        position: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        let moved = self.vault.update(cx, |vault, _| {
            let ((start_x, start_y), (pan_x, pan_y)) = vault.camera.drag?;
            let (mut transform, nodes) = vault.camera.adjusted?;
            let bounds = vault.camera.bounds.get();
            transform.pan_x = pan_x + f32::from(position.x - bounds.left()) - start_x;
            transform.pan_y = pan_y + f32::from(position.y - bounds.top()) - start_y;
            vault.camera.adjusted = Some((transform, nodes));
            Some(())
        });
        if moved.is_some() {
            cx.notify();
        }
    }

    /// Pointer entered or left a node's dot.
    pub(crate) fn vault_graph_hover(
        &mut self,
        path: String,
        hovered: bool,
        cx: &mut Context<Self>,
    ) {
        let changed = self.vault.update(cx, |vault, _| {
            let camera = &mut vault.camera;
            let next = if hovered {
                Some(path)
            } else if camera.hovered.as_deref() == Some(path.as_str()) {
                None
            } else {
                return false;
            };
            let changed = camera.hovered != next;
            camera.hovered = next;
            changed
        });
        if changed {
            cx.notify();
        }
    }

    pub(crate) fn vault_graph_release(&mut self, cx: &mut Context<Self>) {
        if self.vault.read(cx).camera.drag.is_some() {
            self.vault.update(cx, |vault, _| vault.camera.drag = None);
            cx.notify();
        }
    }

    /// Zooms by `factor` around `anchor` (window coordinates) or the panel centre.
    pub(crate) fn vault_graph_zoom(
        &mut self,
        factor: f32,
        anchor: Option<gpui::Point<gpui::Pixels>>,
        cx: &mut Context<Self>,
    ) {
        self.vault.update(cx, |vault, _| {
            let Some((base, nodes)) = vault.camera.last.get() else {
                return;
            };
            let base = vault
                .camera
                .adjusted
                .filter(|(_, count)| *count == nodes)
                .map_or(base, |(transform, _)| transform);
            let bounds = vault.camera.bounds.get();
            let (anchor_x, anchor_y) = match anchor {
                Some(point) => (
                    f32::from(point.x - bounds.left()),
                    f32::from(point.y - bounds.top()),
                ),
                None => (
                    f32::from(bounds.size.width) / 2.0,
                    f32::from(bounds.size.height) / 2.0,
                ),
            };
            vault.camera.adjusted = Some((base.zoomed(factor, anchor_x, anchor_y), nodes));
        });
        cx.notify();
    }

    /// Back to the automatic fit.
    pub(crate) fn vault_graph_fit(&mut self, cx: &mut Context<Self>) {
        self.vault.update(cx, |vault, _| {
            vault.camera.adjusted = None;
            vault.camera.drag = None;
        });
        cx.notify();
    }

    /// Switches the Workspace between the editor and the rendered Markdown. The editor keeps its
    /// buffer, caret and scroll while the preview shows.
    pub(crate) fn set_workspace_preview(&mut self, preview: bool, cx: &mut Context<Self>) {
        if preview {
            // The preview renders the tab's copy of the text; bring it up to date first.
            self.sync_active_editor(cx);
        }
        self.workspace
            .update(cx, |workspace, _| workspace.preview_mode = preview);
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
                .rename(ikore_core::workspace_service::RenameWorkspaceNoteRequest {
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

    /// The checkpoint the brief notes belong to.
    fn brief_scope(&self, cx: &gpui::App) -> Option<(String, u64)> {
        self.pulsations
            .read(cx)
            .hitl
            .expected_checkpoint
            .as_ref()
            .map(|checkpoint| (checkpoint.checkpoint_id.clone(), checkpoint.state_version))
    }

    /// The note input of brief item `index`, created when the item is first
    /// flagged. Notes of another checkpoint or version are dropped first.
    fn brief_note_input(
        &mut self,
        index: usize,
        cx: &mut Context<Self>,
    ) -> Entity<NativeTextInput> {
        let scope = self.brief_scope(cx);
        if self.brief_notes_scope != scope {
            self.brief_notes.clear();
            self.brief_notes_scope = scope;
        }
        if let Some(input) = self.brief_notes.get(&index) {
            return input.clone();
        }
        let input = cx.new(|cx| NativeTextInput::new("What should change? (optional)", cx));
        cx.observe(&input, |_, _, cx| cx.notify()).detach();
        self.brief_notes.insert(index, input.clone());
        input
    }

    /// Notes of the current checkpoint only, by item index.
    pub(crate) fn current_brief_notes(
        &self,
        cx: &gpui::App,
    ) -> HashMap<usize, Entity<NativeTextInput>> {
        if self.brief_notes_scope == self.brief_scope(cx) {
            self.brief_notes.clone()
        } else {
            HashMap::new()
        }
    }

    /// Confirms or flags brief item `index` (ADR-024 M2); the same mark again
    /// reopens it. A flag set with the mouse focuses its note; otherwise the
    /// focus stays on the review so C/F/Tab keep working.
    pub(crate) fn toggle_brief_mark(
        &mut self,
        index: usize,
        mark: crate::hitl::ItemMark,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        let marked = self
            .pulsations
            .update(cx, |pulsations, _| pulsations.hitl.toggle_mark(index, mark));
        match (marked, window) {
            (Some(crate::hitl::ItemMark::Flagged), Some(window)) => {
                let input = self.brief_note_input(index, cx);
                gpui::Focusable::focus_handle(input.read(cx), cx).focus(window);
            }
            (Some(crate::hitl::ItemMark::Flagged), None) => {
                self.brief_note_input(index, cx);
            }
            (Some(_), Some(window)) => self.review_focus.focus(window),
            _ => {}
        }
        cx.notify();
    }

    pub(crate) fn move_brief_cursor(&mut self, forward: bool, cx: &mut Context<Self>) {
        self.pulsations.update(cx, |pulsations, _| {
            pulsations.hitl.move_brief_cursor(forward)
        });
        cx.notify();
    }

    /// The Request revision feedback: flagged brief items with their notes,
    /// then the free-text feedback. Without flags the free text goes out as
    /// typed, exactly as before the brief existed.
    fn revision_feedback(&self, cx: &gpui::App) -> String {
        let free_text = self.stage_feedback_input.read(cx).value().to_owned();
        let hitl = &self.pulsations.read(cx).hitl;
        let Some(brief) = hitl.brief.as_ref().filter(|_| hitl.flag_count() > 0) else {
            return free_text;
        };
        let notes = self.current_brief_notes(cx);
        crate::hitl::compose_revision_feedback(
            &brief.judgement,
            &hitl.marks,
            |index| {
                notes
                    .get(&index)
                    .map(|input| input.read(cx).value().to_owned())
            },
            &free_text,
        )
    }

    pub(crate) fn apply_hitl_decision(&mut self, decision: HitlDecision, cx: &mut Context<Self>) {
        // ADR-032: beyond the round threshold the plain revision is gone;
        // the developer picks Continue or Replan with a reason.
        if decision == HitlDecision::RequestRevision
            && self.pulsations.read(cx).hitl.beyond_threshold()
        {
            self.pulsations.update(cx, |pulsations, _| {
                pulsations.hitl.show_feedback = true;
                pulsations.hitl.error = Some(THRESHOLD_CHOICE.into());
            });
            cx.notify();
            return;
        }
        // ADR-034 M3: approving a high-risk review with unverified or unrun
        // items needs a reason; the feedback text is it.
        if decision == HitlDecision::Approve
            && self
                .pulsations
                .read(cx)
                .hitl
                .risk_acknowledgement_required()
        {
            let reason = self.stage_feedback_input.read(cx).value().trim().to_owned();
            if reason.is_empty() {
                self.pulsations.update(cx, |pulsations, _| {
                    pulsations.hitl.show_feedback = true;
                    pulsations.hitl.error = Some(RISK_REASON_REQUIRED.into());
                });
                cx.notify();
                return;
            }
            self.apply_hitl_decision_with(
                decision,
                None,
                Some(ikore_core::persistence_service::RiskAcknowledgement { reason }),
                cx,
            );
            return;
        }
        self.apply_hitl_decision_with(decision, None, None, cx);
    }

    /// A checkpoint revision beyond the round threshold (ADR-032): the
    /// free-text feedback is the decision's reason.
    pub(crate) fn revise_beyond_threshold(&mut self, replan: bool, cx: &mut Context<Self>) {
        let Some(ack) = self.convergence_ack(replan, cx) else {
            self.pulsations.update(cx, |pulsations, _| {
                pulsations.hitl.show_feedback = true;
                pulsations.hitl.error = Some(REASON_REQUIRED.into());
            });
            cx.notify();
            return;
        };
        self.apply_hitl_decision_with(HitlDecision::RequestRevision, Some(ack), None, cx);
    }

    fn apply_hitl_decision_with(
        &mut self,
        decision: HitlDecision,
        ack: Option<ikore_core::persistence_service::ConvergenceAck>,
        risk: Option<ikore_core::persistence_service::RiskAcknowledgement>,
        cx: &mut Context<Self>,
    ) {
        self.confirm_hitl_reject = false;
        self.confirm_stage_reject = false;
        self.confirm_failure_abort = false;
        let request = self
            .pulsations
            .update(cx, |pulsations, _| pulsations.hitl.begin(decision));
        let request = request.map(|mut request| {
            if decision == HitlDecision::RequestRevision {
                request.feedback = Some(self.revision_feedback(cx));
            }
            request
        });
        let Some(request) = request else {
            return;
        };
        #[cfg(test)]
        {
            self.last_revision_feedback = request.feedback.clone();
        }
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
                        .apply_hitl_decision_with_acknowledgements(request, ack, risk)
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
        let (generation, selection, scroll_top) = {
            let editor = self.workspace.read(cx).editor.read(cx);
            (
                editor.edit_generation(),
                editor.buffer.selection(),
                editor.buffer.scroll_top(),
            )
        };
        if generation == self.synced_edit_generation {
            // No edit since the last sync: keep the caret and scroll for tab restore without
            // copying the document.
            self.workspace.update(cx, |workspace, _| {
                workspace.sync_active_cursor(selection, scroll_top)
            });
            return;
        }
        self.synced_edit_generation = generation;
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
        // A restore resets the editor's generation to 0 and loads exactly the tab's content.
        self.synced_edit_generation = 0;
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

    pub(crate) fn chat_composer_for(
        &self,
        conversation_id: &str,
    ) -> Option<Entity<NativeTextInput>> {
        self.chat_composer_inputs.get(conversation_id).cloned()
    }

    pub(crate) fn ensure_chat_composer(
        &mut self,
        conversation_id: &str,
        cx: &mut Context<Self>,
    ) -> Entity<NativeTextInput> {
        let input = if let Some(input) = self.chat_composer_inputs.get(conversation_id) {
            input.clone()
        } else {
            let input = if self.chat_composer_inputs.is_empty() {
                self.chat_initial_input.clone()
            } else {
                let input =
                    cx.new(|cx| NativeTextInput::new("Message to Companion", cx).multiline());
                let subscriptions = vec![
                    cx.observe(&input, |model, input, cx| {
                        model.chat_composer_changed(input, cx)
                    }),
                    cx.subscribe(&input, |model, input, _: &crate::text_input::Submit, cx| {
                        model.send_chat_composer_input(input, cx)
                    }),
                ];
                self.chat_composer_subscriptions
                    .insert(conversation_id.into(), subscriptions);
                input
            };
            self.chat_composer_inputs
                .insert(conversation_id.into(), input.clone());
            input
        };
        if self
            .chat
            .read(cx)
            .active_conversation
            .as_ref()
            .map(AsRef::as_ref)
            == Some(conversation_id)
        {
            self.chat_draft_input = input.clone();
        }
        if let Some(restored) = self.companion_dock.restored_drafts.remove(conversation_id)
            && input.read(cx).value().is_empty()
        {
            input.update(cx, |input, cx| input.set_value(restored, cx));
        }
        input
    }

    fn conversation_for_chat_composer(
        &self,
        input: &Entity<NativeTextInput>,
        cx: &gpui::App,
    ) -> Option<String> {
        self.chat_composer_inputs
            .iter()
            .find(|(_, composer)| composer.entity_id() == input.entity_id())
            .map(|(id, _)| id.clone())
            .or_else(|| {
                (self.chat_composer_inputs.is_empty()
                    && self.chat_initial_input.entity_id() == input.entity_id())
                .then(|| {
                    self.chat
                        .read(cx)
                        .active_conversation
                        .as_ref()
                        .map(ToString::to_string)
                })
                .flatten()
            })
    }

    fn chat_composer_changed(&mut self, input: Entity<NativeTextInput>, cx: &mut Context<Self>) {
        if let Some(conversation_id) = self.conversation_for_chat_composer(&input, cx) {
            let content = input.read(cx).value().to_owned();
            self.companion_draft_changed(conversation_id, content, cx);
        }
        cx.notify();
    }

    fn send_chat_composer_input(&mut self, input: Entity<NativeTextInput>, cx: &mut Context<Self>) {
        if let Some(conversation_id) = self.conversation_for_chat_composer(&input, cx) {
            let content = input.read(cx).value().to_owned();
            self.send_chat_message_to(conversation_id, content, cx);
        }
    }

    fn hydrate_chat_project(
        &mut self,
        project_id: String,
        restored_conversation_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let changed_project =
            self.chat.read(cx).project_id.as_ref().map(AsRef::as_ref) != Some(project_id.as_str());
        if changed_project {
            self.stage_companion_inputs(cx);
            self.persist_companion_immediately(cx);
            self.chat_tasks.clear();
            self.chat_create_task.take();
            self.chat_target_task.take();
            self.chat_resync_tasks.clear();
            self.chat_composer_subscriptions.clear();
            self.chat_composer_inputs.clear();
            self.chat_draft_input = self.chat_initial_input.clone();
            self.chat_draft_input
                .update(cx, |input, cx| input.clear(cx));
        }
        let generation = self.chat.update(cx, |chat, _| {
            chat.begin_hydration(project_id.clone(), restored_conversation_id.clone())
        });
        let Some(backend) = self.chat_backend.clone() else {
            self.chat
                .update(cx, |chat, _| chat.finish_hydration_without_backend());
            return;
        };
        self.chat_hydration_task = Some(cx.spawn(async move |this, cx| {
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
                    if let Some(id) = model
                        .chat
                        .read(cx)
                        .active_conversation
                        .as_ref()
                        .map(ToString::to_string)
                    {
                        model.ensure_chat_composer(&id, cx);
                    }
                    model.schedule_session_persistence(cx);
                    model.refresh_canvas_authoring(cx);
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
        if !self.chat.update(cx, |chat, _| {
            chat.select_conversation_locally(&conversation_id)
        }) {
            return;
        }
        self.ensure_chat_composer(&conversation_id, cx);
        self.refresh_canvas_authoring(cx);
        self.schedule_session_persistence(cx);
        cx.notify();
        self.load_chat_conversation(conversation_id, cx);
    }

    pub(crate) fn load_chat_conversation(
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
        let Some(generation) = self.chat.update(cx, |chat, _| {
            chat.begin_operation(&conversation_id, ChatOperation::Load)
        }) else {
            return;
        };
        let task_key = (conversation_id.clone(), ChatOperation::Load);
        let task = cx.spawn(async move |this, cx| {
            let result = backend.load_conversation(conversation_id.clone()).await;
            let _ = this.update(cx, |model, cx| {
                if !model.chat.update(cx, |chat, _| {
                    chat.finish_operation(
                        &project_id,
                        &conversation_id,
                        ChatOperation::Load,
                        generation,
                    )
                }) {
                    return;
                }
                match result {
                    Ok(snapshot) => {
                        model.chat.update(cx, |chat, _| {
                            chat.apply_selected_conversation(
                                &project_id,
                                &conversation_id,
                                snapshot,
                            );
                        });
                        model.refresh_canvas_authoring(cx);
                    }
                    Err(error) => model.chat.update(cx, |chat, _| {
                        chat.set_conversation_error(&conversation_id, error)
                    }),
                }
                cx.notify();
            });
        });
        self.chat_tasks.insert(task_key, task);
    }

    pub(crate) fn set_chat_transport(
        &mut self,
        transport: ChatTransportKind,
        cx: &mut Context<Self>,
    ) {
        if self.chat.read(cx).target_change_pending {
            return;
        }
        self.chat.update(cx, |chat, _| {
            chat.selected_transport = transport;
        });
        self.close_cli_model_picker(cx);
        self.close_dropdown(cx);
        self.apply_chat_target_change(cx);
    }

    pub(crate) fn set_chat_cli_backend(&mut self, backend: CliBackend, cx: &mut Context<Self>) {
        if self.chat.read(cx).target_change_pending {
            return;
        }
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
        self.apply_chat_target_change(cx);
    }

    pub(crate) fn set_chat_cli_profile(&mut self, profile: Option<String>, cx: &mut Context<Self>) {
        if self.chat.read(cx).target_change_pending {
            return;
        }
        self.chat.update(cx, |chat, _| {
            chat.selected_cli_model_profile = profile;
        });
        self.close_dropdown(cx);
        self.apply_chat_target_change(cx);
    }

    pub(crate) fn set_chat_connection(&mut self, connection_id: String, cx: &mut Context<Self>) {
        if self.chat.read(cx).target_change_pending {
            return;
        }
        self.chat.update(cx, |chat, _| {
            chat.selected_connection_id = Some(connection_id.into());
        });
        self.close_dropdown(cx);
        self.apply_chat_target_change(cx);
    }

    /// The Companion panel applies to the open conversation from its next
    /// message and is remembered per project (ADR-014 amendment,
    /// 2026-09-22). Without an open conversation it is only remembered. A
    /// refused change (a reply still running, an invalid model) puts the
    /// panel back on the conversation's real target.
    fn apply_chat_target_change(&mut self, cx: &mut Context<Self>) {
        let Some(backend) = self.chat_backend.clone() else {
            return;
        };
        let (project_id, conversation_id, target) = {
            let chat = self.chat.read(cx);
            let Some(project_id) = chat.project_id.as_ref().map(ToString::to_string) else {
                return;
            };
            let Some(target) = chat.panel_target() else {
                return;
            };
            (
                project_id,
                chat.active_conversation.as_ref().map(ToString::to_string),
                target,
            )
        };
        let generation = self.chat.update(cx, |chat, _| {
            chat.target_change_generation = chat.target_change_generation.saturating_add(1);
            chat.target_change_pending = true;
            chat.target_change_generation
        });
        cx.notify();
        self.chat_target_task = Some(cx.spawn(async move |this, cx| match conversation_id {
            Some(conversation_id) => {
                let result = backend
                    .update_conversation_target(conversation_id.clone(), target)
                    .await;
                let _ = this.update(cx, |model, cx| {
                    model.chat.update(cx, |chat, _| match result {
                        _ if chat.target_change_generation != generation => (),
                        Ok(snapshot)
                            if chat.active_conversation.as_ref().map(AsRef::as_ref)
                                == Some(conversation_id.as_str()) =>
                        {
                            chat.target_change_pending = false;
                            chat.apply_retargeted(snapshot)
                        }
                        Ok(snapshot) => {
                            chat.target_change_pending = false;
                            chat.store_conversation_snapshot(snapshot)
                        }
                        Err(error) => {
                            chat.target_change_pending = false;
                            chat.sync_panel_with_active_conversation();
                            chat.set_error(error);
                        }
                    });
                    cx.notify();
                });
            }
            None => {
                let result = backend.save_default_target(project_id, target).await;
                let _ = this.update(cx, |model, cx| {
                    model.chat.update(cx, |chat, _| {
                        if chat.target_change_generation != generation {
                            return;
                        }
                        chat.target_change_pending = false;
                        if let Err(error) = result {
                            chat.set_error(error);
                        }
                    });
                    cx.notify();
                });
            }
        }));
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
            let Some(target) = chat.panel_target() else {
                return;
            };
            (project_id, target)
        };
        if !self.chat.read(cx).new_conversation_target_available() {
            return;
        }
        if !self.chat.update(cx, |chat, _| chat.begin_busy()) {
            return;
        }
        let Some(generation) = self.chat.update(cx, |chat, _| {
            chat.begin_operation("", ChatOperation::Create)
        }) else {
            return;
        };
        cx.notify();
        self.chat_create_task = Some(cx.spawn(async move |this, cx| {
            let result = backend
                .create_conversation(project_id.clone(), target)
                .await;
            let _ = this.update(cx, |model, cx| {
                if !model.chat.update(cx, |chat, _| {
                    chat.finish_operation(&project_id, "", ChatOperation::Create, generation)
                }) {
                    return;
                }
                let created_id = result
                    .as_ref()
                    .ok()
                    .map(|snapshot| snapshot.conversation.id.clone());
                model.chat.update(cx, |chat, _| {
                    chat.finish_busy();
                    match result {
                        Ok(snapshot) => chat.apply_created(snapshot),
                        Err(error) => chat.set_error(error),
                    }
                });
                if let Some(id) = model
                    .chat
                    .read(cx)
                    .active_conversation
                    .as_ref()
                    .map(ToString::to_string)
                {
                    model.ensure_chat_composer(&id, cx);
                }
                model.companion_chat_creation_finished(created_id, cx);
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
        let Some(project_id) = self
            .chat
            .read(cx)
            .project_id
            .as_ref()
            .map(ToString::to_string)
        else {
            return;
        };
        let Some(generation) = self.chat.update(cx, |chat, _| {
            chat.begin_operation(&conversation_id, ChatOperation::Delete)
        }) else {
            return;
        };
        cx.notify();
        let task_key = (conversation_id.clone(), ChatOperation::Delete);
        let task = cx.spawn(async move |this, cx| {
            let result = backend.delete_conversation(conversation_id.clone()).await;
            let _ = this.update(cx, |model, cx| {
                if !model.chat.update(cx, |chat, _| {
                    chat.finish_operation(
                        &project_id,
                        &conversation_id,
                        ChatOperation::Delete,
                        generation,
                    )
                }) {
                    return;
                }
                if matches!(result, Ok(true)) {
                    model.chat_composer_inputs.remove(&conversation_id);
                    model.chat_composer_subscriptions.remove(&conversation_id);
                }
                let next = model.chat.update(cx, |chat, _| match result {
                    Ok(true) => chat.apply_deleted(&conversation_id),
                    Ok(false) => {
                        chat.set_conversation_error(
                            &conversation_id,
                            crate::errors::UiCommandError::from_chat_parts(
                                "conversation_not_found",
                                "Conversation not found.",
                            ),
                        );
                        None
                    }
                    Err(error) => {
                        chat.set_conversation_error(&conversation_id, error);
                        None
                    }
                });
                if let Some(next) = next {
                    model.select_chat_conversation(next, cx);
                }
                model.schedule_session_persistence(cx);
                cx.notify();
            });
        });
        self.chat_tasks.insert(task_key, task);
    }

    pub(crate) fn acknowledge_chat_disclosure(&mut self, cx: &mut Context<Self>) {
        self.chat
            .update(cx, |chat, _| chat.acknowledge_disclosure());
        cx.notify();
    }

    pub(crate) fn send_chat_message(&mut self, cx: &mut Context<Self>) {
        let Some(conversation_id) = self
            .chat
            .read(cx)
            .active_conversation
            .as_ref()
            .map(ToString::to_string)
        else {
            return;
        };
        let content = self.chat_draft_input.read(cx).value().to_owned();
        self.send_chat_message_to(conversation_id, content, cx);
    }

    pub(crate) fn send_chat_message_to(
        &mut self,
        conversation_id: String,
        content: String,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.chat_backend.clone() else {
            return;
        };
        let (project_id, expected_target) = {
            let chat = self.chat.read(cx);
            if content.trim().is_empty()
                || !chat.composer_available_for(&conversation_id)
                || !chat.disclosure_accepted_for(&conversation_id)
                || chat.is_streaming(&conversation_id)
                || chat.is_busy(&conversation_id)
            {
                return;
            }
            (
                chat.project_id.as_ref().map(ToString::to_string),
                chat.conversation_target(&conversation_id),
            )
        };
        let Some(project_id) = project_id else {
            return;
        };
        let Some(generation) = self.chat.update(cx, |chat, _| {
            chat.begin_operation(&conversation_id, ChatOperation::Send)
        }) else {
            return;
        };
        let input = self.ensure_chat_composer(&conversation_id, cx);
        cx.notify();
        let task_key = (conversation_id.clone(), ChatOperation::Send);
        let task = cx.spawn(async move |this, cx| {
            let result = backend
                .send_message(
                    conversation_id.clone(),
                    content.trim().to_owned(),
                    expected_target,
                )
                .await;
            let _ = this.update(cx, |model, cx| {
                let accepted = model.chat.update(cx, |chat, _| {
                    if !chat.finish_operation(
                        &project_id,
                        &conversation_id,
                        ChatOperation::Send,
                        generation,
                    ) {
                        return false;
                    }
                    match result {
                        Ok((started, snapshot)) => chat.apply_started_snapshot(
                            &conversation_id,
                            &started.generation_id,
                            snapshot,
                        ),
                        Err(error) => {
                            chat.set_conversation_error(&conversation_id, error);
                            false
                        }
                    }
                });
                if accepted && input.read(cx).value() == content {
                    input.update(cx, |input, cx| input.clear(cx));
                    model.companion_draft_changed(conversation_id.clone(), String::new(), cx);
                }
                model.schedule_session_persistence(cx);
                cx.notify();
            });
        });
        self.chat_tasks.insert(task_key, task);
    }

    pub(crate) fn cancel_chat_turn(&mut self, cx: &mut Context<Self>) {
        if let Some(conversation_id) = self
            .chat
            .read(cx)
            .active_conversation
            .as_ref()
            .map(ToString::to_string)
        {
            self.cancel_chat_turn_for(conversation_id, cx);
        }
    }

    pub(crate) fn cancel_chat_turn_for(&mut self, conversation_id: String, cx: &mut Context<Self>) {
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
        let Some(execution_id) = self.chat.read(cx).execution_id_for(&conversation_id) else {
            return;
        };
        let Some(generation) = self.chat.update(cx, |chat, _| {
            chat.begin_operation(&conversation_id, ChatOperation::Cancel)
        }) else {
            return;
        };
        let task_key = (conversation_id.clone(), ChatOperation::Cancel);
        let task = cx.spawn(async move |this, cx| {
            let result = backend.cancel_turn(execution_id).await;
            let _ = this.update(cx, |model, cx| {
                model.chat.update(cx, |chat, _| {
                    if chat.finish_operation(
                        &project_id,
                        &conversation_id,
                        ChatOperation::Cancel,
                        generation,
                    ) && let Err(error) = result
                    {
                        chat.set_conversation_error(&conversation_id, error);
                    }
                });
                cx.notify();
            });
        });
        self.chat_tasks.insert(task_key, task);
    }

    pub(crate) fn copy_chat_diff(&mut self, diff: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(diff));
        self.status = "Diff copied; no changes were applied".into();
        cx.notify();
    }

    fn handle_chat_event(&mut self, envelope: ChatEventEnvelope, cx: &mut Context<Self>) {
        let completed = envelope.kind == ikore_core::chat_service::ChatEventKind::TurnCompleted;
        let conversation_id = envelope.conversation_id.clone();
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
        if completed
            && self
                .chat
                .read(cx)
                .active_conversation
                .as_ref()
                .map(|id| id.as_ref())
                == Some(conversation_id.as_str())
        {
            self.refresh_canvas_authoring(cx);
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
                            model.refresh_canvas_authoring(cx);
                        }
                    }
                    Err(error) => model.chat.update(cx, |chat, _| {
                        if chat.project_id.as_ref().map(AsRef::as_ref) == Some(project_id.as_str())
                        {
                            chat.set_conversation_error(&conversation_id, error);
                        }
                    }),
                }
                cx.notify();
            });
        });
        self.chat_resync_tasks.insert(task_key, task);
    }

    pub(crate) fn toggle_agents_viewer(&mut self, cx: &mut Context<Self>) {
        self.agents.update(cx, |agents, _| {
            agents.viewer_enabled = !agents.viewer_enabled
        });
        cx.notify();
    }

    pub(crate) fn toggle_agents_terminal_expanded(&mut self, cx: &mut Context<Self>) {
        self.agents.update(cx, |agents, _| {
            agents.terminal_expanded = !agents.terminal_expanded;
            agents.active_model_picker = None;
        });
        cx.notify();
    }

    pub(crate) fn cycle_agent_api_filter(&mut self, key: String, cx: &mut Context<Self>) {
        self.agents.update(cx, |agents, _| {
            let filter = agents
                .api_filters
                .entry(key)
                .or_insert(ApiActivityFilter::All);
            *filter = filter.next();
        });
        cx.notify();
    }

    pub(crate) fn clear_agent_api_activity(
        &mut self,
        target: AgentIdentifier,
        cx: &mut Context<Self>,
    ) {
        self.agents.update(cx, |agents, _| {
            agents.api_activity.remove(&target);
            agents.notice =
                Some("Local API viewer cleared; persisted history was retained.".into());
        });
        cx.notify();
    }

    pub(crate) fn export_agent_api_execution(
        &mut self,
        target: AgentIdentifier,
        execution_id: String,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.agents_backend.clone() else {
            return;
        };
        let Some(project_id) = self.workspace.read(cx).project_id.clone() else {
            return;
        };
        let generation = self.agents.update(cx, |agents, _| agents.begin(false));
        cx.notify();
        self.agents_task = Some(cx.spawn(async move |this, cx| {
            let result = backend
                .export_api_execution(project_id.to_string(), target.target(), execution_id)
                .await;
            let _ = this.update(cx, |model, cx| {
                match result {
                    Ok(path) => {
                        model.agents.update(cx, |agents, _| {
                            if agents.generation == generation {
                                agents.busy = false;
                                agents.notice =
                                    Some(format!("Redacted API execution exported to {path}"));
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

    pub(crate) fn configure_agent_api(
        &mut self,
        role: ikore_core::llm::AgentRole,
        cx: &mut Context<Self>,
    ) {
        let descriptor = ikore_core::terminals::canonical_agent(role);
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
            agents.selected_api_target = Some(AgentIdentifier::Canonical(role));
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
        role: ikore_core::llm::AgentRole,
        cx: &mut Context<Self>,
    ) {
        self.configure_agent_api(role, cx);
        self.save_settings_binding(cx);
    }

    pub(crate) fn configure_free_form_api(&mut self, slot_id: String, cx: &mut Context<Self>) {
        let (route_id, connection_id) = {
            let agents = self.agents.read(cx);
            let settings = self.settings.read(cx);
            let route = settings.snapshot.as_ref().and_then(|snapshot| {
                agents
                    .free_form_bindings
                    .iter()
                    .find(|binding| binding.slot_id == slot_id)
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
            agents.selected_api_target = Some(AgentIdentifier::FreeForm(slot_id));
        });
        if let Some(route_id) = route_id {
            self.select_settings_route(route_id, cx);
        }
        if let Some(connection_id) = connection_id {
            self.select_settings_connection(connection_id, cx);
        }
        cx.notify();
    }

    pub(crate) fn save_free_form_api_binding(&mut self, slot_id: String, cx: &mut Context<Self>) {
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
        let Some(route_id) = self.settings.read(cx).route().map(|route| route.id.clone()) else {
            return;
        };
        let version = self
            .agents
            .read(cx)
            .free_form_bindings
            .iter()
            .find(|binding| binding.project_id == project_id && binding.slot_id == slot_id)
            .map(|binding| binding.version);
        let generation = self.agents.update(cx, |agents, _| agents.begin(false));
        self.agents_task = Some(cx.spawn(async move |this, cx| {
            let result = backend
                .upsert_free_form_binding(UpsertFreeFormProviderRouteBindingRequest {
                    project_id,
                    slot_id,
                    route_id,
                    enabled: Some(true),
                    version,
                })
                .await;
            let _ = this.update(cx, |model, cx| {
                match result {
                    Ok(binding) => {
                        model.agents.update(cx, |agents, _| {
                            if generation == agents.generation {
                                agents.busy = false;
                                if let Some(current) = agents
                                    .free_form_bindings
                                    .iter_mut()
                                    .find(|current| current.slot_id == binding.slot_id)
                                {
                                    *current = binding;
                                } else {
                                    agents.free_form_bindings.push(binding);
                                }
                                agents.notice = Some("FreeForm API route updated.".into());
                            }
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

    pub(crate) fn open_advanced_api_settings(
        &mut self,
        role: ikore_core::llm::AgentRole,
        cx: &mut Context<Self>,
    ) {
        self.configure_agent_api(role, cx);
        // Straight to the route the agent uses, when it has one.
        let has_route = self.settings.read(cx).selected_route_id.is_some();
        self.settings
            .update(cx, |settings, _| settings.route_detail = has_route);
        self.open_settings(Some(crate::settings::SettingsPage::Routes), cx);
    }

    pub(crate) fn refresh_agents(&mut self, cx: &mut Context<Self>) {
        self.refresh_agents_inner(false, false, cx);
    }

    pub(crate) fn refresh_agent_diagnostics(&mut self, cx: &mut Context<Self>) {
        self.refresh_agents_inner(true, false, cx);
    }

    /// Refreshes the agents snapshot for the shell chrome without touching the Agents panel's
    /// loading/busy state; skipped while a loud read or an operation is in flight.
    pub(crate) fn refresh_agents_quiet(&mut self, cx: &mut Context<Self>) {
        self.refresh_agents_inner(false, true, cx);
    }

    pub(crate) fn toggle_agent_follow_tail(&mut self, key: String, cx: &mut Context<Self>) {
        self.agents.update(cx, |agents, _| {
            let enabled = agents.follow_tail.get(&key).copied().unwrap_or(true);
            agents.follow_tail.insert(key, !enabled);
        });
        cx.notify();
    }

    pub(crate) fn cycle_agent_event_filter(&mut self, key: String, cx: &mut Context<Self>) {
        self.agents.update(cx, |agents, _| {
            let filter = agents
                .event_filters
                .entry(key)
                .or_insert(crate::agents::TerminalEventFilter::All);
            *filter = filter.next();
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
                                ikore_core::terminals::TerminalEventKind::System => "system",
                                ikore_core::terminals::TerminalEventKind::Message => "message",
                                ikore_core::terminals::TerminalEventKind::ToolUse => "tool_use",
                                ikore_core::terminals::TerminalEventKind::ToolResult =>
                                    "tool_result",
                                ikore_core::terminals::TerminalEventKind::Stdout => "stdout",
                                ikore_core::terminals::TerminalEventKind::Stderr => "stderr",
                                ikore_core::terminals::TerminalEventKind::Warning => "warning",
                                ikore_core::terminals::TerminalEventKind::Error => "error",
                                ikore_core::terminals::TerminalEventKind::Completed => "completed",
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

    fn refresh_agents_inner(&mut self, force_discovery: bool, quiet: bool, cx: &mut Context<Self>) {
        if self.lifecycle.is_disposed() {
            return;
        }
        if self.agents.read(cx).in_flight() {
            self.agents_refresh_pending = Some(
                self.agents_refresh_pending
                    .take()
                    .map(|(pending_force, pending_quiet)| {
                        (pending_force || force_discovery, pending_quiet && quiet)
                    })
                    .unwrap_or((force_discovery, quiet)),
            );
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
        let generation = self.agents.update(cx, |agents, _| {
            if quiet {
                agents.begin_quiet()
            } else {
                agents.begin(true)
            }
        });
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
                        let known: std::collections::HashSet<String> = model
                            .agents
                            .read(cx)
                            .fallbacks
                            .iter()
                            .map(|fallback| fallback.checkpoint_id.clone())
                            .collect();
                        let arrived: Vec<(String, String, ikore_core::llm::AgentRole)> = snapshot
                            .fallbacks
                            .iter()
                            .filter(|fallback| !known.contains(&fallback.checkpoint_id))
                            .map(|fallback| {
                                (
                                    fallback.error_code.clone(),
                                    fallback.diagnostic.clone(),
                                    fallback.agent_role,
                                )
                            })
                            .collect();
                        let applied = model
                            .agents
                            .update(cx, |agents, _| agents.apply_snapshot(generation, snapshot));
                        if applied {
                            model.finish_agents_resync();
                        }
                        if applied {
                            for (code, diagnostic, role) in arrived {
                                model.announce_cli_fallback(&code, &diagnostic, role, cx);
                            }
                        }
                    }
                    // A quiet read keeps whatever the chrome already shows; the next loud read
                    // (opening Agents) reports the failure in its own panel.
                    Err(_) if quiet => {}
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
                if let Some((force_discovery, quiet)) = model.agents_refresh_pending.take() {
                    model.refresh_agents_inner(force_discovery, quiet, cx);
                }
                cx.notify();
            });
        }));
    }

    pub(crate) fn current_agent_config(
        &self,
        role: ikore_core::llm::AgentRole,
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
        role: ikore_core::llm::AgentRole,
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
                if self.chat.read(cx).target_change_pending {
                    self.close_cli_model_picker(cx);
                    return;
                }
                self.chat.update(cx, |chat, _| {
                    chat.selected_cli_model = model_id;
                    chat.selected_cli_model_profile = None;
                    chat.selected_cli_model_source = source;
                });
                self.apply_chat_target_change(cx);
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
        role: ikore_core::llm::AgentRole,
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
        role: ikore_core::llm::AgentRole,
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
                            format!("Custom arguments must be a JSON array of strings: {error}"),
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
        role: ikore_core::llm::AgentRole,
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
                                    "Custom arguments must be a JSON array of strings: {error}"
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

    pub(crate) fn start_free_form_api_agent(&mut self, slot_id: String, cx: &mut Context<Self>) {
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
        let slot_for_result = slot_id.clone();
        self.agents_task = Some(cx.spawn(async move |this, cx| {
            let result = backend
                .start_free_form_api(StartFreeFormApiRunRequest {
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
                            agents.apply_started_api_run(
                                generation,
                                slot_for_result,
                                run.execution_id,
                            );
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

    pub(crate) fn cancel_free_form_api_agent(&mut self, slot_id: String, cx: &mut Context<Self>) {
        let Some(backend) = self.agents_backend.clone() else {
            return;
        };
        let Some(execution_id) = self
            .agents
            .read(cx)
            .active_api_execution_by_slot
            .get(&slot_id)
            .cloned()
        else {
            return;
        };
        self.agents_task = Some(cx.spawn(async move |this, cx| {
            let result = backend.cancel_api_execution(execution_id).await;
            let _ = this.update(cx, |model, cx| {
                match result {
                    Ok(true) => model.agents.update(cx, |agents, _| {
                        agents.notice = Some("API cancellation requested.".into());
                    }),
                    Ok(false) => model.agents.update(cx, |agents, _| {
                        agents.notice = Some("API execution is no longer active.".into());
                        agents.active_api_execution_by_slot.remove(&slot_id);
                    }),
                    Err(error) => model.agents.update(cx, |agents, _| {
                        agents.error = Some(error);
                    }),
                }
                cx.notify();
            });
        }));
    }

    pub(crate) fn resume_agent_run(
        &mut self,
        source_run_id: String,
        slot_id: String,
        role: Option<ikore_core::llm::AgentRole>,
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
            // ADR-037 M3: say what the agent will be told. A refusal comes
            // from the resume itself, with the count.
            let told = match backend
                .preview_resume(project_id.clone(), source_run_id.clone())
                .await
            {
                Ok(Some(ikore_core::terminals::ResumePreview::Changes(changes)))
                    if changes.decision
                        == ikore_core::exchange::external_changes::ExternalChangesDecision::Notice =>
                {
                    Some(format!(
                        "{} files changed since this session; the agent will be told",
                        changes.changed_files()
                    ))
                }
                _ => None,
            };
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
                        if let Some(told) = told {
                            model.push_header_notice(HeaderNoticeKind::Notice, told, cx);
                        }
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
        role: ikore_core::llm::AgentRole,
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
        role: ikore_core::llm::AgentRole,
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

    /// A CLI fallback checkpoint just appeared: say so where the developer is looking (toast +
    /// activity), pointing at Agents where the decision lives.
    fn announce_cli_fallback(
        &mut self,
        code: &str,
        diagnostic: &str,
        role: ikore_core::llm::AgentRole,
        cx: &mut Context<Self>,
    ) {
        let agent = ikore_core::terminals::canonical_agent(role).name;
        let (summary, kind) = if code == ikore_core::persistence_service::CLI_VERSION_UNVALIDATED {
            (
                format!("{agent}: updated CLI needs validation before the pulsation continues"),
                HeaderNoticeKind::Warning,
            )
        } else if code == ikore_core::persistence_service::CLI_QUOTA_LOW {
            (
                format!("{agent}: plan quota is low — the stage waits for your decision"),
                HeaderNoticeKind::Warning,
            )
        } else {
            (
                format!("{agent}: CLI fallback awaiting your decision"),
                HeaderNoticeKind::Warning,
            )
        };
        self.push_header_notice(kind, format!("{summary} — {diagnostic}"), cx);
        self.record_activity(
            ActivityKind::Agent,
            summary,
            Some(FlowTarget::AgentFallback(role)),
            true,
        );
    }

    /// "Update & validate CLI": lets the CLI launch (Gemini updates itself on launch), probes the
    /// adapter contract against the new version, and resumes the pulsation when it passes.
    pub(crate) fn validate_agent_cli_fallback(
        &mut self,
        checkpoint_id: String,
        expected_state_version: u64,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.agents_backend.clone() else {
            return;
        };
        let generation = self.agents.update(cx, |agents, _| {
            let generation = agents.begin(false);
            agents.notice = Some("Validating the updated CLI — this can take a minute…".into());
            generation
        });
        cx.notify();
        self.agents_task = Some(cx.spawn(async move |this, cx| {
            let result = backend
                .validate_fallback(ikore_core::persistence_service::ValidateCliFallbackRequest {
                    checkpoint_id,
                    expected_state_version,
                })
                .await;
            let _ = this.update(cx, |model, cx| {
                match result {
                    Ok(response) => {
                        let validated = response.report.validated;
                        let version = response
                            .report
                            .version
                            .clone()
                            .unwrap_or_else(|| "unknown".into());
                        let label = response.report.backend.label();
                        model.agents.update(cx, |agents, _| {
                            if agents.generation == generation {
                                agents.busy = false;
                                agents.notice = Some(if validated {
                                    format!("{label} v{version} validated — pulsation resumed.")
                                } else {
                                    format!(
                                        "{label} v{version} failed validation. Switch provider or abort the pulsation."
                                    )
                                });
                            }
                        });
                        if !validated {
                            model.push_header_notice(
                                HeaderNoticeKind::Warning,
                                response.report.diagnostic.clone(),
                                cx,
                            );
                        }
                        model.record_activity(
                            ActivityKind::Agent,
                            if validated {
                                format!("{label} v{version} validated · pulsation resumed")
                            } else {
                                format!("{label} v{version} failed validation")
                            },
                            Some(FlowTarget::View(AppView::Agents)),
                            !validated,
                        );
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
        if self.settings_page_visible(crate::settings::SettingsPage::Usage, cx) {
            self.refresh_usage(cx);
        }
        if self.settings_page_visible(crate::settings::SettingsPage::Connections, cx) {
            self.refresh_cli_connections(true, cx);
        }
        self.refresh_settings_inner(false, cx);
    }

    pub(crate) fn refresh_cli_connections(&mut self, refresh: bool, cx: &mut Context<Self>) {
        if self.lifecycle.is_disposed() || self.settings.read(cx).cli_connections_loading {
            return;
        }
        let settings = self.settings.read(cx);
        if !refresh
            && settings.cli_connections.as_ref().is_some_and(|snapshot| {
                chrono::DateTime::parse_from_rfc3339(&snapshot.checked_at).is_ok_and(|checked| {
                    chrono::Utc::now()
                        .signed_duration_since(checked)
                        .num_seconds()
                        < 60
                })
            })
        {
            return;
        }
        let Some(backend) = self.settings_backend.clone() else {
            return;
        };
        self.settings.update(cx, |settings, _| {
            settings.cli_connections_loading = true;
            settings.cli_connections_error = None;
        });
        cx.notify();
        self.cli_connections_task = Some(cx.spawn(async move |this, cx| {
            let result = backend.cli_connections(refresh).await;
            let _ = this.update(cx, |model, cx| {
                if model.lifecycle.is_disposed() {
                    return;
                }
                if let Ok(snapshot) = &result {
                    model.apply_quota_overview(snapshot.overview.clone(), cx);
                }
                model.settings.update(cx, |settings, _| {
                    settings.cli_connections_loading = false;
                    match result {
                        Ok(snapshot) => settings.cli_connections = Some(snapshot),
                        Err(error) => settings.cli_connections_error = Some(error),
                    }
                });
                cx.notify();
            });
        }));
    }

    /// Refreshes providers, resilience and embedding readiness for the shell chrome without
    /// flipping the Settings panel into its loading state; skipped while Settings is busy.
    pub(crate) fn refresh_settings_quiet(&mut self, cx: &mut Context<Self>) {
        if self.settings.read(cx).in_flight() {
            return;
        }
        self.refresh_settings_inner(true, cx);
    }

    /// Debounced quiet refresh for bursts of LLM failures (each one may open a cooldown).
    fn schedule_settings_quiet_refresh(&mut self, cx: &mut Context<Self>) {
        if self.settings_quiet_refresh_pending || self.lifecycle.is_disposed() {
            return;
        }
        self.settings_quiet_refresh_pending = true;
        let timer = cx
            .background_executor()
            .timer(std::time::Duration::from_millis(350));
        cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |model, cx| {
                model.settings_quiet_refresh_pending = false;
                if !model.settings_modal.open {
                    model.refresh_settings_quiet(cx);
                }
            });
        })
        .detach();
    }

    fn refresh_settings_inner(&mut self, quiet: bool, cx: &mut Context<Self>) {
        if self.lifecycle.is_disposed() {
            return;
        }
        let Some(backend) = self.settings_backend.clone() else {
            return;
        };
        let generation = self.settings.update(cx, |settings, _| {
            if quiet {
                settings.begin_quiet()
            } else {
                settings.begin(true)
            }
        });
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
                        model.fill_risk_policy_inputs(&snapshot, cx);
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
                        // ADR-034 D1: the batch composer says when the review
                        // policy does not apply; keep it in step with a
                        // composition toggle made in Settings.
                        let composition_enabled = model
                            .settings
                            .read(cx)
                            .snapshot
                            .as_ref()
                            .and_then(|snapshot| snapshot.composition.as_ref())
                            .map(|project| project.settings.enabled);
                        if applied
                            && composition_enabled.is_some_and(|enabled| {
                                enabled != model.forge.snapshot.composition_enabled
                            })
                        {
                            model.refresh_forge(cx);
                        }
                    }
                    Err(_) if quiet => {}
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

    pub(crate) fn select_project_for_deletion(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(backend) = self.settings_backend.clone() else {
            return;
        };
        self.settings_project_delete_input
            .update(cx, |input, cx| input.clear(cx));
        let generation = self.settings.update(cx, |settings, _| {
            settings.deletion_generation = settings.deletion_generation.saturating_add(1);
            settings.deletion_project_id = Some(id.clone());
            settings.deletion_preview = None;
            settings.deletion_loading = true;
            settings.error = None;
            settings.deletion_generation
        });
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = backend.preview_project_deletion(id.clone()).await;
            let _ = this.update(cx, |model, cx| {
                model.settings.update(cx, |settings, _| {
                    if settings.deletion_generation != generation
                        || settings.deletion_project_id.as_deref() != Some(&id)
                    {
                        return;
                    }
                    settings.deletion_loading = false;
                    match result {
                        Ok(preview) => settings.deletion_preview = Some(preview),
                        Err(error) => settings.error = Some(error),
                    }
                });
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn cancel_project_deletion(&mut self, cx: &mut Context<Self>) {
        if self.settings.read(cx).deletion_busy {
            return;
        }
        self.settings.update(cx, |settings, _| {
            settings.deletion_generation = settings.deletion_generation.saturating_add(1);
            settings.deletion_project_id = None;
            settings.deletion_preview = None;
            settings.deletion_loading = false;
            settings.deletion_busy = false;
            settings.error = None;
        });
        self.settings_project_delete_input
            .update(cx, |input, cx| input.clear(cx));
        cx.notify();
    }

    pub(crate) fn confirm_project_deletion(&mut self, cx: &mut Context<Self>) {
        let Some(backend) = self.settings_backend.clone() else {
            return;
        };
        let (preview, generation, busy) = {
            let settings = self.settings.read(cx);
            (
                settings.deletion_preview.clone(),
                settings.deletion_generation,
                settings.deletion_busy,
            )
        };
        let Some(preview) = preview else {
            return;
        };
        if busy {
            return;
        }
        let confirmation = self
            .settings_project_delete_input
            .read(cx)
            .value()
            .to_owned();
        if confirmation != preview.project.name {
            self.settings.update(cx, |settings, _| {
                settings.error = Some(UiCommandError::worker(
                    "Type the exact project name to confirm deletion.",
                ))
            });
            cx.notify();
            return;
        }
        let active = self
            .workspace
            .read(cx)
            .project_id
            .as_ref()
            .is_some_and(|id| id.as_ref() == preview.project.id);
        if active
            && (self.canvas_has_pending()
                || self
                    .workspace
                    .read(cx)
                    .tabs
                    .iter()
                    .any(|tab| tab.dirty || tab.saving || tab.conflict.is_some()))
        {
            self.settings.update(cx,|settings,_|settings.error=Some(UiCommandError::worker("Save or close all dirty, saving or conflicted editor tabs before deleting this project.")));
            cx.notify();
            return;
        }
        let id = preview.project.id.clone();
        self.settings.update(cx, |settings, _| {
            settings.deletion_busy = true;
            settings.error = None;
        });
        cx.notify();
        cx.spawn(async move |this,cx| {
            let result=backend.delete_project_permanently(id.clone(),confirmation).await;
            let _=this.update(cx,|model,cx| {
                let current=model.settings.read(cx).deletion_generation==generation;
                model.settings.update(cx,|settings,_|settings.deletion_busy=false);
                match result {
                    Ok(outcome)=> {
                        if active && model.workspace.read(cx).project_id.as_ref().is_some_and(|current|current.as_ref()==id) {model.clear_deleted_active_project(cx);}
                        let message=if outcome.cleanup_pending {
                            "Project removed. Some quarantined files or old backups still need cleanup; the app will retry on startup."
                        } else {"Project and repository permanently removed. Usage remains in the attributed history."};
                        model.settings.update(cx,|settings,_| {
                            settings.deletion_project_id=None;
                            settings.deletion_preview=None;
                            settings.notice=Some(message.into());
                            settings.status_line=Some((crate::settings::StatusTone::Saved,message.into()));
                        });
                        model.status=message.into();
                        model.settings_project_delete_input.update(cx,|input,cx|input.clear(cx));
                        model.refresh_settings(cx);
                        model.refresh_usage(cx);
                    }
                    Err(error) if current=>model.settings.update(cx,|settings,_|settings.error=Some(error)),
                    Err(_)=>{},
                }
                cx.notify();
            });
        }).detach();
    }

    fn clear_deleted_active_project(&mut self, cx: &mut Context<Self>) {
        self.session_task.take();
        self.session_generation = self.session_generation.saturating_add(1);
        self.workspace_task.take();
        self.brownfield_task.take();
        self.brownfield_generation = self.brownfield_generation.saturating_add(1);
        self.workspace.update(cx, |workspace, cx| {
            workspace.project_id = None;
            workspace.root_path = None;
            workspace.root_path_canonical = None;
            workspace.tree.clear();
            workspace.tabs.clear();
            workspace.active_relative_path = None;
            workspace.editor.update(cx, |editor, cx| {
                *editor = crate::native_editor::NativeEditor::new(cx)
            });
        });
        self.pulsations
            .update(cx, |pulsations, _| pulsations.reset(None));
        self.chat.update(cx, |chat, _| *chat = ChatModel::default());
        self.agents.update(cx, |agents, _| {
            *agents = crate::agents::AgentsModel::default()
        });
        self.vault
            .update(cx, |vault, _| *vault = VaultModel::default());
        self.forge = crate::forge::ForgeUi::new(cx);
        self.notes = crate::notes::NotesUi::new(cx);
        self.canvas = crate::canvas::CanvasUi::new(cx);
        self.pending_canvas_workspace = None;
        self.canvas_close_requested = false;
        self.canvas_close_window = None;
        self.canvas_project_switching = false;
        self.navigation
            .update(cx, |navigation, _| navigation.active = AppView::Home);
        self.sync_voice_project(cx);
    }

    pub(crate) fn run_settings_operation(
        &mut self,
        operation: crate::settings::SettingsFuture<String>,
        cx: &mut Context<Self>,
    ) {
        if self.lifecycle.is_disposed() {
            return;
        }
        let generation = self.settings.update(cx, |settings, _| {
            settings.status_line = Some((crate::settings::StatusTone::Working, "Working…".into()));
            settings.begin(false)
        });
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
                                settings.status_line = Some((
                                    crate::settings::StatusTone::Saved,
                                    message.clone().into(),
                                ));
                                settings.notice = Some(message.into());
                            });
                            model.refresh_settings(cx);
                        }
                    }
                    Err(error) => {
                        let message = error.presentable();
                        if model.settings.update(cx, |settings, _| {
                            settings.status_line = Some((
                                crate::settings::StatusTone::Failed,
                                message.to_string().into(),
                            ));
                            settings.fail(generation, error)
                        }) && !model.settings_modal.open
                        {
                            // With Settings open the status line already says it; the error
                            // still reaches the notification history, without a toast.
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
        let model_is_empty = self
            .settings_connection_model_input
            .read(cx)
            .value()
            .trim()
            .is_empty();
        let default_model = self.settings.update(cx, |settings, _| {
            let changed = settings.connection_provider_id.as_ref() != provider_id;
            let default_model = (changed || model_is_empty)
                .then(|| {
                    settings
                        .snapshot
                        .as_ref()?
                        .profiles
                        .iter()
                        .find(|profile| profile.id == provider_id)
                        .map(|profile| profile.default_model.clone())
                })
                .flatten();
            settings.connection_provider_id = provider_id.into();
            default_model
        });
        if let Some(default_model) = default_model {
            self.settings_connection_model_input
                .update(cx, |input, cx| input.set_value(default_model, cx));
        }
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
            settings.connection_detail = true;
            settings.page = crate::settings::SettingsPage::Connections;
        });
        for input in [
            &self.settings_connection_name_input,
            &self.settings_connection_model_input,
            &self.settings_connection_endpoint_input,
            &self.settings_connection_secret_input,
        ] {
            input.update(cx, |input, cx| input.clear(cx));
        }
        let provider_id = self.settings.read(cx).connection_provider_id.to_string();
        self.select_settings_provider(provider_id, cx);
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
        if self.settings.read(cx).testing_connection.is_some() {
            return;
        }
        let model = self
            .settings
            .read(cx)
            .connection()
            .map(|connection| connection.default_model.clone())
            .unwrap_or_default();
        self.settings.update(cx, |settings, _| {
            settings.testing_connection = Some(connection_id.clone());
            settings.status_line = Some((
                crate::settings::StatusTone::Working,
                "Testing the connection…".into(),
            ));
        });
        cx.notify();
        // Not a generic Settings operation: the result is kept per connection, with its latency
        // and a classified failure, instead of a one-line notice.
        self.settings_test_task = Some(cx.spawn(async move |this, cx| {
            let started = std::time::Instant::now();
            let result = backend
                .test_connection(TestProviderConnectionRequest {
                    connection_id: connection_id.clone(),
                })
                .await;
            let _ = this.update(cx, |model_state, cx| {
                use crate::settings::{ConnectionTestOutcome, ConnectionTestResult};
                let at = crate::time::current_clock_hhmm();
                let outcome = match result {
                    Ok(response) => ConnectionTestResult {
                        outcome: if response.reachable {
                            ConnectionTestOutcome::Connected
                        } else {
                            ConnectionTestOutcome::ConnectedWithWarnings
                        },
                        latency_ms: Some(response.latency_ms),
                        model: response.model,
                        detail: response.message,
                        at,
                    },
                    Err(error) => {
                        let outcome = ConnectionTestOutcome::from_error(&error);
                        ConnectionTestResult {
                            outcome,
                            // Time to the failure, so a timeout reads as ~30 s; a missing key
                            // fails before any request, so it has no latency to show.
                            latency_ms: (outcome != ConnectionTestOutcome::NoApiKey)
                                .then(|| started.elapsed().as_millis() as u64),
                            model,
                            detail: error.presentable(),
                            at,
                        }
                    }
                };
                model_state.settings.update(cx, |settings, _| {
                    settings.testing_connection = None;
                    settings.status_line = Some((outcome.outcome.tone(), outcome.summary().into()));
                    settings.connection_tests.insert(connection_id, outcome);
                });
                cx.notify();
            });
        }));
    }

    /// Refreshes remote API metadata only after an explicit Settings action. It never changes a
    /// connection, route, or conversation; selecting an entry still only edits the form draft.
    pub(crate) fn refresh_settings_api_catalog(&mut self, cx: &mut Context<Self>) {
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
                let snapshot = backend.refresh_api_model_catalog(connection_id).await?;
                let state = match snapshot.state {
                    ikore_core::llm::ApiModelCatalogState::Ready => "updated",
                    ikore_core::llm::ApiModelCatalogState::Stale => {
                        "stale; showing last successful result"
                    }
                    ikore_core::llm::ApiModelCatalogState::Failed => {
                        "failed; no previous result is available"
                    }
                    ikore_core::llm::ApiModelCatalogState::Unsupported => {
                        "not available for this provider"
                    }
                };
                Ok(format!("API model catalog {state}."))
            }),
            cx,
        );
    }

    pub(crate) fn select_settings_api_model(&mut self, model_id: String, cx: &mut Context<Self>) {
        self.settings_connection_model_input
            .update(cx, |input, cx| input.set_value(model_id, cx));
        self.close_dropdown(cx);
        cx.notify();
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
            settings.route_detail = true;
            settings.page = crate::settings::SettingsPage::Routes;
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
        if self.active_view != AppView::Logs {
            return;
        }
        let Some(backend) = self.logs_backend.clone() else {
            return;
        };
        let generation = self.logs.update(cx, |logs, _| logs.begin_poll());
        let Some(generation) = generation else {
            return;
        };
        let active_project_id = self.workspace.read(cx).project_id.clone();
        let project_id = active_project_id.as_ref().map(ToString::to_string);
        self.logs_task = Some(cx.spawn(async move |this, cx| {
            let result = backend.observability(project_id, None).await;
            let _ = this.update(cx, |model, cx| {
                if model.workspace.read(cx).project_id != active_project_id {
                    model.logs.update(cx, |logs, _| {
                        logs.discard_poll(generation);
                        logs.reset_expansion();
                    });
                    model.refresh_logs(cx);
                    return;
                }
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

    pub(crate) fn toggle_log_pulsation_group(&mut self, key: String, cx: &mut Context<Self>) {
        self.logs.update(cx, |logs, _| {
            logs.toggle_group(&key);
        });
        cx.notify();
    }

    pub(crate) fn jump_to_pulsation_from_logs(
        &mut self,
        pulsation_id: String,
        cx: &mut Context<Self>,
    ) {
        self.pulsations.update(cx, |pulsations, _| {
            pulsations.select(&pulsation_id);
        });
        self.select_view(AppView::Pulsations, cx);
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

    fn request_event_resync(&mut self, cx: &mut Context<Self>) {
        if self.event_resync_pending {
            return;
        }
        self.event_resync_pending = true;
        self.needs_resync = true;
        self.artifact_read.invalidate();
        self.backend_read.invalidate();
        self.cleanup_read.invalidate();
        self.status = "Event history delayed; refreshing runtime snapshots".into();
        // Workspace changes may also have been lost; refresh its index without
        // replacing open buffers, the current project, or navigation state.
        self.mark_vault_stale(cx);
        self.refresh_canvas_authoring(cx);
    }

    /// An applied Agents snapshot is the resync for a terminal/agents sequence gap. A lagged
    /// broadcast still owns `needs_resync` until its own resync (`finish_event_resync`) ends.
    fn finish_agents_resync(&mut self) {
        if !std::mem::take(&mut self.agents_resync_pending) {
            return;
        }
        if !self.event_resync_pending {
            self.needs_resync = false;
            self.status = "Agent history resynchronized".into();
        }
    }

    fn finish_event_resync(&mut self, cx: &mut Context<Self>) {
        if !self.event_resync_pending {
            return;
        }
        self.event_resync_pending = false;
        self.needs_resync = false;
        match self.active_view {
            AppView::Agents => self.refresh_agents(cx),
            AppView::Forge => self.refresh_forge(cx),
            AppView::Notes => self.refresh_notes(cx),
            AppView::Canvas => self.refresh_canvas(cx),
            AppView::Pulsations => self.refresh_pulsation_usage(cx),
            AppView::Chat => {
                let project = self
                    .workspace
                    .read(cx)
                    .project_id
                    .as_ref()
                    .map(ToString::to_string);
                let conversation = self
                    .chat
                    .read(cx)
                    .active_conversation
                    .as_ref()
                    .map(ToString::to_string);
                if let Some(project) = project {
                    self.hydrate_chat_project(project, conversation, cx);
                }
            }
            _ => {}
        }
        if self.settings_modal.open {
            self.refresh_settings(cx);
        }
    }

    fn start_lifecycle(&mut self, cx: &mut Context<Self>) {
        let Some(core) = self.core.clone() else {
            return;
        };
        self.lifecycle.start(1);
        // The core registered push-to-talk before this subscription existed.
        self.refresh_voice_status(cx);

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
                                model.handle_event_lag(cx);
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
                let interval = match this.update(cx, |model, cx| {
                    let pulsations = model.pulsations.read(cx);
                    if pulsations.running_jobs > 0
                        || pulsations.awaiting_hitl > 0
                        || pulsations.awaiting_failure > 0
                    {
                        std::time::Duration::from_millis(1_200)
                    } else {
                        std::time::Duration::from_secs(5)
                    }
                }) {
                    Ok(interval) => interval,
                    Err(_) => break,
                };
                timer.timer(interval).await;
                let state = this.update(cx, |model, cx| {
                    if model.lifecycle.is_disposed() {
                        return false;
                    }
                    // ADR-037 M2: with a healthy doorbell, pulsations poll every 5 s.
                    if model.workspace.read(cx).project_id.is_some()
                        && !model.snapshot_in_flight
                        && model.pulsation_poll_due()
                        && model.lifecycle.try_begin_poll()
                    {
                        model.refresh_pulsations(cx);
                    }
                    if model.workspace.read(cx).project_id.is_some() {
                        model.refresh_resolution_chips(cx);
                        model.poll_resolution_turn_if_pending(cx);
                    }
                    model.tick_footer_clock(cx);
                    true
                });
                if !matches!(state, Ok(true)) {
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
        self.canvas_project_switching = false;
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
        // Sessions saved before Settings became a modal may name it; land on Home instead.
        let active_view = session
            .map(|session| AppView::from_id(&session.active_view))
            .filter(|view| *view != AppView::Settings)
            .unwrap_or(AppView::Home);
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
            navigation.context_rail_collapsed =
                session.is_some_and(|value| value.sidebar_collapsed);
        });
        self.activity.clear();
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
        self.sync_voice_project(cx);
        self.active_view = active_view;
        self.needs_resync = false;
        self.session_dirty = false;
        self.status = format!("Session restored: {}", restored.root_path).into();
        if let Some(notice) = restored.notice {
            self.push_header_notice(HeaderNoticeKind::Notice, notice, cx);
        }
        self.hydrating = false;
        self.refresh_pulsations_with_preferred(preferred_pulsation_id, cx);
        self.refresh_resolution_chips(cx);
        self.refresh_vault(true, cx);
        // Home's next step is a function of the Forge snapshot, and the header/context rail read
        // the agents and settings snapshots, so all three load with the project instead of
        // waiting for their view to be opened.
        self.refresh_forge(cx);
        // Recover committed drawing mirrors even when the restored view is not Canvas.
        self.refresh_canvas(cx);
        match active_view {
            AppView::Agents => self.refresh_agents(cx),
            AppView::Logs => self.refresh_logs(cx),
            AppView::Home => self.refresh_home_stats(true, cx),
            _ => {}
        }
        if active_view != AppView::Agents {
            self.refresh_agents_quiet(cx);
        }
        self.refresh_settings_quiet(cx);
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
            sidebar_collapsed: self.navigation.read(cx).context_rail_collapsed,
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

    pub(crate) fn take_chat_admissions_for_shutdown(&mut self) -> Vec<Task<()>> {
        let mut admissions = self
            .chat_tasks
            .drain()
            .filter_map(|((_, operation), task)| (operation == ChatOperation::Send).then_some(task))
            .collect::<Vec<_>>();
        if let Some(create) = self.chat_create_task.take() {
            admissions.push(create);
        }
        admissions
    }

    fn flush_pending_session(&mut self, cx: &mut Context<Self>) -> Task<()> {
        let companion = self.flush_companion_for_shutdown(cx);
        self.lifecycle.dispose();
        self.event_task.take();
        self.llm_delta_task.take();
        self.pending_llm_deltas.clear();
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
        self.pulsation_snapshot_task.take();
        self.backend_override_task.take();
        let backend_override_mutation = self.backend_override_mutation_task.take();
        self.hitl_documents_task.take();
        self.vault_refresh_task.take();
        self.vault_search_task.take();
        self.vault_rename_task.take();
        self.vault_context_task.take();
        self.vault_document_tasks.clear();
        let settings = self.settings_task.take();
        self.cli_connections_task.take();
        self.quota.dispose();
        self.agents_task.take();
        self.agents_notice_timer.take();
        self.model_catalog_task.take();
        self.logs_task.take();
        self.logs_resync_tasks.clear();
        self.chat_hydration_task.take();
        self.chat_create_task.take();
        self.chat_tasks.clear();
        self.chat_target_task.take();
        self.chat_resync_tasks.clear();
        self.resolution_chips_task.take();
        self.resolution_conversation_task.take();
        self.resolution_action_task.take();
        self.resolution_turn_task.take();
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
            companion.await;
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
            if let Some(task) = backend_override_mutation {
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

    fn queue_llm_delta(
        &mut self,
        envelope: ikore_core::llm::LlmEventEnvelope,
        cx: &mut Context<Self>,
    ) {
        self.pending_llm_deltas.push(envelope);
        if self.llm_delta_task.is_some() {
            return;
        }
        let timer = cx.background_executor().clone();
        self.llm_delta_task = Some(cx.spawn(async move |this, cx| {
            timer.timer(std::time::Duration::from_millis(250)).await;
            let _ = this.update(cx, |model, cx| {
                model.llm_delta_task = None;
                model.apply_pending_llm_deltas(cx);
            });
        }));
    }

    /// Flushes only the UI-side projection of already persisted agent output. A terminal event
    /// calls this before it is applied so a completion, failure, or cancellation can never pass
    /// older visible text.
    fn flush_pending_llm_deltas(&mut self, cx: &mut Context<Self>) {
        self.llm_delta_task.take();
        self.apply_pending_llm_deltas(cx);
    }

    fn apply_pending_llm_deltas(&mut self, cx: &mut Context<Self>) {
        let deltas = std::mem::take(&mut self.pending_llm_deltas);
        if deltas.is_empty() || self.lifecycle.is_disposed() {
            return;
        }
        let delta_count = deltas.len() as u64;
        let execution_id = deltas.last().map(|envelope| envelope.execution_id.clone());
        let agents_gap = self
            .agents
            .update(cx, |agents, _| agents.record_llm_batch(&deltas));
        let classification = self
            .logs
            .update(cx, |logs, _| logs.record_llm_batch(deltas));
        self.event_count = self.event_count.saturating_add(delta_count);
        // The count is intentionally updated once per presentation batch. It remains a UI
        // activity indicator rather than an audit counter; the durable stream retains each event.
        self.status = "Agent output updated".into();
        if classification == SequenceClassification::Gap || agents_gap {
            self.needs_resync = true;
            self.agents_resync_pending |= agents_gap;
            self.status = "LLM gap detected; reloading persisted execution".into();
            if let Some(execution_id) = execution_id {
                self.resync_llm_execution(execution_id, cx);
            }
        }
        cx.notify();
    }

    fn handle_event(&mut self, event: AppEvent, cx: &mut Context<Self>) {
        if let AppEvent::Llm(envelope) = &event
            && matches!(
                envelope.event,
                ikore_core::llm::LlmStreamEvent::TokenDelta { .. }
            )
        {
            self.queue_llm_delta(envelope.clone(), cx);
            return;
        }
        if matches!(&event, AppEvent::Llm(_)) {
            self.flush_pending_llm_deltas(cx);
        }
        // Indexing progress ticks at supervisor cadence and must not steal the status bar's
        // main text (see the generic "Event received: ..." below) on every delta.
        if let AppEvent::Indexing(event) = event {
            self.apply_indexing_event(event, cx);
            return;
        }
        // Doorbells are frequent and say nothing new about themselves.
        if let AppEvent::Exchange(doorbell) = event {
            self.handle_exchange_doorbell(doorbell, cx);
            return;
        }
        if let AppEvent::ModelPull(event) = event {
            match &event {
                ModelPullEvent::Completed { model } => self.record_activity(
                    ActivityKind::Indexing,
                    format!("Embedding model {model} ready"),
                    Some(FlowTarget::Settings(
                        crate::settings::SettingsPage::Embeddings,
                    )),
                    false,
                ),
                ModelPullEvent::Failed { model, error } => self.record_activity(
                    ActivityKind::Indexing,
                    format!("Embedding model {model} failed: {error}"),
                    Some(FlowTarget::Settings(
                        crate::settings::SettingsPage::Embeddings,
                    )),
                    true,
                ),
                ModelPullEvent::Started { .. } | ModelPullEvent::Progress { .. } => {}
            }
            self.model_pull_status = Some(event);
            cx.notify();
            return;
        }
        let usage_changed = match &event {
            AppEvent::Llm(envelope) => matches!(
                envelope.event,
                ikore_core::llm::LlmStreamEvent::RouteCompleted { .. }
                    | ikore_core::llm::LlmStreamEvent::RouteFailed { .. }
                    | ikore_core::llm::LlmStreamEvent::AttemptFailed { .. }
            ),
            AppEvent::Chat(envelope) => matches!(
                envelope.kind,
                ikore_core::chat_service::ChatEventKind::TurnCompleted
                    | ikore_core::chat_service::ChatEventKind::TurnFailed
                    | ikore_core::chat_service::ChatEventKind::TurnCancelled
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
                    self.activity.push_coalesced(
                        ActivityEntry {
                            at: std::time::SystemTime::now(),
                            kind: ActivityKind::Forge,
                            summary: if event.feature_id.is_some() {
                                "Forge feature updated".into()
                            } else {
                                "Forge backlog updated".into()
                            },
                            target: Some(FlowTarget::ForgeSection(
                                crate::forge::ForgeSection::Backlog,
                            )),
                            attention: false,
                        },
                        ACTIVITY_COALESCE_WINDOW,
                    );
                    self.refresh_forge(cx);
                }
            }
            AppEvent::Notes(event) => {
                if self
                    .workspace
                    .read(cx)
                    .project_id
                    .as_ref()
                    .is_some_and(|p| p.as_ref() == event.project_id)
                {
                    // Reports what finished once the snapshot shows it (`notes.rs`).
                    self.refresh_notes(cx);
                }
            }
            AppEvent::Drawings(event) => {
                if self
                    .workspace
                    .read(cx)
                    .project_id
                    .as_ref()
                    .is_some_and(|p| p.as_ref() == event.project_id)
                    && !self.canvas_has_pending()
                {
                    self.refresh_canvas(cx);
                }
            }
            AppEvent::CanvasAuthoring(event) => {
                self.handle_canvas_authoring_changed(event.project_id, event.conversation_id, cx)
            }
            AppEvent::Voice(_) => self.refresh_voice_status(cx),
            AppEvent::DocDrift(event) => {
                if self
                    .workspace
                    .read(cx)
                    .project_id
                    .as_ref()
                    .is_some_and(|p| p.as_ref() == event.project_id)
                {
                    self.refresh_doc_drift(cx);
                }
            }
            AppEvent::WorkspaceChanged(event) => {
                self.activity.push_coalesced(
                    ActivityEntry {
                        at: std::time::SystemTime::now(),
                        kind: ActivityKind::Workspace,
                        summary: format!("{} {}", event.relative_path, event.kind).into(),
                        target: Some(FlowTarget::View(AppView::Workspace)),
                        attention: false,
                    },
                    ACTIVITY_COALESCE_WINDOW,
                );
                self.logs.update(cx, |logs, _| {
                    logs.record_workspace(event.clone());
                });
                self.handle_workspace_change(event, cx);
                self.schedule_doc_drift_refresh(cx);
            }
            AppEvent::Llm(envelope) => {
                let execution_id = envelope.execution_id.clone();
                let terminal = matches!(
                    envelope.event,
                    ikore_core::llm::LlmStreamEvent::RouteCompleted { .. }
                        | ikore_core::llm::LlmStreamEvent::RouteFailed { .. }
                );
                // Failures may open a provider cooldown; the rail's runtime card follows it.
                let failed = matches!(
                    envelope.event,
                    ikore_core::llm::LlmStreamEvent::RouteFailed { .. }
                        | ikore_core::llm::LlmStreamEvent::AttemptFailed { .. }
                );
                match &envelope.event {
                    ikore_core::llm::LlmStreamEvent::RouteCompleted { model, .. } => {
                        let stage = envelope
                            .context
                            .as_ref()
                            .map(|context| context.stage.clone())
                            .filter(|stage| !stage.is_empty())
                            .unwrap_or_else(|| "LLM".to_string());
                        self.record_activity(
                            ActivityKind::Agent,
                            format!("{stage} · {model} completed"),
                            Some(FlowTarget::View(AppView::Logs)),
                            false,
                        );
                    }
                    ikore_core::llm::LlmStreamEvent::RouteFailed { error_code, .. } => {
                        self.record_activity(
                            ActivityKind::Agent,
                            format!("LLM route failed ({error_code})"),
                            Some(FlowTarget::View(AppView::Logs)),
                            true,
                        );
                    }
                    _ => {}
                }
                let agents_gap = self
                    .agents
                    .update(cx, |agents, _| agents.record_llm(&envelope));
                let classification = self.logs.update(cx, |logs, _| logs.record_llm(envelope));
                if classification == SequenceClassification::Gap || agents_gap {
                    self.needs_resync = true;
                    self.agents_resync_pending |= agents_gap;
                    self.status = "LLM gap detected; reloading persisted execution".into();
                    self.resync_llm_execution(execution_id, cx);
                }
                if terminal || agents_gap {
                    if terminal {
                        self.refresh_pulsations(cx);
                    }
                    if self.active_view == AppView::Agents {
                        self.refresh_agents(cx);
                    } else {
                        self.refresh_agents_quiet(cx);
                    }
                }
                if failed && !self.settings_modal.open {
                    self.schedule_settings_quiet_refresh(cx);
                }
            }
            AppEvent::Terminal(envelope) => {
                if envelope.run.status.is_terminal() {
                    self.backend_read.invalidate();
                }
                let terminal = envelope.run.status.is_terminal()
                    || envelope.run.status == TerminalRunStatus::AwaitingFallback;
                if terminal {
                    let agent = envelope
                        .run
                        .agent_role
                        .map(|role| ikore_core::terminals::canonical_agent(role).name)
                        .unwrap_or("Free-form agent");
                    let status = envelope.run.status;
                    self.record_activity(
                        ActivityKind::Agent,
                        format!("{agent} · {}", crate::agents_view::run_status(status)),
                        Some(FlowTarget::View(AppView::Agents)),
                        !matches!(status, TerminalRunStatus::Completed),
                    );
                }
                let gap = self
                    .agents
                    .update(cx, |agents, _| agents.record_event(*envelope));
                // ADR-037 M2: the skipped range is re-read, not the panel.
                if let Some(gap) = gap {
                    self.repair_terminal_gap(gap, cx);
                }
                if terminal {
                    self.refresh_pulsations(cx);
                    if self.active_view == AppView::Agents {
                        self.refresh_agents(cx);
                    } else {
                        self.refresh_agents_quiet(cx);
                    }
                }
            }
            AppEvent::Chat(envelope) => {
                match envelope.kind {
                    ikore_core::chat_service::ChatEventKind::TurnCompleted => self.record_activity(
                        ActivityKind::Chat,
                        "Companion replied",
                        Some(FlowTarget::View(AppView::Chat)),
                        false,
                    ),
                    ikore_core::chat_service::ChatEventKind::TurnFailed => self.record_activity(
                        ActivityKind::Chat,
                        "Companion turn failed",
                        Some(FlowTarget::View(AppView::Chat)),
                        true,
                    ),
                    ikore_core::chat_service::ChatEventKind::TurnCancelled => self.record_activity(
                        ActivityKind::Chat,
                        "Companion turn cancelled",
                        Some(FlowTarget::View(AppView::Chat)),
                        false,
                    ),
                    _ => {}
                }
                self.handle_chat_event(envelope, cx)
            }
            // Returned early above, before the generic event-count/status preamble.
            AppEvent::Indexing(_) => unreachable!(),
            AppEvent::ModelPull(_) => unreachable!(),
            AppEvent::Exchange(_) => unreachable!(),
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
        match &event {
            IndexingEvent::Failed { error, .. } => self.record_activity(
                ActivityKind::Indexing,
                format!("Knowledge indexing failed: {error}"),
                Some(FlowTarget::Settings(
                    crate::settings::SettingsPage::Embeddings,
                )),
                true,
            ),
            // Only the transition into Idle is a milestone; the supervisor keeps repeating it.
            IndexingEvent::Idle { .. }
                if matches!(
                    self.indexing_status,
                    Some(IndexingEvent::Progress { .. } | IndexingEvent::Paused { .. })
                ) =>
            {
                self.record_activity(
                    ActivityKind::Indexing,
                    "Knowledge index up to date",
                    Some(FlowTarget::View(AppView::Vault)),
                    false,
                )
            }
            _ => {}
        }
        self.indexing_status = Some(event);
        cx.notify();
    }

    fn handle_workspace_change(&mut self, event: WorkspaceChangedEvent, cx: &mut Context<Self>) {
        let should_reload = self.workspace.update(cx, |workspace, _| {
            workspace.mark_external_change(&event.relative_path, &event.kind)
        });
        self.mark_vault_stale(cx);
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

    // Restoring runs on every launch; "already set up" would greet the developer each time.
    let notice = opened
        .greenfield
        .filter(|status| status.initialized)
        .map(|status| status.message)
        .or_else(|| {
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

/// Opens a written pulsation report.
type ReportOpener = Rc<dyn Fn(&Path, &mut gpui::App)>;

/// Where "Save as…" starts for a pulsation report: the developer's home
/// folder, never the project, whose repository the report must not enter.
pub(crate) fn report_save_directory() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use gpui::TestAppContext;
    use ikore_core::persistence_service::{Artifact, Pulsation, PulsationStage, PulsationStatus};
    use ikore_core::workspace_service::{
        GetWorkspaceVaultIndexResponse, RenameWorkspaceNoteRequest, RenameWorkspaceNoteResponse,
        SearchMatch, SearchWorkspaceTextRequest, SearchWorkspaceTextResponse, VaultNoteSummary,
    };

    use super::*;
    use crate::pulsations::PulsationsFuture;
    use crate::vault::VaultFuture;
    use crate::workspace::{WorkspaceFileSnapshot, WorkspaceFuture, WorkspaceOperationError};

    #[gpui::test]
    async fn shutdown_retains_only_chat_send_and_create_admissions(cx: &mut TestAppContext) {
        let completed = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let model = cx.new(AppModel::new_for_test);
        let admissions = model.update(cx, |model, cx| {
            for (operation, label) in [
                (ChatOperation::Load, "load"),
                (ChatOperation::Delete, "delete"),
                (ChatOperation::Cancel, "cancel"),
                (ChatOperation::Send, "send"),
            ] {
                let completed = completed.clone();
                let task = cx.spawn(async move |_, _| completed.borrow_mut().push(label));
                model.chat_tasks.insert((label.into(), operation), task);
            }
            let completed = completed.clone();
            model.chat_create_task =
                Some(cx.spawn(async move |_, _| completed.borrow_mut().push("create")));
            let admissions = model.take_chat_admissions_for_shutdown();
            assert_eq!(admissions.len(), 2);
            assert!(model.chat_tasks.is_empty());
            assert!(model.chat_create_task.is_none());
            admissions
        });
        for admission in admissions {
            admission.await;
        }
        let mut completed = completed.borrow().clone();
        completed.sort_unstable();
        assert_eq!(completed, ["create", "send"]);
    }

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
        ) -> WorkspaceFuture<ikore_core::orchestration::brownfield::BrownfieldOnboardingResult>
        {
            self.consent_requests
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((project_id.clone(), consent));
            Box::pin(async move {
                Ok(
                    ikore_core::orchestration::brownfield::BrownfieldOnboardingResult {
                        status: BrownfieldOnboardingStatus {
                            project_id,
                            consent: if consent {
                                ikore_core::orchestration::brownfield::BrownfieldConsentState::Granted
                            } else {
                                ikore_core::orchestration::brownfield::BrownfieldConsentState::Declined
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
                    consent: ikore_core::orchestration::brownfield::BrownfieldConsentState::Granted,
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
        snapshot_barrier: Arc<Mutex<Option<tokio::sync::oneshot::Receiver<()>>>>,
        artifact_calls: Arc<Mutex<Vec<String>>>,
        create_calls: Arc<Mutex<Vec<(String, String)>>>,
        cancel_calls: Arc<Mutex<Vec<String>>>,
        cleanup_preview_calls: Arc<Mutex<Vec<String>>>,
        cleanup_calls:
            Arc<Mutex<Vec<ikore_core::orchestration::CleanupFailedPulsationFilesRequest>>>,
        cleanup_plan: Arc<Mutex<Option<ikore_core::orchestration::FailedPulsationCleanupPlan>>>,
        report_calls: Arc<Mutex<Vec<ReportCall>>>,
        report_error: Arc<Mutex<Option<String>>>,
        snapshot: Arc<Mutex<PulsationRuntimeSnapshot>>,
        artifacts: Arc<Mutex<Vec<Artifact>>>,
        resolution_chips: Arc<Mutex<Vec<ikore_core::orchestration::SpecificationQuestionSet>>>,
    }

    impl RecordingPulsationsBackend {
        fn new() -> Self {
            Self {
                actions: Arc::new(Mutex::new(Vec::new())),
                snapshot_calls: Arc::new(Mutex::new(0)),
                snapshot_barrier: Default::default(),
                artifact_calls: Arc::new(Mutex::new(Vec::new())),
                create_calls: Arc::new(Mutex::new(Vec::new())),
                cancel_calls: Arc::new(Mutex::new(Vec::new())),
                cleanup_preview_calls: Arc::new(Mutex::new(Vec::new())),
                cleanup_calls: Arc::new(Mutex::new(Vec::new())),
                cleanup_plan: Arc::new(Mutex::new(None)),
                report_calls: Arc::new(Mutex::new(Vec::new())),
                report_error: Arc::new(Mutex::new(None)),
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
                    active_jobs: Vec::new(),
                    brownfield_coverage: None,
                })),
                artifacts: Arc::new(Mutex::new(vec![test_artifact("Plan")])),
                resolution_chips: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl PulsationsBackend for RecordingPulsationsBackend {
        fn review_documents(
            &self,
            _checkpoint: ikore_core::persistence_service::HitlCheckpoint,
        ) -> PulsationsFuture<Vec<ikore_core::persistence_service::HitlReviewDocument>> {
            Box::pin(async { Ok(Vec::new()) })
        }
        fn stage_failure_detail(
            &self,
            checkpoint_id: String,
        ) -> PulsationsFuture<ikore_core::persistence_service::StageFailureDetail> {
            Box::pin(async move {
                Ok(ikore_core::persistence_service::StageFailureDetail {
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
            _request: ikore_core::orchestration::ConfirmBaseReconciliationRequest,
        ) -> PulsationsFuture<()> {
            Box::pin(async { Ok(()) })
        }
        fn runtime_snapshot(
            &self,
            _project_id: String,
            _detail_pulsation_id: Option<String>,
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
            let barrier = self.snapshot_barrier.lock().unwrap().take();
            Box::pin(async move {
                if let Some(barrier) = barrier {
                    let _ = barrier.await;
                }
                Ok(snapshot)
            })
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

        fn resolution_chips(
            &self,
            _project_id: String,
        ) -> PulsationsFuture<Vec<ikore_core::orchestration::SpecificationQuestionSet>> {
            let chips = self
                .resolution_chips
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            Box::pin(async move { Ok(chips) })
        }

        fn cancel_jobs(&self, pulsation_id: String) -> PulsationsFuture<usize> {
            self.cancel_calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(pulsation_id);
            Box::pin(async { Ok(1) })
        }

        fn cleanup_preview(
            &self,
            pulsation_id: String,
        ) -> PulsationsFuture<ikore_core::orchestration::FailedPulsationCleanupPlan> {
            self.cleanup_preview_calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(pulsation_id.clone());
            let plan = self
                .cleanup_plan
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            Box::pin(async move {
                plan.ok_or_else(|| "no cleanup preview scripted for this test".to_string())
            })
        }

        fn cleanup_files(
            &self,
            request: ikore_core::orchestration::CleanupFailedPulsationFilesRequest,
        ) -> PulsationsFuture<ikore_core::orchestration::FailedPulsationCleanupReport> {
            let pulsation_id = request.pulsation_id.clone();
            self.cleanup_calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(request);
            Box::pin(async move {
                Ok(ikore_core::orchestration::FailedPulsationCleanupReport {
                    pulsation_id,
                    removed_files: vec!["src/generated.rs".into()],
                    removed_directories: 1,
                    removed_candidate_dirs: 0,
                    kept_files: 1,
                })
            })
        }

        fn write_report(
            &self,
            pulsation_id: String,
            copy_to: Option<PathBuf>,
        ) -> PulsationsFuture<PathBuf> {
            self.report_calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((pulsation_id.clone(), copy_to.clone()));
            let error = self
                .report_error
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            Box::pin(async move {
                match error {
                    Some(error) => Err(error),
                    None => Ok(copy_to.unwrap_or_else(|| written_report(&pulsation_id))),
                }
            })
        }
    }

    /// A pulsation id and the copy target of one `write_report` call.
    type ReportCall = (String, Option<PathBuf>);

    fn written_report(pulsation_id: &str) -> PathBuf {
        PathBuf::from("data")
            .join("reports")
            .join(pulsation_id)
            .join("pulsation-report.html")
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
            kind: ikore_core::workspace_service::FileNodeKind::File,
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
            let before = cx.read(|cx| model.read(cx).active_view());
            model.update(cx, |model, cx| model.select_view(view, cx));
            // Settings opens its modal over the current view instead of becoming the view.
            let expected = if view == AppView::Settings {
                assert!(cx.read(|cx| model.read(cx).settings_modal.open));
                before
            } else {
                view
            };
            assert_eq!(cx.read(|cx| model.read(cx).active_view()), expected);
            cx.read(|cx| {
                let model = model.read(cx);
                assert_eq!(model.navigation.read(cx).active, expected);
            });
        }
    }

    #[gpui::test]
    fn an_applied_agents_snapshot_clears_a_terminal_gap_but_not_a_lagged_stream(
        cx: &mut TestAppContext,
    ) {
        let model = cx.new(AppModel::new_for_test);
        model.update(cx, |model, _| {
            // A terminal gap alone: the agents resync ends it.
            model.needs_resync = true;
            model.agents_resync_pending = true;
            model.finish_agents_resync();
            assert!(!model.needs_resync());
            assert!(!model.agents_resync_pending);

            // An ordinary snapshot with no gap outstanding leaves an unrelated resync alone.
            model.needs_resync = true;
            model.finish_agents_resync();
            assert!(model.needs_resync());

            // A lagged broadcast is still being resynchronized: it keeps the flag.
            model.event_resync_pending = true;
            model.agents_resync_pending = true;
            model.finish_agents_resync();
            assert!(model.needs_resync());
            assert!(!model.agents_resync_pending);
        });
    }

    #[gpui::test]
    fn quiet_agent_refresh_never_flips_the_panel_into_loading(cx: &mut TestAppContext) {
        let model = cx.new(AppModel::new_for_test);
        model.update(cx, |model, cx| {
            model.workspace.update(cx, |workspace, _| {
                workspace.project_id = Some("project-1".into());
            });
            model.select_view(AppView::Vault, cx);
            model.refresh_agents_quiet(cx);
            let agents = model.agents.read(cx);
            assert!(!agents.loading);
            assert!(!agents.busy);

            // A loud read in flight wins: the quiet one yields instead of bumping the generation.
            let before = model.agents.update(cx, |agents, _| agents.begin(true));
            model.refresh_agents_quiet(cx);
            assert_eq!(model.agents.read(cx).generation, before);
            assert!(model.agents.read(cx).loading);
            assert_eq!(model.agents_refresh_pending, Some((false, true)));
        });
    }

    #[gpui::test]
    fn rail_groups_cover_every_workbench_view_exactly_once(cx: &mut TestAppContext) {
        let model = cx.new(AppModel::new_for_test);
        assert_eq!(cx.read(|cx| model.read(cx).active_view()), AppView::Home);
        let mut grouped: Vec<AppView> = AppTab::RAIL_ORDER
            .iter()
            .flat_map(|tab| tab.sub_views().iter().copied())
            .collect();
        assert_eq!(
            grouped.len(),
            AppView::ALL.len() - 2,
            "Home and Settings are ungrouped"
        );
        grouped.sort_by_key(|view| view.id());
        grouped.dedup_by_key(|view| view.id());
        assert_eq!(grouped.len(), AppView::ALL.len() - 2);
        assert_eq!(AppTab::from_view(AppView::Home), None);
        assert_eq!(AppTab::from_view(AppView::Settings), None);
        assert_eq!(AppView::from_id("home"), AppView::Home);
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

            // With the Settings modal open the shortcut walks its pages instead.
            model.select_view(AppView::Settings, cx);
            assert!(model.settings_modal.open);
            model.cycle_current_section(1, cx);
            assert_eq!(
                model.settings.read(cx).page,
                crate::settings::SettingsPage::Usage
            );
            model.cycle_current_section(-1, cx);
            model.cycle_current_section(-1, cx);
            assert_eq!(
                model.settings.read(cx).page,
                crate::settings::SettingsPage::About
            );

            // Another view closes Settings; a view without a section strip (Vault) ignores the
            // shortcut instead of panicking.
            model.select_view(AppView::Vault, cx);
            assert!(!model.settings_modal.open);
            model.cycle_current_section(1, cx);
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
            model.select_view(AppView::Logs, cx);
            // Settings is a modal over the view; the view is what persists.
            model.select_view(AppView::Settings, cx);
            model.set_context_rail_collapsed(true, cx);
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
        assert_eq!(request.active_view, "logs");
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
            assert_eq!(model.canvas.project_id.as_deref(), Some("project-1"));
            assert!(model.navigation.read(cx).context_rail_collapsed);
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
            assert!(round_trip.sidebar_collapsed);
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
            assert_eq!(model.navigation.read(cx).active, AppView::Vault);
            assert!(model.navigation.read(cx).context_rail_collapsed);
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
                            ikore_core::orchestration::brownfield::BrownfieldConsentState::NotDecided,
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
    fn real_pulsation_completes_under_every_view_with_the_same_four_checkpoints(
        cx: &mut TestAppContext,
    ) {
        use ikore_core::persistence_service::{
            CreatePulsationRequest, EnsureProjectForWorkspaceRequest,
            GetPulsationRuntimeSnapshotRequest, PulsationStatus,
        };
        use ikore_test_support::pulsation_benchmark::{
            configure_loopback, force_api_direct, stage_response,
        };
        use ikore_test_support::{CoreTestHarness, ScriptedHttpServer};
        let harness = CoreTestHarness::new();
        let core = harness.core();
        let server = ScriptedHttpServer::respond_with(stage_response);
        configure_loopback(core.persistence(), &server);
        let project = core
            .persistence()
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: harness.workspace_dir().to_string_lossy().into_owned(),
                preferred_name: Some("View independence fixture".into()),
            })
            .unwrap()
            .project;
        force_api_direct(core.persistence(), &project.id);
        let handle = core.runtime_handle().clone();
        let _entered = handle.enter();
        let model = cx.new(|cx| AppModel::new(core.clone(), cx));
        let restore_deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            cx.run_until_parked();
            if cx.read(|cx| !model.read(cx).hydrating) {
                break;
            }
            assert!(
                std::time::Instant::now() < restore_deadline,
                "fixture session restore timed out"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        model.update(cx, |model, cx| {
            model.workspace.update(cx, |workspace, _| {
                workspace.project_id = Some(project.id.clone().into());
            });
        });
        for view in [
            AppView::Agents,
            AppView::Pulsations,
            AppView::Forge,
            AppView::Workspace,
            AppView::Chat,
            AppView::Vault,
            AppView::Logs,
            AppView::Settings,
        ] {
            model.update(cx, |model, cx| model.select_view(view, cx));
            let pulse = core
                .persistence()
                .create_pulsation(CreatePulsationRequest {
                    project_id: project.id.clone(),
                    feature_name:
                        "Describe expired-token validation returning status 401. Fixture only."
                            .into(),
                })
                .unwrap()
                .pulsation;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            let mut approvals = 0;
            loop {
                cx.run_until_parked();
                model.update(cx, |model, cx| {
                    // Settings opens over the previous view (Logs) instead of replacing it.
                    if view == AppView::Settings {
                        assert_eq!(model.active_view(), AppView::Logs);
                        assert!(model.settings_modal.open);
                    } else {
                        assert_eq!(model.active_view(), view);
                    }
                    model.refresh_pulsations(cx);
                });
                let snapshot = core
                    .persistence()
                    .get_pulsation_runtime_snapshot(GetPulsationRuntimeSnapshotRequest {
                        project_id: project.id.clone(),
                    })
                    .unwrap();
                let current = snapshot
                    .snapshot
                    .pulsations
                    .iter()
                    .find(|item| item.id == pulse.id)
                    .unwrap();
                assert_ne!(current.status, PulsationStatus::Failed, "view={view:?}");
                if current.status == PulsationStatus::Done {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "view={view:?} stage={:?} status={:?}",
                    current.stage,
                    current.status
                );
                if let Ok(checkpoint) = core.persistence().get_hitl_checkpoint(&pulse.id) {
                    core.persistence()
                        .apply_hitl_decision(
                            ikore_core::persistence_service::ApplyHitlDecisionRequest {
                                pulsation_id: pulse.id.clone(),
                                checkpoint_id: checkpoint.checkpoint_id,
                                expected_state_version: checkpoint.state_version,
                                feedback: None,
                                decision: ikore_core::persistence_service::HitlDecision::Approve,
                            },
                        )
                        .unwrap();
                    approvals += 1;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            assert_eq!(approvals, 4, "view={view:?}");
        }
        model.update(cx, |model, _| {
            model.lifecycle.dispose();
        });
    }

    #[gpui::test]
    fn overlapping_snapshot_requests_do_not_restore_an_earlier_selection(cx: &mut TestAppContext) {
        let backend = Arc::new(RecordingPulsationsBackend::new());
        let mut other = test_pulsation();
        other.id = "pulse-2".into();
        backend.snapshot.lock().unwrap().pulsations.push(other);
        let model = cx.new(|cx| {
            AppModel::new_for_test_with_domain_backends(
                backend.clone(),
                Arc::new(RecordingVaultBackend::new()),
                cx,
            )
        });
        model.update(cx, |model, cx| {
            configure_domain_workspace(model, cx);
            model.apply_pulsation_snapshot(
                "project-1".into(),
                backend.snapshot.lock().unwrap().clone(),
                Some("pulse-1"),
                cx,
            );
        });
        let (release, barrier) = tokio::sync::oneshot::channel();
        *backend.snapshot_barrier.lock().unwrap() = Some(barrier);
        model.update(cx, AppModel::refresh_pulsations);
        cx.run_until_parked();
        model.update(cx, |model, cx| {
            for _ in 0..10 {
                model.refresh_pulsations(cx);
            }
            model.select_pulsation("pulse-2", cx);
        });
        assert_eq!(*backend.snapshot_calls.lock().unwrap(), 1);
        release.send(()).unwrap();
        cx.run_until_parked();
        assert_eq!(*backend.snapshot_calls.lock().unwrap(), 2);
        cx.read(|cx| {
            assert_eq!(
                model.read(cx).pulsations.read(cx).active_id().as_deref(),
                Some("pulse-2")
            )
        });
    }

    #[gpui::test]
    fn runtime_observation_and_lag_recovery_preserve_all_views_and_dirty_buffers(
        cx: &mut TestAppContext,
    ) {
        let backend = Arc::new(RecordingPulsationsBackend::new());
        let vault = Arc::new(RecordingVaultBackend::new());
        let model =
            cx.new(|cx| AppModel::new_for_test_with_domain_backends(backend.clone(), vault, cx));
        model.update(cx, |model, cx| {
            configure_domain_workspace(model, cx);
            configure_editable_tab(model, cx);
        });
        let views = [
            AppView::Agents,
            AppView::Pulsations,
            AppView::Forge,
            AppView::Workspace,
            AppView::Chat,
            AppView::Vault,
            AppView::Logs,
            AppView::Settings,
        ];
        for view in views.into_iter().cycle().take(16) {
            model.update(cx, |model, cx| {
                model.select_view(view, cx);
                let revision = model
                    .workspace
                    .read(cx)
                    .active_tab()
                    .map(|tab| tab.content.clone());
                model.request_event_resync(cx);
                model.request_event_resync(cx);
                let mut snapshot = backend.snapshot.lock().unwrap().clone();
                snapshot.main_agents_count = 1;
                model.apply_pulsation_snapshot("project-1".into(), snapshot, None, cx);
                // Settings opens over the previous view (Logs) instead of replacing it.
                if view == AppView::Settings {
                    assert_eq!(model.active_view(), AppView::Logs);
                    assert!(model.settings_modal.open);
                } else {
                    assert_eq!(model.active_view(), view);
                }
                assert_eq!(
                    model
                        .workspace
                        .read(cx)
                        .active_tab()
                        .map(|tab| tab.content.clone()),
                    revision
                );
                assert_eq!(model.pulsations.read(cx).running_jobs, 2);
                assert!(model.event_resync_pending);
                model.refresh_pulsation_artifacts("pulse-1".into(), cx);
            });
            cx.run_until_parked();
        }
        // Only opening Pulsations reads artifacts. Other views still receive runtime state.
        assert!(backend.artifact_calls.lock().unwrap().len() <= 2);
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
            model.select_view(AppView::Pulsations, cx);
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

        model.update(cx, AppModel::refresh_pulsations);
        cx.run_until_parked();
        assert_eq!(
            artifact_calls.lock().unwrap().len(),
            1,
            "unchanged snapshots reuse persisted artifacts"
        );
        pulsations_backend.snapshot.lock().unwrap().pulsations[0].updated_at =
            "next-revision".into();
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
    ) -> ikore_core::orchestration::PulsationCoordinationSnapshot {
        use ikore_core::orchestration::*;
        PulsationCoordinationSnapshot {
            sanitization: Default::default(),
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
            stage_execution: None,
            layers: None,
            attestation: None,
            attested: false,
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
        assert_eq!(request.answers[0].text_answer, "JWT assinado");
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

    struct RecordingResolutionChatBackend {
        conversation_id: String,
        sent: Mutex<Vec<(String, String)>>,
        reply: Mutex<String>,
    }

    impl RecordingResolutionChatBackend {
        fn new(conversation_id: &str, reply: &str) -> Self {
            Self {
                conversation_id: conversation_id.into(),
                sent: Mutex::new(Vec::new()),
                reply: Mutex::new(reply.into()),
            }
        }

        fn message(
            &self,
            generation: &str,
            content: &str,
        ) -> ikore_core::chat_service::ChatMessage {
            ikore_core::chat_service::ChatMessage {
                id: format!("assistant-{generation}"),
                conversation_id: self.conversation_id.clone(),
                ordinal: 1,
                role: "assistant".into(),
                content: content.into(),
                status: "completed".into(),
                execution_id: Some(format!("execution-{generation}")),
                generation_id: Some(generation.into()),
                last_sequence: 1,
                input_tokens: None,
                output_tokens: None,
                estimated_cost_usd: None,
                error_code: None,
                error_message: None,
                target_label: None,
                created_at: "now".into(),
                updated_at: "now".into(),
            }
        }
    }

    impl ChatBackend for RecordingResolutionChatBackend {
        fn hydrate(
            &self,
            _: String,
            _: Option<String>,
        ) -> crate::chat::ChatFuture<crate::chat::ChatHydration> {
            Box::pin(async { Err(crate::errors::UiCommandError::worker("unused")) })
        }

        fn load_conversation(
            &self,
            _: String,
        ) -> crate::chat::ChatFuture<ikore_core::chat_service::GetChatConversationResponse>
        {
            let conversation_id = self.conversation_id.clone();
            Box::pin(async move {
                Ok(ikore_core::chat_service::GetChatConversationResponse {
                    conversation: test_chat_conversation(&conversation_id),
                    messages: Vec::new(),
                    tool_calls: Vec::new(),
                })
            })
        }

        fn create_conversation(
            &self,
            _: String,
            _: ikore_core::chat_service::ChatExecutionTargetRequest,
        ) -> crate::chat::ChatFuture<ikore_core::chat_service::GetChatConversationResponse>
        {
            Box::pin(async { Err(crate::errors::UiCommandError::worker("unused")) })
        }

        fn delete_conversation(&self, _: String) -> crate::chat::ChatFuture<bool> {
            Box::pin(async { Err(crate::errors::UiCommandError::worker("unused")) })
        }

        fn send_message(
            &self,
            conversation_id: String,
            content: String,
            _: Option<ikore_core::chat_service::ChatExecutionTargetRequest>,
        ) -> crate::chat::ChatFuture<(
            ikore_core::chat_service::SendChatMessageResponse,
            ikore_core::chat_service::GetChatConversationResponse,
        )> {
            self.sent
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((conversation_id.clone(), content));
            let generation = format!("generation-{}", self.sent.lock().unwrap().len());
            let reply = self
                .reply
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            let assistant = self.message(&generation, &reply);
            Box::pin(async move {
                Ok((
                    ikore_core::chat_service::SendChatMessageResponse {
                        execution_id: assistant.execution_id.clone().unwrap(),
                        generation_id: generation,
                        user_message_id: "user-1".into(),
                        assistant_message_id: assistant.id.clone(),
                    },
                    ikore_core::chat_service::GetChatConversationResponse {
                        conversation: test_chat_conversation(&conversation_id),
                        messages: vec![assistant],
                        tool_calls: Vec::new(),
                    },
                ))
            })
        }

        fn cancel_turn(&self, _: String) -> crate::chat::ChatFuture<bool> {
            Box::pin(async { Ok(true) })
        }

        fn turn_snapshot(
            &self,
            _: String,
        ) -> crate::chat::ChatFuture<ikore_core::chat_service::GetChatTurnSnapshotResponse>
        {
            Box::pin(async { Err(crate::errors::UiCommandError::worker("unused")) })
        }

        fn update_conversation_target(
            &self,
            _: String,
            _: ikore_core::chat_service::ChatExecutionTargetRequest,
        ) -> crate::chat::ChatFuture<ikore_core::chat_service::GetChatConversationResponse>
        {
            Box::pin(async { Err(crate::errors::UiCommandError::worker("unused")) })
        }

        fn save_default_target(
            &self,
            _: String,
            _: ikore_core::chat_service::ChatExecutionTargetRequest,
        ) -> crate::chat::ChatFuture<()> {
            Box::pin(async { Ok(()) })
        }
    }

    fn test_chat_conversation(conversation_id: &str) -> ikore_core::chat_service::ChatConversation {
        ikore_core::chat_service::ChatConversation {
            id: conversation_id.into(),
            project_id: "project-1".into(),
            connection_id: Some("connection-a".into()),
            provider_id: "openai".into(),
            model_id: "gpt-fixture".into(),
            transport: ChatTransportKind::Api,
            cli_backend: None,
            model_profile: None,
            model_source: ikore_core::terminals::CliModelSelectionSource::Default,
            title: "Specification clarification".into(),
            connection_available: true,
            created_at: "now".into(),
            updated_at: "now".into(),
        }
    }

    fn test_question_set(
        id: &str,
        pulsation_id: &str,
    ) -> ikore_core::orchestration::SpecificationQuestionSet {
        ikore_core::orchestration::SpecificationQuestionSet {
            id: id.into(),
            checkpoint_id: format!("checkpoint-{id}"),
            fence: ikore_core::orchestration::ExecutionFence {
                project_id: "project-1".into(),
                pulsation_id: Some(pulsation_id.into()),
                input_revision: Some(3),
                job_id: "job-1".into(),
                generation: 1,
                state_version: 9,
            },
            conversation_id: format!("host-conv-{id}"),
            questions: vec![ikore_core::orchestration::SpecificationQuestion {
                id: format!("question-{id}"),
                question: "Where should this file live?".into(),
                rationale: "No planned-destination handle exists yet.".into(),
            }],
        }
    }

    #[gpui::test]
    fn the_pulsations_resolution_entry_opens_a_dock_card_without_a_conversation_or_model_call(
        cx: &mut TestAppContext,
    ) {
        let backend = Arc::new(RecordingPulsationsBackend::new());
        let chat_backend = Arc::new(RecordingResolutionChatBackend::new("unused", "unused"));
        let model = cx.new(|cx| {
            AppModel::new_for_test_with_resolution_backends(backend, chat_backend.clone(), cx)
        });
        model.update(cx, |model, cx| {
            model.workspace.update(cx, |workspace, _| {
                workspace.project_id = Some("project-1".into())
            });
            model.resolution.update(cx, |resolution, _| {
                resolution.apply_chips(vec![test_question_set("set-a", "pulse-a")])
            });
            model.toggle_resolution_chip("set-a".into(), cx);
            model.toggle_resolution_chip("set-a".into(), cx);
            assert!(model.resolution.read(cx).expanded.is_none());
            assert!(model.resolution.read(cx).conversation.is_none());
            assert_eq!(model.companion_dock.windows.len(), 1);
            assert_eq!(
                model.companion_dock.windows[0].decision_id.as_deref(),
                Some("checkpoint:checkpoint-set-a")
            );
            assert!(model.companion_dock.windows[0].conversation_id.is_none());
        });
        cx.run_until_parked();
        assert!(chat_backend.sent.lock().unwrap().is_empty());
    }

    #[gpui::test]
    fn explicit_resolution_synthesis_preserves_guarded_answer_submission(cx: &mut TestAppContext) {
        let pulsations_backend = Arc::new(RecordingPulsationsBackend::new());
        pulsations_backend
            .resolution_chips
            .lock()
            .unwrap()
            .push(test_question_set("set-a", "pulse-a"));
        let chat_backend = Arc::new(RecordingResolutionChatBackend::new(
            "resolution-conversation-1",
            "Prose summary.\nFILES_NEW: src/new_module.rs",
        ));
        let model = cx.new(|cx| {
            AppModel::new_for_test_with_resolution_backends(
                pulsations_backend.clone(),
                chat_backend.clone(),
                cx,
            )
        });
        model.update(cx, |m, cx| {
            m.workspace.update(cx, |workspace, _| {
                workspace.project_id = Some("project-1".into())
            });
            m.refresh_resolution_chips(cx);
        });
        cx.run_until_parked();
        model.read_with(cx, |m, cx| {
            assert_eq!(m.resolution.read(cx).chips.len(), 1);
        });

        model.update(cx, |m, cx| m.toggle_resolution_chip("set-a".into(), cx));
        cx.run_until_parked();
        assert!(
            chat_backend.sent.lock().unwrap().is_empty(),
            "opening a resolution conversation must not synthesize a human message or call a model"
        );
        model.update(cx, |model, cx| {
            assert!(model.resolution.read(cx).expanded.is_none());
            assert!(
                model
                    .companion_dock
                    .windows
                    .iter()
                    .any(|window| window.decision_id.as_deref()
                        == Some("checkpoint:checkpoint-set-a")
                        && window.conversation_id.is_none())
            );
            // Exercise the existing explicit synthesis and guarded submission separately
            // from opening a decision. The dock's opening path never creates a turn.
            model.resolution.update(cx, |resolution, _| {
                resolution.expanded = Some("set-a".into());
                resolution.conversation = Some(crate::resolution::ResolutionConversation {
                    conversation_id: "resolution-conversation-1".into(),
                    messages: Vec::new(),
                    synthesized_draft: None,
                    pending_synthesis_generation_id: None,
                    busy: false,
                });
            });
        });
        model.read_with(cx, |m, cx| {
            let resolution = m.resolution.read(cx);
            assert_eq!(resolution.expanded.as_deref(), Some("set-a"));
            let conversation = resolution
                .conversation
                .as_ref()
                .expect("conversation loaded");
            assert_eq!(conversation.conversation_id, "resolution-conversation-1");
            assert!(conversation.synthesized_draft.is_none());
        });

        model.update(cx, |m, cx| m.synthesize_resolution_reply(cx));
        cx.run_until_parked();
        model.read_with(cx, |m, cx| {
            let resolution = m.resolution.read(cx);
            let conversation = resolution.conversation.as_ref().unwrap();
            assert_eq!(
                conversation.synthesized_draft.as_deref(),
                Some("Prose summary.\nFILES_NEW: src/new_module.rs")
            );
            assert_eq!(
                m.resolution_synthesis_input.read(cx).value(),
                "Prose summary.\nFILES_NEW: src/new_module.rs"
            );
        });

        model.update(cx, |m, cx| m.confirm_resolution_answer(cx));
        cx.run_until_parked();
        let actions = pulsations_backend.actions.lock().unwrap();
        assert_eq!(actions.len(), 1);
        let crate::pulsations::PulsationAction::Answer(request) = &actions[0] else {
            panic!("answer action")
        };
        assert_eq!(request.question_set_id, "set-a");
        assert_eq!(
            request.answers[0].text_answer,
            "Prose summary.\nFILES_NEW: src/new_module.rs"
        );
        drop(actions);
        model.read_with(cx, |m, cx| {
            let resolution = m.resolution.read(cx);
            assert!(resolution.chips.is_empty());
            assert!(resolution.expanded.is_none());
        });
    }

    #[gpui::test]
    fn learning_ui_submits_the_displayed_generation_once(cx: &mut TestAppContext) {
        use ikore_core::orchestration::{LearningRunSnapshot, LearningState};
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
        use ikore_core::orchestration::{
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

    fn scripted_cleanup_plan() -> ikore_core::orchestration::FailedPulsationCleanupPlan {
        use ikore_core::orchestration::{CleanupDisposition, FailedPulsationCleanupEntry};
        ikore_core::orchestration::FailedPulsationCleanupPlan {
            pulsation_id: "pulse-1".into(),
            project_id: "project-1".into(),
            entries: vec![
                FailedPulsationCleanupEntry {
                    relative_path: "src/generated.rs".into(),
                    disposition: CleanupDisposition::Remove,
                    detail: None,
                },
                FailedPulsationCleanupEntry {
                    relative_path: "src/lib.rs".into(),
                    disposition: CleanupDisposition::KeepPreexisting,
                    detail: None,
                },
            ],
            candidate_dirs: vec![],
            plan_token: "token-1".into(),
        }
    }

    #[gpui::test]
    fn failed_pulsation_cleanup_previews_once_and_removes_only_after_a_second_click(
        cx: &mut TestAppContext,
    ) {
        let pulsations_backend = Arc::new(RecordingPulsationsBackend::new());
        *pulsations_backend
            .cleanup_plan
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(scripted_cleanup_plan());
        pulsations_backend
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pulsations[0]
            .status = PulsationStatus::Failed;
        let preview_calls = Arc::clone(&pulsations_backend.cleanup_preview_calls);
        let cleanup_calls = Arc::clone(&pulsations_backend.cleanup_calls);
        let model = cx.new(|cx| {
            AppModel::new_for_test_with_domain_backends(
                pulsations_backend,
                Arc::new(RecordingVaultBackend::new()),
                cx,
            )
        });
        model.update(cx, |model, cx| {
            configure_domain_workspace(model, cx);
            model.select_view(AppView::Pulsations, cx);
            model.refresh_pulsations(cx);
        });
        cx.run_until_parked();
        // Two refreshes of the same failed pulsation read the workspace once.
        model.update(cx, |model, cx| model.refresh_pulsations(cx));
        cx.run_until_parked();
        assert_eq!(
            *preview_calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec!["pulse-1"]
        );
        cx.read(|cx| {
            let state = model.read(cx).pulsations.read(cx);
            let plan = state.cleanup.plan.as_ref().expect("preview loaded");
            assert_eq!(plan.removable_files(), 1);
            assert_eq!(plan.kept_files(), 1);
            assert!(state.cleanup.can_remove());
            assert!(!state.cleanup.armed);
        });

        // First click arms; nothing reaches the host.
        model.update(cx, |model, cx| model.request_cleanup_failed_files(cx));
        cx.run_until_parked();
        assert!(
            cleanup_calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        );
        cx.read(|cx| {
            let state = model.read(cx).pulsations.read(cx);
            assert!(state.cleanup.armed);
            assert_eq!(
                state.notice.as_deref(),
                Some("Confirm again to remove the files this pulsation created.")
            );
        });

        // Second click sends the previewed plan token.
        model.update(cx, |model, cx| model.request_cleanup_failed_files(cx));
        cx.run_until_parked();
        let calls = cleanup_calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].pulsation_id, "pulse-1");
        assert_eq!(calls[0].expected_plan_token, "token-1");
        cx.read(|cx| {
            let state = model.read(cx).pulsations.read(cx);
            assert!(!state.cleanup.armed);
            assert!(!state.cleanup.busy);
            assert_eq!(
                state.notice.as_deref(),
                Some("1 file(s) removed, 1 kept, 0 staging dir(s) cleared. Kept files stay in the base review.")
            );
        });
        // The cleanup re-reads the preview so the card reflects the new disk state.
        assert_eq!(
            preview_calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len(),
            2
        );
    }

    /// ADR-027 M6: the report card does nothing before the report exists,
    /// opens the written file, saves a copy only where the developer chose,
    /// and shows a failure on the card.
    #[gpui::test]
    fn the_pulsation_report_opens_or_saves_only_once_available(cx: &mut TestAppContext) {
        use crate::pulsations::ReportNotice;
        let pulsations_backend = Arc::new(RecordingPulsationsBackend::new());
        let report_calls = Arc::clone(&pulsations_backend.report_calls);
        let report_error = Arc::clone(&pulsations_backend.report_error);
        let snapshot = Arc::clone(&pulsations_backend.snapshot);
        let opened: Rc<std::cell::RefCell<Vec<PathBuf>>> = Rc::default();
        let model = cx.new(|cx| {
            AppModel::new_for_test_with_domain_backends(
                pulsations_backend,
                Arc::new(RecordingVaultBackend::new()),
                cx,
            )
        });
        model.update(cx, |model, cx| {
            let opened = Rc::clone(&opened);
            model.report_opener = Rc::new(move |path, _| opened.borrow_mut().push(path.into()));
            configure_domain_workspace(model, cx);
            model.select_view(AppView::Pulsations, cx);
            model.refresh_pulsations(cx);
        });
        cx.run_until_parked();
        let notice = |cx: &mut TestAppContext| {
            cx.read(|cx| {
                let report = &model.read(cx).pulsations.read(cx).report;
                assert!(!report.busy);
                report.notice.clone()
            })
        };

        // Running at Code: neither action reaches the host or asks for a path.
        model.update(cx, |model, cx| {
            model.open_pulsation_report(cx);
            model.save_pulsation_report(cx);
        });
        cx.run_until_parked();
        assert!(report_calls.lock().unwrap().is_empty());
        assert!(!cx.did_prompt_for_new_path());
        assert_eq!(notice(cx), None);

        {
            let mut snapshot = snapshot.lock().unwrap();
            snapshot.pulsations[0].stage = PulsationStage::Integrator;
            snapshot.pulsations[0].status = PulsationStatus::Checkpoint;
        }
        model.update(cx, |model, cx| model.refresh_pulsations(cx));
        cx.run_until_parked();
        model.update(cx, |model, cx| model.open_pulsation_report(cx));
        cx.run_until_parked();
        let written = written_report("pulse-1");
        assert_eq!(
            *report_calls.lock().unwrap(),
            [("pulse-1".to_owned(), None)]
        );
        assert_eq!(*opened.borrow(), std::slice::from_ref(&written));
        assert_eq!(notice(cx), Some(ReportNotice::Opened(written)));

        // A cancelled "Save as…" writes nothing; a chosen path gets a copy
        // and nothing opens.
        model.update(cx, |model, cx| model.save_pulsation_report(cx));
        cx.simulate_new_path_selection(|_| None);
        cx.run_until_parked();
        assert_eq!(report_calls.lock().unwrap().len(), 1);
        let copy = PathBuf::from("copies").join("report.html");
        model.update(cx, |model, cx| model.save_pulsation_report(cx));
        cx.simulate_new_path_selection({
            let copy = copy.clone();
            move |_| Some(copy)
        });
        cx.run_until_parked();
        assert_eq!(
            report_calls.lock().unwrap()[1],
            ("pulse-1".to_owned(), Some(copy.clone()))
        );
        assert_eq!(opened.borrow().len(), 1);
        assert_eq!(notice(cx), Some(ReportNotice::Saved(copy)));

        *report_error.lock().unwrap() = Some("The report could not be written.".into());
        model.update(cx, |model, cx| model.open_pulsation_report(cx));
        cx.run_until_parked();
        assert_eq!(opened.borrow().len(), 1);
        assert_eq!(
            notice(cx),
            Some(ReportNotice::Failed(
                "The report could not be written.".into()
            ))
        );
    }

    #[gpui::test]
    fn cleanup_preview_is_never_requested_for_a_live_pulsation(cx: &mut TestAppContext) {
        let pulsations_backend = Arc::new(RecordingPulsationsBackend::new());
        *pulsations_backend
            .cleanup_plan
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(scripted_cleanup_plan());
        let preview_calls = Arc::clone(&pulsations_backend.cleanup_preview_calls);
        let cleanup_calls = Arc::clone(&pulsations_backend.cleanup_calls);
        let model = cx.new(|cx| {
            AppModel::new_for_test_with_domain_backends(
                pulsations_backend,
                Arc::new(RecordingVaultBackend::new()),
                cx,
            )
        });
        model.update(cx, |model, cx| {
            configure_domain_workspace(model, cx);
            model.select_view(AppView::Pulsations, cx);
            model.refresh_pulsations(cx);
        });
        cx.run_until_parked();
        model.update(cx, |model, cx| {
            model.request_cleanup_failed_files(cx);
            model.request_cleanup_failed_files(cx);
        });
        cx.run_until_parked();
        assert!(
            preview_calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty(),
            "a running pulsation has no leftover files to preview"
        );
        assert!(
            cleanup_calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        );
        cx.read(|cx| {
            let state = model.read(cx).pulsations.read(cx);
            assert!(state.cleanup.plan.is_none());
            assert!(!state.cleanup.armed);
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
