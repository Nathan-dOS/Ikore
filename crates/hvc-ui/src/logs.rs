use std::collections::{HashMap, HashSet};
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
/// Bytes of live LLM output kept for the Logs monitor. The whole string is re-shaped on every
/// streamed batch, so an unbounded answer would stall the window; the persisted execution keeps
/// the full text.
const MAX_VISIBLE_OUTPUT_BYTES: usize = 32 * 1024;
const OUTPUT_TRIMMED_MARKER: &str = "[earlier output trimmed]
";

/// Keeps the last `MAX_VISIBLE_OUTPUT_BYTES` of `output` (on a char boundary) behind a marker.
fn keep_output_tail(output: String) -> String {
    if output.len() <= MAX_VISIBLE_OUTPUT_BYTES {
        return output;
    }
    let mut start = output.len() - MAX_VISIBLE_OUTPUT_BYTES;
    while !output.is_char_boundary(start) {
        start += 1;
    }
    let body = output[start..]
        .strip_prefix(OUTPUT_TRIMMED_MARKER)
        .unwrap_or(&output[start..]);
    format!("{OUTPUT_TRIMMED_MARKER}{body}")
}

pub(crate) type LogsFuture<T> =
    Pin<Box<dyn Future<Output = Result<T, UiCommandError>> + Send + 'static>>;

/// A pulsation group being accumulated in `grouped_runs`: `(pulsation_id, title, status, runs)`.
type PulsationGroupEntry = (Option<String>, String, Option<String>, Vec<AgentRunListItem>);

#[derive(Debug, Clone, Default)]
pub(crate) struct LogsSnapshot {
    pub(crate) runs: Vec<AgentRunListItem>,
    pub(crate) metrics: Vec<LlmRoutingMetric>,
}

pub(crate) trait LogsBackend: Send + Sync {
    fn observability(
        &self,
        project_id: Option<String>,
        pulsation_id: Option<String>,
    ) -> LogsFuture<LogsSnapshot>;
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
    fn observability(
        &self,
        project_id: Option<String>,
        pulsation_id: Option<String>,
    ) -> LogsFuture<LogsSnapshot> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    let persistence = core.persistence();
                    let metrics = persistence.get_llm_routing_metrics()?.metrics;
                    let runs = persistence
                        .list_agent_runs(ListAgentRunsRequest {
                            project_id,
                            pulsation_id,
                        })?
                        .runs;
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
pub(crate) struct PulsationLogGroup {
    pub(crate) key: String,
    pub(crate) pulsation_id: Option<String>,
    pub(crate) title: String,
    pub(crate) order_label: Option<String>,
    pub(crate) status: Option<String>,
    pub(crate) is_expanded: bool,
    pub(crate) runs: Vec<AgentRunListItem>,
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
    pub(crate) expanded_groups: HashSet<String>,
    expanded_initialized: bool,
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
        if !self.expanded_initialized
            && let Some(first_run) = snapshot.runs.first()
        {
            let initial_key = first_run
                .pulsation_id
                .clone()
                .unwrap_or_else(|| "general".to_string());
            self.expanded_groups.insert(initial_key);
            self.expanded_initialized = true;
        }
        self.snapshot = snapshot;
        true
    }

    pub(crate) fn toggle_group(&mut self, key: &str) {
        if self.expanded_groups.contains(key) {
            self.expanded_groups.remove(key);
        } else {
            self.expanded_groups.insert(key.to_string());
        }
    }

    #[allow(dead_code)]
    pub(crate) fn is_group_expanded(&self, key: &str) -> bool {
        self.expanded_groups.contains(key)
    }

    pub(crate) fn reset_expansion(&mut self) {
        self.expanded_initialized = false;
        self.expanded_groups.clear();
    }

    pub(crate) fn grouped_runs(&self) -> Vec<PulsationLogGroup> {
        let mut group_keys = Vec::new();
        let mut groups: HashMap<String, PulsationGroupEntry> = HashMap::new();

        for run in &self.snapshot.runs {
            let key = run
                .pulsation_id
                .clone()
                .unwrap_or_else(|| "general".to_string());
            if !groups.contains_key(&key) {
                group_keys.push(key.clone());
                let (pulsation_id, title, status) = if let Some(id) = &run.pulsation_id {
                    let title = run
                        .pulsation_name
                        .clone()
                        .unwrap_or_else(|| format!("Pulsation {}", &id[..id.len().min(8)]));
                    (Some(id.clone()), title, run.pulsation_status.clone())
                } else {
                    (None, "General / Terminal Runs".to_string(), None)
                };
                groups.insert(key.clone(), (pulsation_id, title, status, Vec::new()));
            }
            groups.get_mut(&key).unwrap().3.push(run.clone());
        }

        let pulsation_keys: Vec<&String> = group_keys.iter().filter(|k| *k != "general").collect();
        let total_pulsations = pulsation_keys.len();
        let order_map: HashMap<String, usize> = pulsation_keys
            .iter()
            .enumerate()
            .map(|(idx, key)| ((*key).clone(), total_pulsations.saturating_sub(idx)))
            .collect();

        group_keys
            .into_iter()
            .filter_map(|key| {
                let (pulsation_id, title, status, mut runs) = groups.remove(&key)?;
                runs.sort_by(|a, b| a.created_at.cmp(&b.created_at));
                let order_label = order_map.get(&key).map(|order| format!("#{order}"));
                let is_expanded = self.expanded_groups.contains(&key);
                Some(PulsationLogGroup {
                    key,
                    pulsation_id,
                    title,
                    order_label,
                    status,
                    is_expanded,
                    runs,
                })
            })
            .collect()
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

    pub(crate) fn discard_poll(&mut self, generation: u64) {
        if generation == self.poll_generation {
            self.poll_in_flight = false;
            self.loading = false;
        }
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
        self.record_llm_batch(std::iter::once(envelope))
    }

    /// Applies a contiguous group of LLM events while materializing `visible_output` once. The
    /// event receiver still validates every sequence number, so batching presentation never
    /// hides a gap or changes the persisted event order.
    pub(crate) fn record_llm_batch(
        &mut self,
        envelopes: impl IntoIterator<Item = LlmEventEnvelope>,
    ) -> SequenceClassification {
        let mut result = SequenceClassification::Stale;
        let mut pending_output: Option<String> = None;

        for envelope in envelopes {
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
                continue;
            }
            result = classification;
            self.active_execution_id = Some(envelope.execution_id.clone().into());
            self.active_generation_id = Some(envelope.generation_id.clone().into());
            if classification == SequenceClassification::Gap {
                self.visible_output_complete = false;
                self.resync_pending = true;
                result = SequenceClassification::Gap;
                continue;
            }
            if self.resync_pending {
                continue;
            }

            match envelope.event {
                LlmStreamEvent::RouteStarted { .. } => {
                    pending_output = Some(String::new());
                    self.visible_output_complete = false;
                }
                LlmStreamEvent::TokenDelta { text } => {
                    pending_output
                        .get_or_insert_with(|| self.visible_output.to_string())
                        .push_str(&text);
                }
                LlmStreamEvent::RouteCompleted { .. } => self.visible_output_complete = true,
                LlmStreamEvent::RouteFailed { .. } => self.visible_output_complete = false,
                _ => {}
            }
        }

        if let Some(output) = pending_output {
            self.visible_output = keep_output_tail(output).into();
        }
        result
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
mod output_tail_tests {
    use super::*;

    #[test]
    fn live_output_keeps_a_bounded_tail_on_a_char_boundary() {
        let short = "answer".to_string();
        assert_eq!(keep_output_tail(short.clone()), short);

        let long = format!("{}{}", "→".repeat(MAX_VISIBLE_OUTPUT_BYTES), "the end");
        let kept = keep_output_tail(long);
        assert!(kept.starts_with(OUTPUT_TRIMMED_MARKER));
        assert!(kept.ends_with("the end"));
        assert!(kept.len() <= MAX_VISIBLE_OUTPUT_BYTES + OUTPUT_TRIMMED_MARKER.len());
        // Trimming again (the next streamed batch) never stacks markers.
        let again = keep_output_tail(format!("{kept}{}", "x".repeat(10)));
        assert_eq!(again.matches(OUTPUT_TRIMMED_MARKER).count(), 1);
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
    fn batches_contiguous_deltas_without_losing_sequence_or_output() {
        let mut model = LogsModel::default();
        let mut second = event(2);
        second.event = LlmStreamEvent::TokenDelta { text: "y".into() };
        let mut third = event(3);
        third.event = LlmStreamEvent::TokenDelta { text: "z".into() };

        assert_eq!(
            model.record_llm_batch(vec![event(1), second, third]),
            SequenceClassification::Next
        );
        assert_eq!(model.visible_output, "xyz");
        assert_eq!(model.events.len(), 3);
        assert_eq!(model.events.last().and_then(|event| event.sequence), Some(3));
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

    #[test]
    fn groups_runs_by_pulsation_with_correct_order_and_expansion() {
        use hvc_core::persistence_service::AgentRunListItem;
        let mut model = LogsModel::default();

        let run = |id: &str, pulse: Option<(&str, &str)>, agent: &str, status: &str, created: &str| AgentRunListItem {
            id: id.into(),
            job_id: format!("job-{id}"),
            agent_name: agent.into(),
            status: status.into(),
            input_tokens: Some(100),
            output_tokens: Some(50),
            provider: Some("anthropic".into()),
            model: Some("claude-3-5".into()),
            estimated_cost_usd: Some(0.001),
            last_error: if status == "failed" { Some("syntax error".into()) } else { None },
            started_at: Some(created.into()),
            finished_at: Some(created.into()),
            raw_output: Some("raw output".into()),
            structured_output: None,
            created_at: created.into(),
            updated_at: created.into(),
            pulsation_id: pulse.map(|p| p.0.into()),
            pulsation_name: pulse.map(|p| p.1.into()),
            pulsation_status: pulse.map(|_| "done".into()),
            job_kind: Some("pipeline_stage".into()),
            duration_ms: Some(1200),
            cancellation_reason: None,
            validation_error: None,
            tool_trace: None,
        };

        // Snapshot with 4 runs:
        // - run-4 in pulse-2 (created at 10:30)
        // - run-3 in pulse-1 (created at 10:20)
        // - run-2 in pulse-1 (created at 10:10)
        // - run-1 standalone (created at 10:00)
        let runs = vec![
            run("run-4", Some(("pulse-2", "Feature Two")), "Spec Agent", "completed", "2026-09-16T10:30:00Z"),
            run("run-3", Some(("pulse-1", "Feature One")), "Task Specifier", "failed", "2026-09-16T10:20:00Z"),
            run("run-2", Some(("pulse-1", "Feature One")), "Spec Agent", "completed", "2026-09-16T10:10:00Z"),
            run("run-1", None, "Terminal Agent", "completed", "2026-09-16T10:00:00Z"),
        ];

        let snapshot = super::LogsSnapshot {
            runs: runs.clone(),
            metrics: Vec::new(),
        };

        let generation = model.begin_poll().unwrap();
        assert!(model.apply_poll(generation, snapshot));

        // The top group (pulse-2) is expanded by default (Option A)
        assert!(model.is_group_expanded("pulse-2"));
        assert!(!model.is_group_expanded("pulse-1"));

        let groups = model.grouped_runs();
        assert_eq!(groups.len(), 3);

        // Group 1: pulse-2 (newest pulsation -> #2)
        assert_eq!(groups[0].key, "pulse-2");
        assert_eq!(groups[0].title, "Feature Two");
        assert_eq!(groups[0].order_label.as_deref(), Some("#2"));
        assert!(groups[0].is_expanded);
        assert_eq!(groups[0].runs.len(), 1);

        // Group 2: pulse-1 (older pulsation -> #1)
        assert_eq!(groups[1].key, "pulse-1");
        assert_eq!(groups[1].title, "Feature One");
        assert_eq!(groups[1].order_label.as_deref(), Some("#1"));
        assert!(!groups[1].is_expanded);
        assert_eq!(groups[1].runs.len(), 2);
        // Inside pulse-1, runs are ordered chronologically: run-2 (10:10) then run-3 (10:20)
        assert_eq!(groups[1].runs[0].id, "run-2");
        assert_eq!(groups[1].runs[1].id, "run-3");

        // Group 3: standalone/general runs
        assert_eq!(groups[2].key, "general");
        assert_eq!(groups[2].pulsation_id, None);
        assert_eq!(groups[2].runs.len(), 1);

        // Toggle expansion of pulse-1
        model.toggle_group("pulse-1");
        assert!(model.is_group_expanded("pulse-1"));
        let re_groups = model.grouped_runs();
        assert!(re_groups[1].is_expanded);

        // Collapse both pulse-1 and pulse-2 so all groups are closed
        model.toggle_group("pulse-1");
        model.toggle_group("pulse-2");
        assert!(!model.is_group_expanded("pulse-1"));
        assert!(!model.is_group_expanded("pulse-2"));

        // Subsequent poll must NOT re-expand pulse-2
        let gen2 = model.begin_poll().unwrap();
        assert!(model.apply_poll(gen2, super::LogsSnapshot {
            runs: runs.clone(),
            metrics: Vec::new(),
        }));
        assert!(!model.is_group_expanded("pulse-1"));
        assert!(!model.is_group_expanded("pulse-2"));
    }
}
