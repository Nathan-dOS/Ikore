use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use gpui::SharedString;
use hvc_core::AppCore;
use hvc_core::llm::{LlmEventEnvelope, LlmExecutionSnapshot, LlmRoutingMetric, LlmStreamEvent};
use hvc_core::persistence_service::{
    AgentRunListItem, GetLlmExecutionSnapshotRequest, ListAgentRunsRequest, PersistenceCommandError,
};
use hvc_core::workspace_service::WorkspaceChangedEvent;

use crate::errors::UiCommandError;

const MAX_VISIBLE_EVENTS: usize = 200;

pub(crate) type LogsFuture<T> =
    Pin<Box<dyn Future<Output = Result<T, UiCommandError>> + Send + 'static>>;

#[derive(Debug, Clone, Default)]
pub(crate) struct LogsSnapshot {
    pub(crate) runs: Vec<AgentRunListItem>,
    pub(crate) metrics: Vec<LlmRoutingMetric>,
}

pub(crate) trait LogsBackend: Send + Sync {
    fn observability(&self, pulsation_id: Option<String>) -> LogsFuture<LogsSnapshot>;
    fn execution_snapshot(&self, execution_id: String) -> LogsFuture<LlmExecutionSnapshot>;
}

pub(crate) struct CoreLogsBackend {
    core: Arc<AppCore>,
}

impl CoreLogsBackend {
    pub(crate) fn new(core: Arc<AppCore>) -> Self {
        Self { core }
    }
}

fn command_error(error: hvc_core::persistence_service::PersistenceServiceError) -> UiCommandError {
    UiCommandError::from(PersistenceCommandError::from(error))
}

impl LogsBackend for CoreLogsBackend {
    fn observability(&self, pulsation_id: Option<String>) -> LogsFuture<LogsSnapshot> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    let persistence = core.persistence();
                    let metrics = persistence.get_llm_routing_metrics()?.metrics;
                    let runs = match pulsation_id {
                        Some(pulsation_id) => {
                            persistence
                                .list_agent_runs(ListAgentRunsRequest { pulsation_id })?
                                .runs
                        }
                        None => Vec::new(),
                    };
                    Ok(LogsSnapshot { runs, metrics })
                })
                .await
                .map_err(|error| {
                    UiCommandError::worker(format!(
                        "worker de observabilidade interrompido: {error}"
                    ))
                })?
                .map_err(command_error)
        })
    }

    fn execution_snapshot(&self, execution_id: String) -> LogsFuture<LlmExecutionSnapshot> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    core.persistence()
                        .get_llm_execution_snapshot(GetLlmExecutionSnapshotRequest { execution_id })
                })
                .await
                .map_err(|error| {
                    UiCommandError::worker(format!("resync worker interrupted: {error}"))
                })?
                .map(|response| response.snapshot)
                .map_err(command_error)
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SequenceClassification {
    Next,
    Stale,
    Gap,
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeEventRecord {
    pub(crate) category: SharedString,
    pub(crate) name: SharedString,
    pub(crate) summary: SharedString,
    pub(crate) sequence: Option<u64>,
    pub(crate) timestamp: SharedString,
}

#[derive(Debug, Default)]
pub(crate) struct LogsModel {
    pub(crate) collapsed_routing: bool,
    pub(crate) collapsed_events: bool,
    pub(crate) visible_event_limit: usize,
    routing_visibility_initialized: bool,
    pub(crate) snapshot: LogsSnapshot,
    pub(crate) loading: bool,
    pub(crate) poll_in_flight: bool,
    pub(crate) selected_run_id: Option<SharedString>,
    pub(crate) events: Vec<RuntimeEventRecord>,
    pub(crate) visible_output: SharedString,
    pub(crate) visible_output_complete: bool,
    pub(crate) active_execution_id: Option<SharedString>,
    pub(crate) active_generation_id: Option<SharedString>,
    pub(crate) error: Option<UiCommandError>,
    resync_pending: bool,
    poll_generation: u64,
    resync_generation: u64,
    last_sequences: HashMap<(String, String), u64>,
}

impl LogsModel {
    pub(crate) fn begin_poll(&mut self) -> Option<u64> {
        if self.poll_in_flight {
            return None;
        }
        self.poll_in_flight = true;
        self.loading = self.snapshot.runs.is_empty() && self.snapshot.metrics.is_empty();
        self.poll_generation = self.poll_generation.saturating_add(1);
        Some(self.poll_generation)
    }

    pub(crate) fn apply_poll(&mut self, generation: u64, snapshot: LogsSnapshot) -> bool {
        if generation != self.poll_generation {
            return false;
        }
        self.poll_in_flight = false;
        self.loading = false;
        self.error = None;
        if self
            .selected_run_id
            .as_ref()
            .is_none_or(|selected| !snapshot.runs.iter().any(|run| run.id == selected.as_ref()))
        {
            self.selected_run_id = snapshot.runs.first().map(|run| run.id.clone().into());
        }
        if !self.routing_visibility_initialized && !snapshot.metrics.is_empty() {
            self.collapsed_routing = snapshot.metrics.len() > 3;
            self.routing_visibility_initialized = true;
        }
        self.snapshot = snapshot;
        true
    }

    pub(crate) fn visible_events(&self) -> impl Iterator<Item = &RuntimeEventRecord> {
        self.events
            .iter()
            .rev()
            .take(self.visible_event_limit.max(50))
    }

    pub(crate) fn show_more_events(&mut self) {
        self.visible_event_limit = self.visible_event_limit.max(50).saturating_add(50);
    }

    pub(crate) fn fail_poll(&mut self, generation: u64, error: UiCommandError) -> bool {
        if generation != self.poll_generation {
            return false;
        }
        self.poll_in_flight = false;
        self.loading = false;
        self.error = Some(error);
        true
    }

    pub(crate) fn record_workspace(&mut self, event: WorkspaceChangedEvent) {
        self.push_event(RuntimeEventRecord {
            category: "watcher".into(),
            name: event.kind.into(),
            summary: event.relative_path.into(),
            sequence: None,
            timestamp: crate::time::current_time_only().into(),
        });
    }

    pub(crate) fn record_llm(&mut self, envelope: LlmEventEnvelope) -> SequenceClassification {
        let classification = self.classify(&envelope);
        let summary = llm_summary(&envelope.event);
        self.push_event(RuntimeEventRecord {
            category: "llm".into(),
            name: envelope.event.event_name().into(),
            summary: summary.into(),
            sequence: Some(envelope.sequence),
            timestamp: crate::time::current_time_only().into(),
        });

        if classification == SequenceClassification::Stale {
            return classification;
        }

        self.active_execution_id = Some(envelope.execution_id.clone().into());
        self.active_generation_id = Some(envelope.generation_id.clone().into());
        if classification == SequenceClassification::Gap {
            self.visible_output_complete = false;
            self.resync_pending = true;
            return classification;
        }

        if self.resync_pending {
            return classification;
        }

        match envelope.event {
            LlmStreamEvent::RouteStarted { .. } => {
                self.visible_output = "".into();
                self.visible_output_complete = false;
            }
            LlmStreamEvent::TokenDelta { text } => {
                let mut output = self.visible_output.to_string();
                output.push_str(&text);
                self.visible_output = output.into();
            }
            LlmStreamEvent::RouteCompleted { .. } => self.visible_output_complete = true,
            LlmStreamEvent::RouteFailed { .. } => self.visible_output_complete = false,
            _ => {}
        }
        classification
    }

    pub(crate) fn begin_resync(&mut self) -> u64 {
        self.resync_generation = self.resync_generation.saturating_add(1);
        self.resync_generation
    }

    pub(crate) fn apply_execution_snapshot(
        &mut self,
        request_generation: u64,
        snapshot: LlmExecutionSnapshot,
    ) -> bool {
        if request_generation != self.resync_generation {
            return false;
        }
        if self
            .active_execution_id
            .as_ref()
            .is_none_or(|id| id.as_ref() != snapshot.execution_id)
            || self
                .active_generation_id
                .as_ref()
                .is_none_or(|id| id.as_ref() != snapshot.generation_id)
        {
            return false;
        }
        self.visible_output = snapshot.visible_output.unwrap_or_default().into();
        self.visible_output_complete = snapshot.visible_output_complete;
        self.resync_pending = false;
        true
    }

    pub(crate) fn selected_run(&self) -> Option<&AgentRunListItem> {
        let id = self.selected_run_id.as_ref()?;
        self.snapshot.runs.iter().find(|run| run.id == id.as_ref())
    }

    fn classify(&mut self, envelope: &LlmEventEnvelope) -> SequenceClassification {
        let key = (
            envelope.execution_id.clone(),
            envelope.generation_id.clone(),
        );
        let previous = self.last_sequences.get(&key).copied().unwrap_or(0);
        if envelope.sequence <= previous {
            return SequenceClassification::Stale;
        }
        self.last_sequences.insert(key, envelope.sequence);
        if envelope.sequence == previous.saturating_add(1) {
            SequenceClassification::Next
        } else {
            SequenceClassification::Gap
        }
    }

    fn push_event(&mut self, event: RuntimeEventRecord) {
        self.events.push(event);
        let overflow = self.events.len().saturating_sub(MAX_VISIBLE_EVENTS);
        if overflow > 0 {
            self.events.drain(0..overflow);
        }
    }
}

fn llm_summary(event: &LlmStreamEvent) -> String {
    match event {
        LlmStreamEvent::RouteStarted { route_id } => format!("route {route_id} started"),
        LlmStreamEvent::AttemptStarted {
            ordinal,
            provider_id,
            model,
            ..
        } => format!("attempt {ordinal}: {provider_id}/{model}"),
        LlmStreamEvent::TokenDelta { text } => format!("{} character(s)", text.chars().count()),
        LlmStreamEvent::AttemptFailed {
            ordinal,
            error_code,
        } => format!("attempt {ordinal} failed: {error_code}"),
        LlmStreamEvent::RouteCompleted {
            provider_id, model, ..
        } => format!("completed by {provider_id}/{model}"),
        LlmStreamEvent::RouteFailed { error_code, .. } => {
            format!("route failed: {error_code}")
        }
        LlmStreamEvent::ResilienceChanged {
            scope_id,
            from_state,
            to_state,
            ..
        } => format!("{scope_id}: {from_state} → {to_state}"),
    }
}

#[cfg(test)]
mod tests {
    use hvc_core::llm::{LlmEventEnvelope, LlmExecutionSnapshot, LlmStreamEvent};
    use serde_json::json;

    use super::{LogsModel, SequenceClassification};

    fn event(sequence: u64) -> LlmEventEnvelope {
        LlmEventEnvelope {
            protocol_version: 1,
            execution_id: "execution-1".into(),
            generation_id: "generation-1".into(),
            sequence,
            context: None,
            event: LlmStreamEvent::TokenDelta { text: "x".into() },
        }
    }

    #[test]
    fn classifies_consecutive_stale_and_gap_llm_events() {
        let mut model = LogsModel::default();

        assert_eq!(model.record_llm(event(1)), SequenceClassification::Next);
        assert_eq!(model.record_llm(event(1)), SequenceClassification::Stale);
        assert_eq!(model.record_llm(event(3)), SequenceClassification::Gap);
        assert_eq!(model.record_llm(event(4)), SequenceClassification::Next);
        assert_eq!(model.visible_output, "x");
        assert!(!model.visible_output_complete);
    }

    #[test]
    fn caps_visible_runtime_events_at_two_hundred() {
        let mut model = LogsModel::default();
        for sequence in 1..=225 {
            model.record_llm(event(sequence));
        }
        assert_eq!(model.events.len(), 200);
        assert_eq!(model.events[0].sequence, Some(26));
        assert_eq!(model.visible_events().count(), 50);
        assert_eq!(model.visible_events().next().unwrap().sequence, Some(225));
        model.show_more_events();
        assert_eq!(model.visible_events().count(), 100);
        assert_eq!(model.events.len(), 200);
    }

    #[test]
    fn resynchronizes_gap_and_rejects_a_late_snapshot() {
        let mut model = LogsModel::default();
        model.record_llm(event(1));
        assert_eq!(model.record_llm(event(3)), SequenceClassification::Gap);
        let stale_request = model.begin_resync();
        let current_request = model.begin_resync();
        let snapshot = || LlmExecutionSnapshot {
            execution_id: "execution-1".into(),
            generation_id: "generation-1".into(),
            route_id: "route-1".into(),
            status: "streaming".into(),
            decision: json!({}),
            next_eligible_at: None,
            visible_output: Some("reliable snapshot".into()),
            visible_output_complete: false,
            attempts: Vec::new(),
        };

        assert!(!model.apply_execution_snapshot(stale_request, snapshot()));
        assert!(model.apply_execution_snapshot(current_request, snapshot()));
        assert_eq!(model.visible_output, "reliable snapshot");
        assert_eq!(model.record_llm(event(4)), SequenceClassification::Next);
        assert_eq!(model.visible_output, "reliable snapshotx");
    }

    #[test]
    fn prevents_overlapping_observability_polls() {
        let mut model = LogsModel::default();
        let generation = model.begin_poll().unwrap();
        assert!(model.begin_poll().is_none());
        assert!(model.apply_poll(generation, Default::default()));
        assert!(model.begin_poll().is_some());
    }
}
