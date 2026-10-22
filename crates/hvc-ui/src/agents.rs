use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use hvc_core::AppCore;
use hvc_core::llm::{
    AgentExecutionTarget, AgentRole, FreeFormProviderRouteBinding, LlmEventEnvelope,
    LlmExecutionListItem, LlmStreamEvent, UpsertFreeFormProviderRouteBindingRequest,
};
use hvc_core::persistence_service::{
    CancelAgentApiExecutionRequest, CliFallbackCheckpoint, ListAgentApiExecutionsRequest,
    PersistenceCommandError, ResolveCliFallbackRequest, ResolveCliFallbackResponse,
    StartFreeFormApiRunRequest, StartFreeFormApiRunResponse, ValidateCliFallbackRequest,
    ValidateCliFallbackResponse,
};
use hvc_core::terminals::{
    CliBackend, CliModelCatalogSnapshot, CreateFreeFormSlotRequest, ResumeManualRunRequest,
    StartFreeFormRunRequest, StartManualRunRequest, TerminalEvent, TerminalEventEnvelope,
    TerminalRun, TerminalsSnapshot, UpdateCliAgentConfigRequest, UpdateFreeFormSlotRequest,
};

use crate::errors::UiCommandError;

pub(crate) type AgentsFuture<T> =
    Pin<Box<dyn Future<Output = Result<T, UiCommandError>> + Send + 'static>>;

#[derive(Debug)]
pub(crate) struct AgentsDashboardSnapshot {
    pub(crate) terminals: TerminalsSnapshot,
    pub(crate) fallbacks: Vec<CliFallbackCheckpoint>,
    pub(crate) free_form_bindings: Vec<FreeFormProviderRouteBinding>,
    pub(crate) api_executions: HashMap<String, Vec<LlmExecutionListItem>>,
    /// Plan-window change per recent CLI run, by run id (ADR-028 attribution).
    pub(crate) quota_deltas: HashMap<String, Vec<hvc_core::quota::RunQuotaDelta>>,
}

pub(crate) trait AgentsBackend: Send + Sync {
    fn snapshot(&self, project_id: String) -> AgentsFuture<AgentsDashboardSnapshot>;
    fn refresh_snapshot(&self, project_id: String) -> AgentsFuture<AgentsDashboardSnapshot>;
    fn model_catalog(
        &self,
        project_id: String,
        backend: CliBackend,
        refresh: bool,
    ) -> AgentsFuture<CliModelCatalogSnapshot>;
    fn update_config(
        &self,
        request: UpdateCliAgentConfigRequest,
    ) -> AgentsFuture<hvc_core::terminals::CliAgentConfig>;
    fn start_manual(&self, request: StartManualRunRequest) -> AgentsFuture<TerminalRun>;
    fn start_free_form(&self, request: StartFreeFormRunRequest) -> AgentsFuture<TerminalRun>;
    fn start_free_form_api(
        &self,
        request: StartFreeFormApiRunRequest,
    ) -> AgentsFuture<StartFreeFormApiRunResponse>;
    fn cancel_api_execution(&self, execution_id: String) -> AgentsFuture<bool>;
    fn upsert_free_form_binding(
        &self,
        request: UpsertFreeFormProviderRouteBindingRequest,
    ) -> AgentsFuture<FreeFormProviderRouteBinding>;
    fn resume_manual(&self, request: ResumeManualRunRequest) -> AgentsFuture<TerminalRun>;
    fn create_free_form(
        &self,
        request: CreateFreeFormSlotRequest,
    ) -> AgentsFuture<hvc_core::terminals::TerminalSlot>;
    fn update_free_form(
        &self,
        request: UpdateFreeFormSlotRequest,
    ) -> AgentsFuture<hvc_core::terminals::TerminalSlot>;
    fn delete_free_form(
        &self,
        project_id: String,
        slot_id: String,
        expected_version: u64,
    ) -> AgentsFuture<()>;
    fn reorder_free_form(&self, project_id: String, slot_ids: Vec<String>) -> AgentsFuture<()>;
    fn cancel(&self, run_id: String) -> AgentsFuture<bool>;
    fn events(
        &self,
        run_id: String,
        after_sequence: u64,
        limit: usize,
    ) -> AgentsFuture<Vec<TerminalEvent>>;
    fn clear_history(&self, project_id: String, role: Option<AgentRole>) -> AgentsFuture<usize>;
    fn clear_slot_history(&self, project_id: String, slot_id: String) -> AgentsFuture<usize>;
    fn export_history(&self, project_id: String, run_id: String) -> AgentsFuture<String>;
    fn export_api_execution(
        &self,
        project_id: String,
        target: AgentExecutionTarget,
        execution_id: String,
    ) -> AgentsFuture<String>;
    fn resolve_fallback(
        &self,
        request: ResolveCliFallbackRequest,
    ) -> AgentsFuture<ResolveCliFallbackResponse>;
    /// Runs the CLI contract probe behind a `cli_version_unvalidated` fallback; resumes the
    /// pulsation on success, downgrades the fallback to `cli_validation_failed` otherwise.
    fn validate_fallback(
        &self,
        request: ValidateCliFallbackRequest,
    ) -> AgentsFuture<ValidateCliFallbackResponse>;
}

pub(crate) struct CoreAgentsBackend {
    core: Arc<AppCore>,
}

impl CoreAgentsBackend {
    pub(crate) fn new(core: Arc<AppCore>) -> Self {
        Self { core }
    }
}

fn terminal_error(error: hvc_core::terminals::TerminalError) -> UiCommandError {
    UiCommandError::from_chat_parts("agents_error", error.to_string())
}

fn persistence_error(
    error: hvc_core::persistence_service::PersistenceServiceError,
) -> UiCommandError {
    UiCommandError::from(PersistenceCommandError::from(error))
}

fn complete_dashboard_snapshot(
    core: &AppCore,
    terminals: TerminalsSnapshot,
) -> Result<AgentsDashboardSnapshot, UiCommandError> {
    let persistence = core.persistence();
    let fallbacks = persistence
        .list_cli_fallbacks(&terminals.project_id)
        .map_err(persistence_error)?;
    let free_form_bindings = persistence
        .list_free_form_provider_route_bindings(&terminals.project_id)
        .map_err(persistence_error)?
        .bindings;
    let mut api_executions = HashMap::new();
    let targets = terminals
        .agents
        .iter()
        .map(|agent| AgentExecutionTarget::canonical(agent.config.agent_role))
        .chain(
            terminals
                .free_form_slots
                .iter()
                .chain(&terminals.archived_free_form_slots)
                .map(|slot| AgentExecutionTarget::free_form(slot.slot.id.clone())),
        );
    for target in targets {
        let key = target.stable_key();
        let executions = persistence
            .list_agent_api_executions(ListAgentApiExecutionsRequest {
                project_id: terminals.project_id.clone(),
                target,
                limit: Some(20),
            })
            .map_err(persistence_error)?
            .executions;
        api_executions.insert(key, executions);
    }
    let quota_deltas = core.terminals().quota_meter().run_deltas_many(
        terminals
            .agents
            .iter()
            .flat_map(|agent| &agent.recent_runs)
            .map(|run| run.id.as_str()),
    );
    Ok(AgentsDashboardSnapshot {
        terminals,
        fallbacks,
        free_form_bindings,
        api_executions,
        quota_deltas,
    })
}

impl AgentsBackend for CoreAgentsBackend {
    fn snapshot(&self, project_id: String) -> AgentsFuture<AgentsDashboardSnapshot> {
        let terminals = self.core.terminals();
        let core = Arc::clone(&self.core);
        let runtime = self.core.runtime_handle().clone();
        Box::pin(async move {
            let terminals = runtime
                .spawn(async move { terminals.snapshot(&project_id).await })
                .await
                .map_err(|error| {
                    UiCommandError::worker(format!("task de Agents interrompida: {error}"))
                })?
                .map_err(terminal_error)?;
            complete_dashboard_snapshot(&core, terminals)
        })
    }

    fn refresh_snapshot(&self, project_id: String) -> AgentsFuture<AgentsDashboardSnapshot> {
        let terminals = self.core.terminals();
        let core = Arc::clone(&self.core);
        let runtime = self.core.runtime_handle().clone();
        Box::pin(async move {
            let terminals = runtime
                .spawn(async move {
                    terminals.refresh_discovery().await;
                    terminals.snapshot(&project_id).await
                })
                .await
                .map_err(|error| {
                    UiCommandError::worker(format!("task de Agents interrompida: {error}"))
                })?
                .map_err(terminal_error)?;
            complete_dashboard_snapshot(&core, terminals)
        })
    }

    fn model_catalog(
        &self,
        project_id: String,
        backend: CliBackend,
        refresh: bool,
    ) -> AgentsFuture<CliModelCatalogSnapshot> {
        let terminals = self.core.terminals();
        let runtime = self.core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn(async move { terminals.model_catalog(&project_id, backend, refresh).await })
                .await
                .map_err(|error| {
                    UiCommandError::worker(format!("model catalog task interrupted: {error}"))
                })?
                .map_err(terminal_error)
        })
    }

    fn update_config(
        &self,
        request: UpdateCliAgentConfigRequest,
    ) -> AgentsFuture<hvc_core::terminals::CliAgentConfig> {
        let terminals = self.core.terminals();
        let runtime = self.core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || terminals.update_config(request))
                .await
                .map_err(|error| {
                    UiCommandError::worker(format!("worker de Agents interrompido: {error}"))
                })?
                .map_err(terminal_error)
        })
    }

    fn start_manual(&self, request: StartManualRunRequest) -> AgentsFuture<TerminalRun> {
        let terminals = self.core.terminals();
        let runtime = self.core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn(async move { terminals.start_manual(request).await })
                .await
                .map_err(|error| UiCommandError::worker(error.to_string()))?
                .map_err(terminal_error)
        })
    }

    fn start_free_form(&self, request: StartFreeFormRunRequest) -> AgentsFuture<TerminalRun> {
        let terminals = self.core.terminals();
        let runtime = self.core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn(async move { terminals.start_free_form(request).await })
                .await
                .map_err(|error| {
                    UiCommandError::worker(format!("free-form agent worker interrupted: {error}"))
                })?
                .map_err(terminal_error)
        })
    }

    fn start_free_form_api(
        &self,
        request: StartFreeFormApiRunRequest,
    ) -> AgentsFuture<StartFreeFormApiRunResponse> {
        let core = Arc::clone(&self.core);
        Box::pin(async move {
            core.persistence()
                .start_free_form_api(request)
                .map_err(persistence_error)
        })
    }

    fn cancel_api_execution(&self, execution_id: String) -> AgentsFuture<bool> {
        let core = Arc::clone(&self.core);
        Box::pin(async move {
            core.persistence()
                .cancel_agent_api_execution(CancelAgentApiExecutionRequest { execution_id })
                .map(|response| response.cancelled)
                .map_err(persistence_error)
        })
    }

    fn upsert_free_form_binding(
        &self,
        request: UpsertFreeFormProviderRouteBindingRequest,
    ) -> AgentsFuture<FreeFormProviderRouteBinding> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    core.persistence()
                        .upsert_free_form_provider_route_binding(request)
                })
                .await
                .map_err(|error| UiCommandError::worker(error.to_string()))?
                .map(|response| response.binding)
                .map_err(persistence_error)
        })
    }

    fn resume_manual(&self, request: ResumeManualRunRequest) -> AgentsFuture<TerminalRun> {
        let terminals = self.core.terminals();
        let runtime = self.core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn(async move { terminals.resume_manual(request).await })
                .await
                .map_err(|error| {
                    UiCommandError::worker(format!("worker de retomada interrompido: {error}"))
                })?
                .map_err(terminal_error)
        })
    }

    fn create_free_form(
        &self,
        request: CreateFreeFormSlotRequest,
    ) -> AgentsFuture<hvc_core::terminals::TerminalSlot> {
        let terminals = self.core.terminals();
        let runtime = self.core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || terminals.repository().create_free_form_slot(request))
                .await
                .map_err(|error| {
                    UiCommandError::worker(format!("free-form agent worker interrupted: {error}"))
                })?
                .map_err(terminal_error)
        })
    }

    fn update_free_form(
        &self,
        request: UpdateFreeFormSlotRequest,
    ) -> AgentsFuture<hvc_core::terminals::TerminalSlot> {
        let terminals = self.core.terminals();
        let runtime = self.core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || terminals.repository().update_free_form_slot(request))
                .await
                .map_err(|error| {
                    UiCommandError::worker(format!("free-form agent worker interrupted: {error}"))
                })?
                .map_err(terminal_error)
        })
    }

    fn delete_free_form(
        &self,
        project_id: String,
        slot_id: String,
        expected_version: u64,
    ) -> AgentsFuture<()> {
        let terminals = self.core.terminals();
        let runtime = self.core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    terminals.repository().delete_free_form_slot(
                        &project_id,
                        &slot_id,
                        expected_version,
                    )
                })
                .await
                .map_err(|error| {
                    UiCommandError::worker(format!("free-form agent worker interrupted: {error}"))
                })?
                .map_err(terminal_error)
        })
    }

    fn reorder_free_form(&self, project_id: String, slot_ids: Vec<String>) -> AgentsFuture<()> {
        let terminals = self.core.terminals();
        let runtime = self.core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    terminals
                        .repository()
                        .reorder_free_form_slots(&project_id, &slot_ids)
                })
                .await
                .map_err(|error| {
                    UiCommandError::worker(format!("ordering worker aborted: {error}"))
                })?
                .map_err(terminal_error)
        })
    }

    fn cancel(&self, run_id: String) -> AgentsFuture<bool> {
        let terminals = self.core.terminals();
        Box::pin(async move { Ok(terminals.cancel_run(&run_id)) })
    }

    fn events(
        &self,
        run_id: String,
        after_sequence: u64,
        limit: usize,
    ) -> AgentsFuture<Vec<TerminalEvent>> {
        let terminals = self.core.terminals();
        let runtime = self.core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || terminals.load_events(&run_id, after_sequence, limit))
                .await
                .map_err(|error| {
                    UiCommandError::worker(format!("history worker aborted: {error}"))
                })?
                .map_err(terminal_error)
        })
    }

    fn clear_history(&self, project_id: String, role: Option<AgentRole>) -> AgentsFuture<usize> {
        let terminals = self.core.terminals();
        let runtime = self.core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || terminals.clear_history(&project_id, role))
                .await
                .map_err(|error| UiCommandError::worker(format!("clear worker aborted: {error}")))?
                .map_err(terminal_error)
        })
    }

    fn clear_slot_history(&self, project_id: String, slot_id: String) -> AgentsFuture<usize> {
        let terminals = self.core.terminals();
        let runtime = self.core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || terminals.clear_slot_history(&project_id, &slot_id))
                .await
                .map_err(|error| UiCommandError::worker(format!("clear worker aborted: {error}")))?
                .map_err(terminal_error)
        })
    }

    fn export_history(&self, project_id: String, run_id: String) -> AgentsFuture<String> {
        let terminals = self.core.terminals();
        let runtime = self.core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    terminals
                        .repository()
                        .export_run_history(&project_id, &run_id)
                })
                .await
                .map_err(|error| UiCommandError::worker(format!("export worker aborted: {error}")))?
                .map(|path| path.to_string_lossy().into_owned())
                .map_err(terminal_error)
        })
    }

    fn export_api_execution(
        &self,
        project_id: String,
        target: AgentExecutionTarget,
        execution_id: String,
    ) -> AgentsFuture<String> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    core.persistence().export_agent_api_execution(
                        hvc_core::persistence_service::GetAgentApiExecutionSnapshotRequest {
                            project_id,
                            target,
                            execution_id,
                        },
                    )
                })
                .await
                .map_err(|error| UiCommandError::worker(format!("export worker aborted: {error}")))?
                .map(|path| path.to_string_lossy().into_owned())
                .map_err(persistence_error)
        })
    }

    fn resolve_fallback(
        &self,
        request: ResolveCliFallbackRequest,
    ) -> AgentsFuture<ResolveCliFallbackResponse> {
        let core = Arc::clone(&self.core);
        let runtime = self.core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || core.persistence().resolve_cli_fallback(request))
                .await
                .map_err(|error| {
                    UiCommandError::worker(format!("worker de fallback interrompido: {error}"))
                })?
                .map_err(persistence_error)
        })
    }

    fn validate_fallback(
        &self,
        request: ValidateCliFallbackRequest,
    ) -> AgentsFuture<ValidateCliFallbackResponse> {
        let core = Arc::clone(&self.core);
        let runtime = self.core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn(async move { core.persistence().validate_cli_fallback(request).await })
                .await
                .map_err(|error| {
                    UiCommandError::worker(format!("CLI validation worker interrupted: {error}"))
                })?
                .map_err(persistence_error)
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CliModelPickerTarget {
    Canonical(AgentRole),
    FreeForm(String),
    Companion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CliModelPickerState {
    pub(crate) target: CliModelPickerTarget,
    pub(crate) backend: CliBackend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerminalEventFilter {
    All,
    Activity,
    Tool,
    Stdout,
    Stderr,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApiActivityFilter {
    All,
    Output,
    Lifecycle,
    Errors,
}

impl ApiActivityFilter {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Output => "output",
            Self::Lifecycle => "lifecycle",
            Self::Errors => "errors",
        }
    }

    pub(crate) fn next(self) -> Self {
        match self {
            Self::All => Self::Output,
            Self::Output => Self::Lifecycle,
            Self::Lifecycle => Self::Errors,
            Self::Errors => Self::All,
        }
    }

    pub(crate) fn accepts(self, line: &str) -> bool {
        match self {
            Self::All => true,
            Self::Output => line.starts_with("output · "),
            Self::Lifecycle => {
                line.starts_with("route · ")
                    || line.starts_with("attempt ")
                    || line.starts_with("completed · ")
                    || line.starts_with("resilience ")
            }
            Self::Errors => line.contains("failed") || line.contains("error"),
        }
    }
}

impl TerminalEventFilter {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Activity => "activity",
            Self::Tool => "tool",
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }

    pub(crate) fn next(self) -> Self {
        match self {
            Self::All => Self::Activity,
            Self::Activity => Self::Tool,
            Self::Tool => Self::Stdout,
            Self::Stdout => Self::Stderr,
            Self::Stderr => Self::Warning,
            Self::Warning => Self::Error,
            Self::Error => Self::All,
        }
    }

    pub(crate) fn accepts(self, kind: hvc_core::terminals::TerminalEventKind) -> bool {
        use hvc_core::terminals::TerminalEventKind;
        match self {
            Self::All => true,
            Self::Activity => matches!(
                kind,
                TerminalEventKind::System
                    | TerminalEventKind::Message
                    | TerminalEventKind::Completed
            ),
            Self::Tool => matches!(
                kind,
                TerminalEventKind::ToolUse | TerminalEventKind::ToolResult
            ),
            Self::Stdout => kind == TerminalEventKind::Stdout,
            Self::Stderr => kind == TerminalEventKind::Stderr,
            Self::Warning => kind == TerminalEventKind::Warning,
            Self::Error => kind == TerminalEventKind::Error,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum AgentIdentifier {
    Canonical(AgentRole),
    FreeForm(String),
}

impl AgentIdentifier {
    pub(crate) fn target(&self) -> AgentExecutionTarget {
        match self {
            Self::Canonical(role) => AgentExecutionTarget::canonical(*role),
            Self::FreeForm(slot_id) => AgentExecutionTarget::free_form(slot_id.clone()),
        }
    }

    pub(crate) fn stable_key(&self) -> String {
        self.target().stable_key()
    }

    fn from_target(target: &AgentExecutionTarget) -> Self {
        match target {
            AgentExecutionTarget::Canonical { agent_role } => Self::Canonical(*agent_role),
            AgentExecutionTarget::Ephemeral { agent_role, .. } => Self::Canonical(*agent_role),
            AgentExecutionTarget::FreeForm { slot_id } => Self::FreeForm(slot_id.clone()),
        }
    }
}

#[derive(Debug)]
pub(crate) struct AgentsModel {
    pub(crate) selected_agent: Option<AgentIdentifier>,
    pub(crate) snapshot: Option<TerminalsSnapshot>,
    pub(crate) fallbacks: Vec<CliFallbackCheckpoint>,
    pub(crate) free_form_bindings: Vec<FreeFormProviderRouteBinding>,
    pub(crate) events_by_run: HashMap<String, Vec<TerminalEvent>>,
    pub(crate) selected_run_by_role: HashMap<AgentRole, String>,
    pub(crate) selected_run_by_slot: HashMap<String, String>,
    pub(crate) event_filters: HashMap<String, TerminalEventFilter>,
    pub(crate) follow_tail: HashMap<String, bool>,
    pub(crate) api_activity: HashMap<AgentIdentifier, Vec<String>>,
    pub(crate) api_filters: HashMap<String, ApiActivityFilter>,
    pub(crate) api_executions: HashMap<String, Vec<LlmExecutionListItem>>,
    pub(crate) quota_deltas: HashMap<String, Vec<hvc_core::quota::RunQuotaDelta>>,
    pub(crate) active_api_execution_by_slot: HashMap<String, String>,
    pub(crate) model_catalogs: HashMap<CliBackend, CliModelCatalogSnapshot>,
    pub(crate) active_model_picker: Option<CliModelPickerState>,
    pub(crate) catalog_loading_backend: Option<CliBackend>,
    pub(crate) catalog_error: Option<String>,
    pub(crate) viewer_enabled: bool,
    pub(crate) selected_api_target: Option<AgentIdentifier>,
    pub(crate) terminal_expanded: bool,
    api_sequences: HashMap<String, u64>,
    /// A chrome-only snapshot is in flight. It must participate in the same admission gate as
    /// visible panel work so terminal bursts cannot start overlapping SQLite snapshots.
    quiet_snapshot_in_flight: bool,
    pub(crate) loading: bool,
    pub(crate) busy: bool,
    pub(crate) notice: Option<String>,
    pub(crate) error: Option<UiCommandError>,
    pub(crate) pending_fallback_cancel: Option<String>,
    pub(crate) pending_pipeline_cancel: Option<String>,
    pub(crate) pending_history_clear: Option<AgentRole>,
    pub(crate) pending_free_form_history_clear: Option<String>,
    pub(crate) pending_free_form_delete: Option<String>,
    pub(crate) generation: u64,
}

impl Default for AgentsModel {
    fn default() -> Self {
        Self {
            selected_agent: None,
            snapshot: None,
            fallbacks: Vec::new(),
            free_form_bindings: Vec::new(),
            events_by_run: HashMap::new(),
            selected_run_by_role: HashMap::new(),
            selected_run_by_slot: HashMap::new(),
            event_filters: HashMap::new(),
            follow_tail: HashMap::new(),
            api_activity: HashMap::new(),
            api_filters: HashMap::new(),
            api_executions: HashMap::new(),
            quota_deltas: HashMap::new(),
            active_api_execution_by_slot: HashMap::new(),
            model_catalogs: HashMap::new(),
            active_model_picker: None,
            catalog_loading_backend: None,
            catalog_error: None,
            viewer_enabled: true,
            selected_api_target: None,
            terminal_expanded: false,
            api_sequences: HashMap::new(),
            quiet_snapshot_in_flight: false,
            loading: false,
            busy: false,
            notice: None,
            error: None,
            pending_fallback_cancel: None,
            pending_pipeline_cancel: None,
            pending_history_clear: None,
            pending_free_form_history_clear: None,
            pending_free_form_delete: None,
            generation: 0,
        }
    }
}

impl AgentsModel {
    /// Number of canonical/free-form agents with at least one failed run in their recent history.
    /// Feeds the Agent tab's attention badge (see the UI flow plan Part 4.4). Deliberately checks
    /// the whole `recent_runs` buffer rather than assuming it's ordered newest-first, so an agent
    /// that has since recovered may still count until its history ages out.
    pub(crate) fn failed_agent_count(&self) -> usize {
        let Some(snapshot) = &self.snapshot else {
            return 0;
        };
        let has_failure = |runs: &[hvc_core::terminals::TerminalRun]| {
            runs.iter()
                .any(|run| run.status == hvc_core::terminals::TerminalRunStatus::Failed)
        };
        snapshot
            .agents
            .iter()
            .filter(|agent| has_failure(&agent.recent_runs))
            .count()
            + snapshot
                .free_form_slots
                .iter()
                .filter(|slot| has_failure(&slot.recent_runs))
                .count()
    }

    pub(crate) fn begin(&mut self, loading: bool) -> u64 {
        self.generation = self.generation.saturating_add(1);
        self.quiet_snapshot_in_flight = false;
        self.loading = loading;
        self.busy = !loading;
        self.error = None;
        self.notice = None;
        self.generation
    }

    /// Starts a background snapshot read that leaves `loading`/`busy`/`error` untouched: the
    /// shell chrome (header, context rail, footer) refreshes while another view owns the panel,
    /// so the Agents panel must not flicker into its loading state.
    pub(crate) fn begin_quiet(&mut self) -> u64 {
        self.generation = self.generation.saturating_add(1);
        self.quiet_snapshot_in_flight = true;
        self.generation
    }

    /// `true` while a loud read or an operation is in flight; a quiet refresh yields to it so a
    /// later generation never swallows the operation's result or error.
    pub(crate) fn in_flight(&self) -> bool {
        self.loading || self.busy || self.quiet_snapshot_in_flight
    }

    pub(crate) fn apply_snapshot(
        &mut self,
        generation: u64,
        snapshot: AgentsDashboardSnapshot,
    ) -> bool {
        if generation != self.generation {
            return false;
        }
        self.quiet_snapshot_in_flight = false;
        self.loading = false;
        self.busy = false;
        if self
            .snapshot
            .as_ref()
            .is_some_and(|current| current.project_id != snapshot.terminals.project_id)
        {
            self.model_catalogs.clear();
            self.active_model_picker = None;
            self.selected_agent = None;
        }
        self.events_by_run.clear();
        for agent in &snapshot.terminals.agents {
            if let Some(run) = agent.recent_runs.first() {
                let selected = self
                    .selected_run_by_role
                    .get(&agent.config.agent_role)
                    .filter(|run_id| agent.recent_runs.iter().any(|run| &run.id == *run_id))
                    .cloned()
                    .unwrap_or_else(|| run.id.clone());
                self.selected_run_by_role
                    .insert(agent.config.agent_role, selected);
                self.events_by_run
                    .insert(run.id.clone(), agent.recent_events.clone());
            } else {
                self.selected_run_by_role.remove(&agent.config.agent_role);
            }
        }
        for free_form in &snapshot.terminals.free_form_slots {
            if let Some(run) = free_form.recent_runs.first() {
                let selected = self
                    .selected_run_by_slot
                    .get(&free_form.slot.id)
                    .filter(|run_id| free_form.recent_runs.iter().any(|run| &run.id == *run_id))
                    .cloned()
                    .unwrap_or_else(|| run.id.clone());
                self.selected_run_by_slot
                    .insert(free_form.slot.id.clone(), selected);
                self.events_by_run
                    .insert(run.id.clone(), free_form.recent_events.clone());
            } else {
                self.selected_run_by_slot.remove(&free_form.slot.id);
            }
        }
        for free_form in &snapshot.terminals.archived_free_form_slots {
            if let Some(run) = free_form.recent_runs.first() {
                self.events_by_run
                    .insert(run.id.clone(), free_form.recent_events.clone());
            }
        }
        self.fallbacks = snapshot.fallbacks;
        self.free_form_bindings = snapshot.free_form_bindings;
        self.api_executions = snapshot.api_executions;
        self.quota_deltas = snapshot.quota_deltas;
        if self
            .pending_fallback_cancel
            .as_ref()
            .is_some_and(|checkpoint_id| {
                !self
                    .fallbacks
                    .iter()
                    .any(|fallback| &fallback.checkpoint_id == checkpoint_id)
            })
        {
            self.pending_fallback_cancel = None;
        }
        if self
            .pending_pipeline_cancel
            .as_ref()
            .is_some_and(|pulsation_id| {
                !snapshot.terminals.agents.iter().any(|agent| {
                    agent.recent_runs.iter().any(|run| {
                        run.pulsation_id.as_deref() == Some(pulsation_id.as_str())
                            && matches!(
                                run.status,
                                hvc_core::terminals::TerminalRunStatus::Queued
                                    | hvc_core::terminals::TerminalRunStatus::Starting
                                    | hvc_core::terminals::TerminalRunStatus::Running
                            )
                    })
                })
            })
        {
            self.pending_pipeline_cancel = None;
        }
        self.reconcile_selection(&snapshot.terminals);
        self.snapshot = Some(snapshot.terminals);
        true
    }

    fn reconcile_selection(&mut self, snapshot: &TerminalsSnapshot) {
        let valid = match self.selected_agent.as_ref() {
            Some(AgentIdentifier::Canonical(role)) => snapshot
                .agents
                .iter()
                .any(|agent| agent.config.agent_role == *role),
            Some(AgentIdentifier::FreeForm(id)) => snapshot
                .free_form_slots
                .iter()
                .chain(&snapshot.archived_free_form_slots)
                .any(|slot| slot.slot.id == *id),
            None => false,
        };
        if !valid {
            self.active_model_picker = None;
            self.selected_agent = snapshot
                .agents
                .first()
                .map(|agent| AgentIdentifier::Canonical(agent.config.agent_role))
                .or_else(|| {
                    snapshot
                        .free_form_slots
                        .iter()
                        .chain(&snapshot.archived_free_form_slots)
                        .next()
                        .map(|slot| AgentIdentifier::FreeForm(slot.slot.id.clone()))
                });
        }
    }

    pub(crate) fn apply_model_catalog(&mut self, catalog: CliModelCatalogSnapshot) {
        self.catalog_loading_backend = None;
        self.catalog_error = None;
        self.model_catalogs.insert(catalog.backend, catalog);
    }

    pub(crate) fn apply_run_events(
        &mut self,
        generation: u64,
        role: AgentRole,
        run_id: String,
        events: Vec<TerminalEvent>,
    ) -> bool {
        if generation != self.generation {
            return false;
        }
        self.busy = false;
        self.events_by_run.insert(run_id.clone(), events);
        self.selected_run_by_role.insert(role, run_id);
        true
    }

    pub(crate) fn apply_slot_run_events(
        &mut self,
        generation: u64,
        slot_id: String,
        run_id: String,
        events: Vec<TerminalEvent>,
    ) -> bool {
        if generation != self.generation {
            return false;
        }
        self.busy = false;
        self.events_by_run.insert(run_id.clone(), events);
        self.selected_run_by_slot.insert(slot_id, run_id);
        true
    }

    pub(crate) fn fail(&mut self, generation: u64, error: UiCommandError) -> bool {
        if generation != self.generation {
            return false;
        }
        self.quiet_snapshot_in_flight = false;
        self.loading = false;
        self.busy = false;
        self.error = Some(error);
        true
    }

    pub(crate) fn record_event(&mut self, envelope: TerminalEventEnvelope) -> bool {
        let Some(snapshot) = self.snapshot.as_mut() else {
            return false;
        };
        if snapshot.project_id != envelope.project_id {
            return false;
        }
        let role = envelope.agent_role;
        let slot_id = envelope.run.slot_id.clone();
        let run_id = envelope.run_id.clone();
        let run_is_active = envelope.run.status.is_active();
        if let Some(role) = role {
            let Some(agent) = snapshot
                .agents
                .iter_mut()
                .find(|agent| agent.config.agent_role == role)
            else {
                return false;
            };
            upsert_recent_run(&mut agent.recent_runs, envelope.run);
            if run_is_active || !self.selected_run_by_role.contains_key(&role) {
                self.selected_run_by_role.insert(role, run_id.clone());
            }
        } else {
            let Some(free_form) = snapshot
                .free_form_slots
                .iter_mut()
                .find(|free_form| free_form.slot.id == slot_id)
            else {
                return false;
            };
            upsert_recent_run(&mut free_form.recent_runs, envelope.run);
            if run_is_active || !self.selected_run_by_slot.contains_key(&slot_id) {
                self.selected_run_by_slot.insert(slot_id, run_id.clone());
            }
        }
        let events = self.events_by_run.entry(run_id).or_default();
        let gap = events.last().map_or(envelope.sequence > 1, |event| {
            envelope.sequence > event.sequence.saturating_add(1)
        });
        if events
            .last()
            .is_none_or(|event| event.sequence < envelope.sequence)
        {
            events.push(envelope.event);
            if events.len() > 500 {
                events.drain(..events.len() - 500);
            }
        }
        gap
    }

    pub(crate) fn record_llm(&mut self, envelope: &LlmEventEnvelope) -> bool {
        self.record_llm_batch(std::slice::from_ref(envelope))
    }

    /// Projects a persisted LLM stream in one paint batch. Token text is retained only until the
    /// end of this call, then redacted as a complete visible segment. This preserves redaction for
    /// secrets split across chunks without repeatedly rebuilding accumulated output per token.
    pub(crate) fn record_llm_batch(&mut self, envelopes: &[LlmEventEnvelope]) -> bool {
        let mut gap = false;
        let mut pending_output = HashMap::<AgentIdentifier, String>::new();

        for envelope in envelopes {
            if !self.viewer_enabled {
                continue;
            }
            let Some(context) = envelope.context.as_ref() else {
                continue;
            };
            if self
                .snapshot
                .as_ref()
                .is_none_or(|snapshot| snapshot.project_id != context.project_id)
            {
                continue;
            }
            let identifier = AgentIdentifier::from_target(&context.target);
            let last = self
                .api_sequences
                .entry(envelope.execution_id.clone())
                .or_default();
            if envelope.sequence <= *last {
                continue;
            }
            gap |= envelope.sequence > last.saturating_add(1);
            *last = envelope.sequence;
            if let AgentExecutionTarget::FreeForm { slot_id } = &context.target {
                if matches!(&envelope.event, LlmStreamEvent::RouteStarted { .. }) {
                    self.active_api_execution_by_slot
                        .insert(slot_id.clone(), envelope.execution_id.clone());
                }
                if matches!(
                    &envelope.event,
                    LlmStreamEvent::RouteCompleted { .. } | LlmStreamEvent::RouteFailed { .. }
                ) {
                    self.active_api_execution_by_slot.remove(slot_id);
                }
            }

            if let LlmStreamEvent::TokenDelta { text } = &envelope.event {
                pending_output.entry(identifier).or_default().push_str(text);
                continue;
            }
            self.flush_api_output(&identifier, pending_output.remove(&identifier));
            let line = match &envelope.event {
                LlmStreamEvent::RouteStarted { route_id } => format!(
                    "route · {route_id} · execution {} · pulsation {} · job {}",
                    envelope.execution_id.chars().take(8).collect::<String>(),
                    context.pulsation_id.as_deref().unwrap_or("—"),
                    context.job_id.as_deref().unwrap_or("—")
                ),
                LlmStreamEvent::AttemptStarted {
                    provider_id,
                    model,
                    ordinal,
                    ..
                } => format!("attempt {ordinal} · {provider_id} · {model}"),
                LlmStreamEvent::AttemptFailed { error_code, .. } => {
                    format!("attempt failed · {error_code}")
                }
                LlmStreamEvent::RouteCompleted {
                    provider_id, model, ..
                } => format!("completed · {provider_id} · {model}"),
                LlmStreamEvent::RouteFailed { error_code, .. } => {
                    format!("route failed · {error_code}")
                }
                LlmStreamEvent::ResilienceChanged {
                    scope, to_state, ..
                } => format!("resilience {scope} · {to_state}"),
                LlmStreamEvent::TokenDelta { .. } => unreachable!("token deltas are handled above"),
            };
            self.push_api_activity(
                identifier,
                hvc_core::secret_redactor::redact_secrets(&line)
                    .map(|result| result.text)
                    .unwrap_or_else(|_| "[content unavailable after redaction]".into()),
            );
            if matches!(
                &envelope.event,
                LlmStreamEvent::RouteCompleted { .. } | LlmStreamEvent::RouteFailed { .. }
            ) {
                self.api_sequences.remove(&envelope.execution_id);
            }
        }
        for (identifier, output) in pending_output {
            self.flush_api_output(&identifier, Some(output));
        }
        gap
    }

    fn flush_api_output(&mut self, identifier: &AgentIdentifier, output: Option<String>) {
        let Some(output) = output.filter(|output| !output.is_empty()) else {
            return;
        };
        let activity = self.api_activity.entry(identifier.clone()).or_default();
        let target = if activity
            .last()
            .is_some_and(|line| line.starts_with("output · "))
        {
            activity.last_mut().expect("checked above")
        } else {
            activity.push("output · ".into());
            activity.last_mut().expect("just pushed")
        };
        target.push_str(&output);
        if target.chars().count() > 64 * 1024 {
            *target = target.chars().take(64 * 1024).collect();
            target.push_str("\n[visible output truncated]");
        }
        *target = hvc_core::secret_redactor::redact_secrets(target)
            .map(|result| result.text)
            .unwrap_or_else(|_| "[content unavailable after redaction]".into());
        if activity.len() > 200 {
            activity.drain(..activity.len() - 200);
        }
    }

    fn push_api_activity(&mut self, identifier: AgentIdentifier, line: String) {
        let activity = self.api_activity.entry(identifier).or_default();
        activity.push(line);
        if activity.len() > 200 {
            activity.drain(..activity.len() - 200);
        }
    }

    pub(crate) fn apply_started_api_run(
        &mut self,
        generation: u64,
        slot_id: String,
        execution_id: String,
    ) -> bool {
        if generation != self.generation {
            return false;
        }
        self.busy = false;
        self.active_api_execution_by_slot
            .insert(slot_id, execution_id);
        self.notice = Some("FreeForm API execution started.".into());
        true
    }

    pub(crate) fn apply_config(
        &mut self,
        generation: u64,
        updated: hvc_core::terminals::CliAgentConfig,
    ) -> bool {
        if generation != self.generation {
            return false;
        }
        self.busy = false;
        let Some(snapshot) = self.snapshot.as_mut() else {
            return false;
        };
        let Some(agent) = snapshot
            .agents
            .iter_mut()
            .find(|agent| agent.config.agent_role == updated.agent_role)
        else {
            return false;
        };
        agent.config = updated;
        self.notice = Some("Agent configuration updated.".into());
        true
    }

    pub(crate) fn apply_started_run(&mut self, generation: u64, run: TerminalRun) -> bool {
        if generation != self.generation {
            return false;
        }
        self.busy = false;
        let Some(snapshot) = self.snapshot.as_mut() else {
            return false;
        };
        if let Some(role) = run.agent_role {
            let Some(agent) = snapshot
                .agents
                .iter_mut()
                .find(|agent| agent.config.agent_role == role)
            else {
                return false;
            };
            agent.recent_runs.insert(0, run.clone());
            self.selected_run_by_role.insert(role, run.id.clone());
        } else {
            let Some(free_form) = snapshot
                .free_form_slots
                .iter_mut()
                .find(|free_form| free_form.slot.id == run.slot_id)
            else {
                return false;
            };
            free_form.recent_runs.insert(0, run.clone());
            self.selected_run_by_slot
                .insert(run.slot_id.clone(), run.id.clone());
        }
        self.events_by_run.insert(run.id, Vec::new());
        self.notice = Some("Manual execution started.".into());
        true
    }
}

fn upsert_recent_run(runs: &mut Vec<TerminalRun>, run: TerminalRun) {
    if let Some(existing) = runs.iter_mut().find(|existing| existing.id == run.id) {
        *existing = run;
    } else {
        runs.insert(0, run);
        runs.truncate(20);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hvc_core::llm::AgentExecutionContext;
    use hvc_core::terminals::{
        AgentExecutionMode, AgentTerminalSnapshot, CliAgentConfig, CliBackend,
        CliModelSelectionSource, ExecutionOrigin, TerminalEventKind, TerminalRunStatus,
        ThinkingLevel,
    };

    #[test]
    fn ephemeral_targets_remain_attached_to_the_visible_canonical_agent() {
        assert_eq!(
            AgentIdentifier::from_target(&AgentExecutionTarget::ephemeral(
                "ephemeral:execution-1:unit-1",
                AgentRole::BackendAgent,
            )),
            AgentIdentifier::Canonical(AgentRole::BackendAgent)
        );
    }

    fn run(status: TerminalRunStatus) -> TerminalRun {
        TerminalRun {
            id: "run-1".into(),
            project_id: "project-1".into(),
            slot: hvc_core::terminals::SessionSlot::Canonical {
                agent_role: AgentRole::BackendAgent,
            },
            slot_id: "canonical:project-1:backend_agent".into(),
            agent_role: Some(AgentRole::BackendAgent),
            origin: ExecutionOrigin::Pipeline,
            execution_mode: AgentExecutionMode::CliProcess,
            backend: Some(CliBackend::Codex),
            model: None,
            requested_model_profile: None,
            model_source: CliModelSelectionSource::Default,
            observed_model: None,
            native_session_id: None,
            pulsation_id: Some("pulse-1".into()),
            job_id: Some("job-1".into()),
            agent_run_id: None,
            llm_execution_id: Some("llm-1".into()),
            status,
            exit_code: None,
            error_code: None,
            error_message: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            started_at: None,
            finished_at: None,
            updated_at: "2026-01-01T00:00:00Z".into(),
            deadline_at: None,
            termination_reason: None,
            output_bytes: 0,
            event_count: 0,
            workspace_snapshot_hash: None,
            launch_identity_sha256: None,
            detected_cli_version: None,
            adapter_contract_version: 1,
            parent_run_id: None,
            continuation_reason: None,
            session_fingerprint: None,
            observation_trial_id: None,
        }
    }

    fn event_envelope(status: TerminalRunStatus, sequence: u64) -> TerminalEventEnvelope {
        TerminalEventEnvelope {
            protocol_version: 2,
            project_id: "project-1".into(),
            agent_role: Some(AgentRole::BackendAgent),
            run_id: "run-1".into(),
            sequence,
            run: run(status),
            event: TerminalEvent {
                id: format!("event-{sequence}"),
                run_id: "run-1".into(),
                sequence,
                kind: TerminalEventKind::System,
                stream: None,
                content: "status".into(),
                redaction_count: 0,
                redaction_categories: Vec::new(),
                created_at: "2026-01-01T00:00:00Z".into(),
            },
        }
    }

    fn model() -> AgentsModel {
        let mut model = AgentsModel::default();
        model.snapshot = Some(TerminalsSnapshot {
            project_id: "project-1".into(),
            agents: vec![AgentTerminalSnapshot {
                config: CliAgentConfig {
                    project_id: "project-1".into(),
                    agent_role: AgentRole::BackendAgent,
                    execution_mode: AgentExecutionMode::CliProcess,
                    backend: CliBackend::Codex,
                    model: None,
                    model_profile: None,
                    model_source: CliModelSelectionSource::Default,
                    thinking_level: ThinkingLevel::Default,
                    custom_executable: None,
                    custom_args: Vec::new(),
                    custom_risk_ack: false,
                    version: 1,
                    created_at: "2026-01-01T00:00:00Z".into(),
                    updated_at: "2026-01-01T00:00:00Z".into(),
                },
                recent_runs: Vec::new(),
                recent_events: Vec::new(),
            }],
            free_form_slots: Vec::new(),
            archived_free_form_slots: Vec::new(),
            discovery: Vec::new(),
        });
        model
    }

    #[test]
    fn selection_survives_refresh_and_recovers_when_an_agent_disappears() {
        let mut model = model();
        let snapshot = model.snapshot.clone().unwrap();
        model.reconcile_selection(&snapshot);
        assert_eq!(
            model.selected_agent,
            Some(AgentIdentifier::Canonical(AgentRole::BackendAgent))
        );
        model.selected_agent = Some(AgentIdentifier::FreeForm("deleted-slot".into()));
        model.reconcile_selection(&snapshot);
        assert_eq!(
            model.selected_agent,
            Some(AgentIdentifier::Canonical(AgentRole::BackendAgent))
        );
        let generation = model.begin(true);
        assert!(model.apply_snapshot(
            generation,
            AgentsDashboardSnapshot {
                terminals: snapshot,
                fallbacks: vec![],
                free_form_bindings: vec![],
                api_executions: HashMap::new(),
                quota_deltas: Default::default(),
            }
        ));
        assert_eq!(
            model.selected_agent,
            Some(AgentIdentifier::Canonical(AgentRole::BackendAgent))
        );
        let mut empty = model.snapshot.clone().unwrap();
        empty.agents.clear();
        model.reconcile_selection(&empty);
        assert_eq!(model.selected_agent, None);
    }

    #[test]
    fn quiet_snapshot_blocks_an_overlapping_quiet_refresh() {
        let mut model = model();
        assert!(!model.in_flight());
        let generation = model.begin_quiet();
        assert!(model.in_flight());
        assert!(model.apply_snapshot(
            generation,
            AgentsDashboardSnapshot {
                terminals: model.snapshot.clone().unwrap(),
                fallbacks: vec![],
                free_form_bindings: vec![],
                api_executions: HashMap::new(),
                quota_deltas: Default::default(),
            }
        ));
        assert!(!model.in_flight());
    }

    #[test]
    fn live_terminal_events_insert_select_and_update_automatic_run() {
        let mut model = model();
        assert!(!model.record_event(event_envelope(TerminalRunStatus::Running, 1)));
        assert_eq!(
            model.selected_run_by_role.get(&AgentRole::BackendAgent),
            Some(&"run-1".to_string())
        );
        assert!(!model.record_event(event_envelope(TerminalRunStatus::Completed, 2)));
        let agent = &model.snapshot.as_ref().unwrap().agents[0];
        assert_eq!(agent.recent_runs[0].status, TerminalRunStatus::Completed);
        assert_eq!(model.events_by_run["run-1"].len(), 2);
    }

    #[test]
    fn live_terminal_events_report_sequence_gaps() {
        let mut model = model();
        assert!(model.record_event(event_envelope(TerminalRunStatus::Running, 3)));
    }

    #[test]
    fn live_terminal_event_buffer_keeps_only_the_latest_five_hundred_events() {
        let mut model = model();
        for sequence in 1..=1_000 {
            assert!(!model.record_event(event_envelope(TerminalRunStatus::Running, sequence,)));
        }

        let events = &model.events_by_run["run-1"];
        assert_eq!(events.len(), 500);
        assert_eq!(events.first().map(|event| event.sequence), Some(501));
        assert_eq!(events.last().map(|event| event.sequence), Some(1_000));
    }

    #[test]
    fn api_activity_is_project_scoped_redacted_and_bounded() {
        let mut model = model();
        model.viewer_enabled = true;
        for sequence in 1..=1_000 {
            model.record_llm(&LlmEventEnvelope {
                protocol_version: 1,
                execution_id: "execution-1".into(),
                generation_id: "generation-1".into(),
                sequence,
                context: Some(AgentExecutionContext {
                    project_id: "project-1".into(),
                    pulsation_id: Some("pulse-1".into()),
                    job_id: Some("job-1".into()),
                    target: AgentExecutionTarget::canonical(AgentRole::BackendAgent),
                    stage: "backend".into(),
                }),
                event: LlmStreamEvent::RouteStarted {
                    route_id: "Authorization: Bearer fixture-secret-token".into(),
                },
            });
        }

        let activity = &model.api_activity[&AgentIdentifier::Canonical(AgentRole::BackendAgent)];
        assert_eq!(activity.len(), 200);
        assert!(activity.iter().all(|line| line.contains("[REDACTED]")));
        assert!(
            activity
                .iter()
                .all(|line| !line.contains("fixture-secret-token"))
        );
    }

    #[test]
    fn batches_token_segments_and_redacts_a_secret_split_between_chunks() {
        let mut model = model();
        model.viewer_enabled = true;
        let context = AgentExecutionContext {
            project_id: "project-1".into(),
            pulsation_id: Some("pulse-1".into()),
            job_id: Some("job-1".into()),
            target: AgentExecutionTarget::canonical(AgentRole::BackendAgent),
            stage: "backend".into(),
        };
        let events = [
            LlmEventEnvelope {
                protocol_version: 1,
                execution_id: "execution-1".into(),
                generation_id: "generation-1".into(),
                sequence: 1,
                context: Some(context.clone()),
                event: LlmStreamEvent::TokenDelta {
                    text: "Authorization: Bearer fixture-".into(),
                },
            },
            LlmEventEnvelope {
                protocol_version: 1,
                execution_id: "execution-1".into(),
                generation_id: "generation-1".into(),
                sequence: 2,
                context: Some(context),
                event: LlmStreamEvent::TokenDelta {
                    text: "secret-token ✓".into(),
                },
            },
        ];

        assert!(!model.record_llm_batch(&events));
        let activity = &model.api_activity[&AgentIdentifier::Canonical(AgentRole::BackendAgent)];
        assert_eq!(activity.len(), 1);
        assert!(activity[0].contains("[REDACTED]"));
        assert!(!activity[0].contains("fixture-secret-token"));
        assert_eq!(model.api_sequences["execution-1"], 2);
    }
}
