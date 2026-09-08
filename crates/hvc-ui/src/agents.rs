use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use hvc_core::AppCore;
use hvc_core::llm::{AgentRole, LlmEventEnvelope, LlmStreamEvent};
use hvc_core::persistence_service::{
    CliFallbackCheckpoint, PersistenceCommandError, ResolveCliFallbackRequest,
    ResolveCliFallbackResponse,
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
    fn resolve_fallback(
        &self,
        request: ResolveCliFallbackRequest,
    ) -> AgentsFuture<ResolveCliFallbackResponse>;
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
            let fallbacks = core
                .persistence()
                .list_cli_fallbacks(&terminals.project_id)
                .map_err(persistence_error)?;
            Ok(AgentsDashboardSnapshot {
                terminals,
                fallbacks,
            })
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
            let fallbacks = core
                .persistence()
                .list_cli_fallbacks(&terminals.project_id)
                .map_err(persistence_error)?;
            Ok(AgentsDashboardSnapshot {
                terminals,
                fallbacks,
            })
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
        Box::pin(async move { terminals.start_manual(request).map_err(terminal_error) })
    }

    fn start_free_form(&self, request: StartFreeFormRunRequest) -> AgentsFuture<TerminalRun> {
        let terminals = self.core.terminals();
        let runtime = self.core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || terminals.start_free_form(request))
                .await
                .map_err(|error| {
                    UiCommandError::worker(format!("worker de agente livre interrompido: {error}"))
                })?
                .map_err(terminal_error)
        })
    }

    fn resume_manual(&self, request: ResumeManualRunRequest) -> AgentsFuture<TerminalRun> {
        let terminals = self.core.terminals();
        let runtime = self.core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || terminals.resume_manual(request))
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
                    UiCommandError::worker(format!("worker de agente livre interrompido: {error}"))
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
                    UiCommandError::worker(format!("worker de agente livre interrompido: {error}"))
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
                    UiCommandError::worker(format!("worker de agente livre interrompido: {error}"))
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentsTab {
    Cli,
    Api,
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

impl TerminalEventFilter {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::All => "todos",
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AgentIdentifier {
    Canonical(AgentRole),
    FreeForm(String),
}

#[derive(Debug)]
pub(crate) struct AgentsModel {
    pub(crate) selected_agent: Option<AgentIdentifier>,
    pub(crate) tab: AgentsTab,
    pub(crate) snapshot: Option<TerminalsSnapshot>,
    pub(crate) fallbacks: Vec<CliFallbackCheckpoint>,
    pub(crate) events_by_run: HashMap<String, Vec<TerminalEvent>>,
    pub(crate) selected_run_by_role: HashMap<AgentRole, String>,
    pub(crate) selected_run_by_slot: HashMap<String, String>,
    pub(crate) event_filters: HashMap<String, TerminalEventFilter>,
    pub(crate) follow_tail: HashMap<String, bool>,
    pub(crate) api_activity: HashMap<AgentRole, Vec<String>>,
    pub(crate) model_catalogs: HashMap<CliBackend, CliModelCatalogSnapshot>,
    pub(crate) active_model_picker: Option<CliModelPickerState>,
    pub(crate) catalog_loading_backend: Option<CliBackend>,
    pub(crate) catalog_error: Option<String>,
    pub(crate) viewer_enabled: bool,
    pub(crate) selected_api_role: Option<AgentRole>,
    api_sequences: HashMap<String, u64>,
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
            tab: AgentsTab::Cli,
            snapshot: None,
            fallbacks: Vec::new(),
            events_by_run: HashMap::new(),
            selected_run_by_role: HashMap::new(),
            selected_run_by_slot: HashMap::new(),
            event_filters: HashMap::new(),
            follow_tail: HashMap::new(),
            api_activity: HashMap::new(),
            model_catalogs: HashMap::new(),
            active_model_picker: None,
            catalog_loading_backend: None,
            catalog_error: None,
            viewer_enabled: true,
            selected_api_role: None,
            api_sequences: HashMap::new(),
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
        self.loading = loading;
        self.busy = !loading;
        self.error = None;
        self.notice = None;
        self.generation
    }

    pub(crate) fn apply_snapshot(
        &mut self,
        generation: u64,
        snapshot: AgentsDashboardSnapshot,
    ) -> bool {
        if generation != self.generation {
            return false;
        }
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

    pub(crate) fn record_llm(&mut self, envelope: &LlmEventEnvelope) {
        if !self.viewer_enabled {
            return;
        }
        let Some(context) = envelope.context.as_ref() else {
            return;
        };
        if self
            .snapshot
            .as_ref()
            .is_none_or(|snapshot| snapshot.project_id != context.project_id)
        {
            return;
        }
        let last = self
            .api_sequences
            .entry(envelope.execution_id.clone())
            .or_default();
        if envelope.sequence <= *last {
            return;
        }
        *last = envelope.sequence;
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
            LlmStreamEvent::TokenDelta { text } => text.clone(),
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
        };
        let line = hvc_core::secret_redactor::redact_secrets(&line)
            .map(|result| result.text)
            .unwrap_or_else(|_| "[content unavailable after redaction]".into());
        let activity = self.api_activity.entry(context.agent_role).or_default();
        if matches!(&envelope.event, LlmStreamEvent::TokenDelta { .. })
            && activity.last_mut().is_some_and(|previous| {
                if previous.starts_with("output · ") {
                    previous.push_str(&line);
                    if previous.chars().count() > 64 * 1024 {
                        *previous = previous.chars().take(64 * 1024).collect();
                        previous.push_str("\n[visible output truncated]");
                    }
                    if let Ok(redacted) = hvc_core::secret_redactor::redact_secrets(previous) {
                        *previous = redacted.text;
                    }
                    true
                } else {
                    false
                }
            })
        {
            return;
        }
        activity.push(
            if matches!(&envelope.event, LlmStreamEvent::TokenDelta { .. }) {
                format!("output · {line}")
            } else {
                line
            },
        );
        if activity.len() > 200 {
            activity.drain(..activity.len() - 200);
        }
        if matches!(
            &envelope.event,
            LlmStreamEvent::RouteCompleted { .. } | LlmStreamEvent::RouteFailed { .. }
        ) {
            self.api_sequences.remove(&envelope.execution_id);
        }
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
                fallbacks: vec![]
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
                    agent_role: AgentRole::BackendAgent,
                    stage: "backend".into(),
                }),
                event: LlmStreamEvent::RouteStarted {
                    route_id: "Authorization: Bearer fixture-secret-token".into(),
                },
            });
        }

        let activity = &model.api_activity[&AgentRole::BackendAgent];
        assert_eq!(activity.len(), 200);
        assert!(activity.iter().all(|line| line.contains("[REDACTED]")));
        assert!(
            activity
                .iter()
                .all(|line| !line.contains("fixture-secret-token"))
        );
    }
}
