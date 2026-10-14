//! Versioned JSONL accounting: final snapshots for Claude/Gemini, turns for
//! Codex, uniquely identified step-finish parts for OpenCode.
use super::{TokenCounts, UsageContext, UsageMeasurement, record_usage, record_usage_on};
use crate::terminals::{CliBackend, sanitize_terminal_text_bounded};
use rusqlite::params;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Mutex,
};

#[derive(Default)]
pub struct CliUsageTracker {
    units: BTreeMap<String, UsageMeasurement>,
    model: Option<String>,
    turn: u64,
    incomplete: bool,
    live: ClaudeLiveUsage,
}

/// Characters per token assumed for text that is still streaming. Real text
/// and code run at three to four characters per token, so dividing by four
/// under-counts: the estimate can miss a runaway message by a few percent but
/// never ends a healthy one early. The exact figure replaces it when the
/// message closes.
const LIVE_CHARS_PER_TOKEN: u64 = 4;

/// What a Claude stream has reported before its final `result` event.
///
/// Claude's `result` snapshot is the only place the accounting above reads, so
/// a run that is stopped early would otherwise record nothing at all. Each
/// message states its prompt size when it opens (`message_start`) and its exact
/// output when it closes (`message_delta`); the `assistant` frames repeat a
/// stale figure of one to four tokens and are only a lower bound.
#[derive(Default)]
struct ClaudeLiveUsage {
    messages: BTreeMap<String, LiveMessage>,
    current: Option<String>,
}

#[derive(Default)]
struct LiveMessage {
    input: i64,
    cache_read: i64,
    cache_write: i64,
    /// Exact output once the message closed, else the largest figure seen.
    output: i64,
    closed: bool,
    streamed_chars: u64,
}

impl LiveMessage {
    fn output_tokens(&self) -> u64 {
        let reported = self.output.max(0) as u64;
        if self.closed {
            reported
        } else {
            reported.max(self.streamed_chars / LIVE_CHARS_PER_TOKEN)
        }
    }
}

impl ClaudeLiveUsage {
    fn observe(&mut self, value: &Value) {
        match value.get("type").and_then(Value::as_str) {
            Some("stream_event") => {
                let event = value.get("event").unwrap_or(&Value::Null);
                match event.get("type").and_then(Value::as_str) {
                    Some("message_start") => {
                        let Some(id) = event.pointer("/message/id").and_then(Value::as_str) else {
                            return;
                        };
                        let usage = event.pointer("/message/usage").unwrap_or(&Value::Null);
                        let message = self.messages.entry(id.to_owned()).or_default();
                        message.input = number(usage, "/input_tokens").unwrap_or(0);
                        message.cache_read = number(usage, "/cache_read_input_tokens").unwrap_or(0);
                        message.cache_write =
                            number(usage, "/cache_creation_input_tokens").unwrap_or(0);
                        message.output = message
                            .output
                            .max(number(usage, "/output_tokens").unwrap_or(0));
                        self.current = Some(id.to_owned());
                    }
                    Some("content_block_delta") => {
                        let delta = event.get("delta").unwrap_or(&Value::Null);
                        let chars = ["text", "thinking", "partial_json"]
                            .iter()
                            .filter_map(|key| delta.get(*key).and_then(Value::as_str))
                            .map(|text| text.chars().count() as u64)
                            .sum::<u64>();
                        if let Some(message) = self.current_message() {
                            message.streamed_chars = message.streamed_chars.saturating_add(chars);
                        }
                    }
                    Some("message_delta") => {
                        let reported = event
                            .pointer("/usage/output_tokens")
                            .and_then(Value::as_i64)
                            .filter(|n| *n >= 0);
                        if let (Some(reported), Some(message)) = (reported, self.current_message())
                        {
                            message.output = reported;
                            message.closed = true;
                        }
                    }
                    _ => {}
                }
            }
            Some("assistant") => {
                // Without partial-message frames this is all there is: a lower
                // bound, never an estimate to act on alone.
                let Some(id) = value.pointer("/message/id").and_then(Value::as_str) else {
                    return;
                };
                let reported = number(value, "/message/usage/output_tokens").unwrap_or(0);
                let message = self.messages.entry(id.to_owned()).or_default();
                message.output = message.output.max(reported);
            }
            _ => {}
        }
    }

    fn current_message(&mut self) -> Option<&mut LiveMessage> {
        let id = self.current.as_deref()?;
        self.messages.get_mut(id)
    }

    fn output_tokens(&self) -> u64 {
        self.messages
            .values()
            .map(LiveMessage::output_tokens)
            .fold(0, u64::saturating_add)
    }

    /// Prompt and output tokens for the partial record of a stopped run.
    fn measurement(&self, model: Option<String>) -> Option<UsageMeasurement> {
        if self.messages.is_empty() {
            return None;
        }
        let sum = |field: fn(&LiveMessage) -> i64| {
            self.messages
                .values()
                .map(field)
                .try_fold(0i64, i64::checked_add)
        };
        let (input, read, write) = (
            sum(|m| m.input)?,
            sum(|m| m.cache_read)?,
            sum(|m| m.cache_write)?,
        );
        Some(UsageMeasurement {
            counts: TokenCounts {
                input_tokens: input.checked_add(read)?.checked_add(write),
                output_tokens: i64::try_from(self.output_tokens()).ok(),
                cache_read_tokens: Some(read),
                cache_write_tokens: Some(write),
                ..Default::default()
            },
            observed_model: model,
            // Open messages are estimated, so the figure is a floor.
            partial: true,
        })
    }
}

fn number(value: &Value, key: &str) -> Option<i64> {
    value
        .pointer(key)
        .and_then(Value::as_i64)
        .filter(|n| *n >= 0)
}
fn plus(value: Option<i64>, extra: Option<i64>) -> Option<i64> {
    value.and_then(|n| n.checked_add(extra.unwrap_or(0)))
}
/// True when a per-model entry actually accounts for something. Gemini and
/// Claude report every model the session knows about, including ones that
/// never ran; persisting those adds an all-zero row beside the real one and
/// makes a run look like it used two models.
fn is_measured(counts: &TokenCounts) -> bool {
    [
        counts.input_tokens,
        counts.output_tokens,
        counts.total_tokens,
        counts.cache_read_tokens,
        counts.cache_write_tokens,
        counts.reasoning_tokens,
    ]
    .iter()
    .any(|value| value.unwrap_or(0) > 0)
}

fn snake_counts(value: &Value) -> TokenCounts {
    // Gemini bills thinking separately from `output_tokens` and only folds it
    // into `total_tokens`. Leaving it unattributed makes in+out disagree with
    // the total and hides the budget that reasoning models actually consume.
    let reasoning = number(value, "/thoughts_tokens").or_else(|| number(value, "/thinking_tokens"));
    TokenCounts {
        input_tokens: number(value, "/input_tokens"),
        output_tokens: number(value, "/output_tokens"),
        total_tokens: number(value, "/total_tokens"),
        reasoning_tokens: reasoning,
        ..Default::default()
    }
}

/// `agy` 1.3.5 counts cache reads apart from `input_tokens` and folds thinking
/// into `output_tokens` (80 output with 72 thinking for an eight-token answer).
fn antigravity_counts(usage: &Value) -> TokenCounts {
    let read = number(usage, "/cache_read_tokens");
    TokenCounts {
        input_tokens: plus(number(usage, "/input_tokens"), read),
        output_tokens: number(usage, "/output_tokens"),
        cache_read_tokens: read,
        reasoning_tokens: number(usage, "/thinking_tokens"),
        ..Default::default()
    }
}

impl CliUsageTracker {
    /// Returns changed units and whether a final model snapshot replaces a
    /// previous aggregate snapshot. Parsing never searches assistant text.
    fn observe(
        &mut self,
        backend: CliBackend,
        value: &Value,
    ) -> (bool, Vec<(String, UsageMeasurement)>) {
        let kind = value.get("type").and_then(Value::as_str).unwrap_or("");
        if matches!(
            kind,
            "init" | "system" | "thread.started" | "session.created"
        ) {
            self.model = value
                .get("model")
                .or_else(|| value.get("modelID"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or(self.model.take());
        }
        if backend == CliBackend::Claude {
            self.live.observe(value);
        }
        let mut changes = Vec::new();
        let mut replace = false;
        match backend {
            CliBackend::Codex => {
                if kind == "turn.started" {
                    self.turn = self.turn.saturating_add(1);
                    changes.push((
                        format!("turn:{}", self.turn),
                        UsageMeasurement {
                            observed_model: self.model.clone(),
                            ..Default::default()
                        },
                    ));
                }
                if kind == "turn.completed"
                    && let Some(usage) = value.get("usage")
                {
                    let mut counts = snake_counts(usage);
                    counts.cache_read_tokens = number(usage, "/cached_input_tokens");
                    counts.reasoning_tokens = number(usage, "/reasoning_output_tokens");
                    changes.push((
                        format!("turn:{}", self.turn),
                        UsageMeasurement {
                            counts,
                            observed_model: self.model.clone(),
                            partial: false,
                        },
                    ));
                }
            }
            CliBackend::Claude if kind == "result" => {
                replace = true;
                if let Some(models) = value
                    .get("modelUsage")
                    .and_then(Value::as_object)
                    .filter(|m| !m.is_empty())
                {
                    for (model, usage) in models {
                        let read = number(usage, "/cacheReadInputTokens");
                        let write = number(usage, "/cacheCreationInputTokens");
                        let counts = TokenCounts {
                            input_tokens: plus(plus(number(usage, "/inputTokens"), read), write),
                            output_tokens: number(usage, "/outputTokens"),
                            cache_read_tokens: read,
                            cache_write_tokens: write,
                            ..Default::default()
                        };
                        if !is_measured(&counts) {
                            continue;
                        }
                        changes.push((
                            format!("model:{model}"),
                            UsageMeasurement {
                                counts,
                                observed_model: Some(model.clone()),
                                partial: false,
                            },
                        ));
                    }
                } else if let Some(usage) = value.get("usage") {
                    let read = number(usage, "/cache_read_input_tokens");
                    let write = number(usage, "/cache_creation_input_tokens");
                    let mut counts = snake_counts(usage);
                    counts.input_tokens = plus(plus(counts.input_tokens, read), write);
                    counts.cache_read_tokens = read;
                    counts.cache_write_tokens = write;
                    // The fallback has no guarantee of subagent coverage.
                    changes.push((
                        "summary".into(),
                        UsageMeasurement {
                            counts,
                            observed_model: self.model.clone(),
                            partial: true,
                        },
                    ));
                }
            }
            CliBackend::Gemini if kind == "result" || kind == "stats" => {
                if let Some(stats) = value.get("stats").or_else(|| value.get("usage")) {
                    replace = true;
                    if let Some(models) = stats
                        .get("models")
                        .and_then(Value::as_object)
                        .filter(|m| !m.is_empty())
                    {
                        for (model, usage) in models {
                            let mut counts = snake_counts(usage);
                            counts.cache_read_tokens = number(usage, "/cached");
                            if !is_measured(&counts) {
                                continue;
                            }
                            changes.push((
                                format!("model:{model}"),
                                UsageMeasurement {
                                    counts,
                                    observed_model: Some(model.clone()),
                                    partial: false,
                                },
                            ));
                        }
                    } else {
                        let mut counts = snake_counts(stats);
                        counts.cache_read_tokens = number(stats, "/cached");
                        changes.push((
                            "summary".into(),
                            UsageMeasurement {
                                counts,
                                observed_model: self.model.clone(),
                                partial: true,
                            },
                        ));
                    }
                }
            }
            CliBackend::Opencode if kind == "step_finish" => {
                if let Some(tokens) = value.pointer("/part/tokens") {
                    let read = number(tokens, "/cache/read");
                    let write = number(tokens, "/cache/write");
                    let reasoning = number(tokens, "/reasoning");
                    let counts = TokenCounts {
                        input_tokens: plus(plus(number(tokens, "/input"), read), write),
                        output_tokens: plus(number(tokens, "/output"), reasoning),
                        total_tokens: number(tokens, "/total"),
                        cache_read_tokens: read,
                        cache_write_tokens: write,
                        reasoning_tokens: reasoning,
                    };
                    // A missing step id cannot be deduplicated safely. Keep one
                    // explicitly partial slot instead of manufacturing usage.
                    let id = value.pointer("/part/id").and_then(Value::as_str);
                    let model = value
                        .pointer("/part/modelID")
                        .or_else(|| value.get("modelID"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .or_else(|| self.model.clone());
                    changes.push((
                        format!("step:{}", id.unwrap_or("unknown")),
                        UsageMeasurement {
                            counts,
                            observed_model: model,
                            partial: id.is_none(),
                        },
                    ));
                }
            }
            // `agy` names frames with `event`, reports each finished step's usage
            // and then the run's sum on `result`, which replaces the steps.
            CliBackend::Antigravity => match value.get("event").and_then(Value::as_str) {
                Some("init") => {
                    self.model = value
                        .pointer("/init/model")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .or(self.model.take());
                }
                Some("step_update") => {
                    if let Some(usage) = value.pointer("/step_update/usage") {
                        let step = value
                            .pointer("/step_update/step_index")
                            .and_then(Value::as_u64);
                        changes.push((
                            format!("step:{}", step.map_or("unknown".into(), |n| n.to_string())),
                            UsageMeasurement {
                                counts: antigravity_counts(usage),
                                observed_model: self.model.clone(),
                                partial: step.is_none(),
                            },
                        ));
                    }
                }
                Some("result") => {
                    if let Some(usage) = value.pointer("/result/usage") {
                        replace = true;
                        changes.push((
                            "summary".into(),
                            UsageMeasurement {
                                counts: antigravity_counts(usage),
                                observed_model: self.model.clone(),
                                partial: false,
                            },
                        ));
                    }
                }
                _ => {}
            },
            _ => {}
        }
        for (_, measurement) in &mut changes {
            measurement.partial |= self.incomplete;
        }
        if replace && !changes.is_empty() {
            self.units.clear();
        }
        for (key, measurement) in &changes {
            self.units.insert(key.clone(), measurement.clone());
        }
        (replace, changes)
    }

    pub fn totals(&self) -> TokenCounts {
        let mut totals = TokenCounts::default();
        for measurement in self.units.values() {
            macro_rules! add { ($($field:ident),*) => { $(
                if let Some(value)=measurement.counts.$field {
                    totals.$field=totals.$field.unwrap_or(0).checked_add(value);
                }
            )* }; }
            add!(
                input_tokens,
                output_tokens,
                total_tokens,
                cache_read_tokens,
                cache_write_tokens,
                reasoning_tokens
            );
        }
        totals
    }

    /// Output tokens produced so far, whatever the backend has reported yet:
    /// Claude message by message, Codex and OpenCode per turn or step, Gemini
    /// only once it closes (so a Gemini run cannot be metered while it runs).
    pub fn output_tokens_so_far(&self) -> u64 {
        let reported = self
            .totals()
            .output_tokens
            .and_then(|n| u64::try_from(n).ok())
            .unwrap_or(0);
        reported.max(self.live.output_tokens())
    }
}

pub struct CliUsageRecorder {
    path: PathBuf,
    context: UsageContext,
    backend: CliBackend,
    tracker: Mutex<CliUsageTracker>,
}

impl CliUsageRecorder {
    pub fn new(path: &Path, context: UsageContext, backend: CliBackend) -> rusqlite::Result<Self> {
        record_usage(path, &context, "unavailable", &UsageMeasurement::default())?;
        Ok(Self {
            path: path.to_owned(),
            context,
            backend,
            tracker: Mutex::new(CliUsageTracker::default()),
        })
    }

    pub fn observe_line(&self, line: &[u8]) -> rusqlite::Result<()> {
        // Only framed and bounded stdout is accepted; stderr is never a source.
        if line.len() > 256 * 1024 {
            return self.mark_incomplete();
        }
        let Ok(value) =
            serde_json::from_str::<Value>(&sanitize_terminal_text_bounded(line, line.len()))
        else {
            if line
                .iter()
                .copied()
                .find(|byte| !byte.is_ascii_whitespace())
                == Some(b'{')
            {
                return self.mark_incomplete();
            }
            return Ok(());
        };
        let (replace, changes) = self
            .tracker
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .observe(self.backend, &value);
        if changes.is_empty() {
            return Ok(());
        }
        let db = super::connection(&self.path, false)?;
        let transaction = db.unchecked_transaction()?;
        transaction.execute(
            "DELETE FROM token_usage_records WHERE source_kind=?1 AND source_id=?2 AND (?3 OR unit_key='unavailable')",
            params![self.context.source_kind,self.context.source_id,replace])?;
        for (key, measurement) in changes {
            // Provider identifiers may contain arbitrary text. Persist only a
            // stable digest as the deduplication key, never the raw identifier.
            let key = format!("unit:{:x}", Sha256::digest(key.as_bytes()));
            record_usage_on(&transaction, &self.context, &key, &measurement)?;
        }
        transaction.commit()
    }

    pub fn totals(&self) -> TokenCounts {
        self.tracker
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .totals()
    }

    /// Output tokens the stream has shown so far; see
    /// [`CliUsageTracker::output_tokens_so_far`].
    pub fn output_tokens_so_far(&self) -> u64 {
        self.tracker
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .output_tokens_so_far()
    }

    /// Records what a Claude stream showed before the host stopped it. Claude
    /// reports usage only in its final `result`, so without this a run ended
    /// early (for spending past its output ceiling, for instance) would keep
    /// the empty `unavailable` placeholder and its cost would not exist in
    /// the ledger. Does nothing when `result` already arrived or the stream
    /// never opened a message.
    pub fn record_live_partial(&self) -> rusqlite::Result<()> {
        let measurement = {
            let tracker = self.tracker.lock().unwrap_or_else(|e| e.into_inner());
            if !tracker.units.is_empty() {
                return Ok(());
            }
            tracker.live.measurement(tracker.model.clone())
        };
        let Some(measurement) = measurement else {
            return Ok(());
        };
        let db = super::connection(&self.path, false)?;
        let transaction = db.unchecked_transaction()?;
        transaction.execute(
            "DELETE FROM token_usage_records WHERE source_kind=?1 AND source_id=?2 AND unit_key='unavailable'",
            params![self.context.source_kind, self.context.source_id],
        )?;
        let key = format!("unit:{:x}", Sha256::digest(b"live"));
        record_usage_on(&transaction, &self.context, &key, &measurement)?;
        transaction.commit()
    }

    fn mark_incomplete(&self) -> rusqlite::Result<()> {
        self.tracker
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .incomplete = true;
        self.mark_partial()
    }

    pub fn mark_partial(&self) -> rusqlite::Result<()> {
        super::connection(&self.path,false)?.execute(
            "UPDATE token_usage_records SET quality='partial' WHERE source_kind=?1 AND source_id=?2 AND quality='complete'",
            params![self.context.source_kind,self.context.source_id])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn claude_model_snapshot_replaces_global_and_repeated_snapshot() {
        let mut tracker = CliUsageTracker::default();
        tracker.observe(
            CliBackend::Claude,
            &json!({"type":"result","usage":{"input_tokens":2,"output_tokens":4}}),
        );
        let value = json!({"type":"result","usage":{"input_tokens":999,"output_tokens":999},"modelUsage":{
            "model-a":{"inputTokens":2,"outputTokens":4,"cacheReadInputTokens":100,"cacheCreationInputTokens":10},
            "model-b":{"inputTokens":3,"outputTokens":5}}});
        tracker.observe(CliBackend::Claude, &value);
        tracker.observe(CliBackend::Claude, &value);
        assert_eq!(tracker.units.len(), 2);
        assert_eq!(tracker.totals().input_tokens, Some(115));
        assert_eq!(tracker.totals().output_tokens, Some(9));
    }

    #[test]
    fn opencode_steps_are_idempotent_and_include_cache_and_reasoning_once() {
        let mut tracker = CliUsageTracker::default();
        let mut value = json!({"type":"step_finish","part":{"id":"one","tokens":{
            "input":2,"output":4,"reasoning":6,"cache":{"read":10,"write":3}}}});
        tracker.observe(CliBackend::Opencode, &value);
        tracker.observe(CliBackend::Opencode, &value);
        value["part"]["id"] = json!("two");
        tracker.observe(CliBackend::Opencode, &value);
        assert_eq!(tracker.units.len(), 2);
        assert_eq!(tracker.totals().input_tokens, Some(30));
        assert_eq!(tracker.totals().output_tokens, Some(20));
    }

    #[test]
    fn codex_turns_and_gemini_models_count_once_and_ignore_text() {
        let mut tracker = CliUsageTracker::default();
        tracker.observe(CliBackend::Codex, &json!({"type":"turn.started"}));
        let value = json!({"type":"turn.completed","usage":{"input_tokens":20,"cached_input_tokens":15,"output_tokens":3}});
        tracker.observe(CliBackend::Codex, &value);
        tracker.observe(CliBackend::Codex, &value);
        tracker.observe(CliBackend::Codex, &json!({"type":"turn.started"}));
        tracker.observe(CliBackend::Codex, &value);
        assert_eq!(tracker.totals().input_tokens, Some(40));
        let mut gemini = CliUsageTracker::default();
        gemini.observe(
            CliBackend::Gemini,
            &json!({"type":"message","content":{"stats":{"input_tokens":999}}}),
        );
        assert_eq!(gemini.totals().input_tokens, None);
        gemini.observe(
            CliBackend::Gemini,
            &json!({"type":"result","stats":{"input_tokens":999,"models":{
            "gemini-a":{"input_tokens":20,"output_tokens":5,"cached":10},
            "gemini-b":{"input_tokens":7,"output_tokens":2}}}}),
        );
        assert_eq!(gemini.totals().input_tokens, Some(27));
        assert_eq!(gemini.units.len(), 2);
    }

    fn message_start(id: &str, input: i64, cache_read: i64, cache_write: i64) -> Value {
        json!({"type":"stream_event","event":{"type":"message_start","message":{
            "id":id,"usage":{"input_tokens":input,"cache_read_input_tokens":cache_read,
                "cache_creation_input_tokens":cache_write,"output_tokens":1}}}})
    }

    fn message_delta(output: i64) -> Value {
        json!({"type":"stream_event","event":{"type":"message_delta","usage":{"output_tokens":output}}})
    }

    fn text_delta(chars: usize) -> Value {
        json!({"type":"stream_event","event":{"type":"content_block_delta",
            "delta":{"type":"text_delta","text":"x".repeat(chars)}}})
    }

    #[test]
    fn claude_output_is_known_message_by_message_before_the_result_arrives() {
        // The seven closing counts of "Haiku CLI teste 1" (2026-09-26), whose
        // `result` recorded the same 44,354.
        let mut tracker = CliUsageTracker::default();
        for (index, output) in [6_955, 393, 9_438, 9_284, 6_554, 6_619, 5_111]
            .into_iter()
            .enumerate()
        {
            tracker.observe(CliBackend::Claude, &message_start(&format!("m{index}"), 8, 100, 0));
            // The `assistant` frame repeats a stale one-to-four token figure.
            tracker.observe(
                CliBackend::Claude,
                &json!({"type":"assistant","message":{"id":format!("m{index}"),"usage":{"output_tokens":3}}}),
            );
            tracker.observe(CliBackend::Claude, &message_delta(output));
        }
        assert_eq!(tracker.output_tokens_so_far(), 44_354);
        assert!(tracker.units.is_empty(), "no result has been seen yet");
    }

    #[test]
    fn an_open_message_is_estimated_low_and_replaced_by_the_exact_count_on_close() {
        let mut tracker = CliUsageTracker::default();
        tracker.observe(CliBackend::Claude, &message_start("m1", 8, 0, 0));
        tracker.observe(CliBackend::Claude, &text_delta(4_000));
        // 4,000 characters at four per token; the start frame's own "1" is lower.
        assert_eq!(tracker.output_tokens_so_far(), 1_000);
        // The real count is higher than the estimate: the estimate never ends a run early.
        tracker.observe(CliBackend::Claude, &message_delta(1_400));
        assert_eq!(tracker.output_tokens_so_far(), 1_400);
        // ...and an exact count lower than the estimate wins too.
        tracker.observe(CliBackend::Claude, &message_start("m2", 8, 0, 0));
        tracker.observe(CliBackend::Claude, &text_delta(4_000));
        tracker.observe(CliBackend::Claude, &message_delta(300));
        assert_eq!(tracker.output_tokens_so_far(), 1_700);
    }

    #[test]
    fn thinking_and_tool_input_stream_count_like_text() {
        let mut tracker = CliUsageTracker::default();
        tracker.observe(CliBackend::Claude, &message_start("m1", 8, 0, 0));
        for delta in [
            json!({"thinking": "t".repeat(400)}),
            json!({"partial_json": "j".repeat(400)}),
            json!({"signature": "s".repeat(4_000)}),
        ] {
            tracker.observe(
                CliBackend::Claude,
                &json!({"type":"stream_event","event":{"type":"content_block_delta","delta":delta}}),
            );
        }
        assert_eq!(tracker.output_tokens_so_far(), 200);
    }

    #[test]
    fn other_backends_report_through_their_own_turn_counts() {
        let mut codex = CliUsageTracker::default();
        codex.observe(CliBackend::Codex, &json!({"type":"turn.started"}));
        codex.observe(
            CliBackend::Codex,
            &json!({"type":"turn.completed","usage":{"input_tokens":20,"output_tokens":3}}),
        );
        // Codex frames that look like Claude's are not read as Claude's.
        codex.observe(CliBackend::Codex, &message_start("m1", 8, 0, 0));
        codex.observe(CliBackend::Codex, &text_delta(4_000));
        assert_eq!(codex.output_tokens_so_far(), 3);
        // Gemini only reports at the end, so there is nothing to meter before then.
        let mut gemini = CliUsageTracker::default();
        gemini.observe(CliBackend::Gemini, &text_delta(4_000));
        assert_eq!(gemini.output_tokens_so_far(), 0);
    }

    #[test]
    fn the_live_measurement_counts_prompt_cache_and_output_as_a_floor() {
        let mut tracker = CliUsageTracker::default();
        assert!(tracker.live.measurement(None).is_none(), "nothing streamed yet");
        tracker.observe(CliBackend::Claude, &message_start("m1", 8, 1_000, 200));
        tracker.observe(CliBackend::Claude, &message_delta(500));
        tracker.observe(CliBackend::Claude, &message_start("m2", 8, 1_500, 0));
        let measurement = tracker.live.measurement(Some("haiku".into())).unwrap();
        assert_eq!(measurement.counts.input_tokens, Some(8 + 1_000 + 200 + 8 + 1_500));
        assert_eq!(measurement.counts.cache_read_tokens, Some(2_500));
        assert_eq!(measurement.counts.cache_write_tokens, Some(200));
        // m1 exact, m2 still at the start frame's own "1".
        assert_eq!(measurement.counts.output_tokens, Some(501));
        assert!(measurement.partial);
    }

    #[test]
    fn a_model_the_session_knew_but_never_ran_is_not_recorded() {
        // Gemini reports every model configured for the session. Persisting the
        // all-zero entries put a second, phantom model beside the real one on
        // every single call, so a run looked like it used two models.
        let mut gemini = CliUsageTracker::default();
        gemini.observe(
            CliBackend::Gemini,
            &json!({"type":"result","stats":{"models":{
            "gemini-pro-preview":{"input_tokens":0,"output_tokens":0,"cached":0},
            "gemini-flash-preview":{"input_tokens":17180,"output_tokens":923,"cached":0}}}}),
        );
        assert_eq!(gemini.units.len(), 1);
        assert!(gemini.units.contains_key("model:gemini-flash-preview"));
        assert_eq!(gemini.totals().input_tokens, Some(17180));

        let mut claude = CliUsageTracker::default();
        claude.observe(
            CliBackend::Claude,
            &json!({"type":"result","modelUsage":{
            "claude-idle":{"inputTokens":0,"outputTokens":0},
            "claude-sonnet-5":{"inputTokens":22784,"outputTokens":15349}}}),
        );
        assert_eq!(claude.units.len(), 1);
        assert!(claude.units.contains_key("model:claude-sonnet-5"));
    }

    #[test]
    fn antigravity_steps_meter_the_run_and_the_result_replaces_them() {
        // Figures from a real `agy` 1.3.5 resume (2026-10-14): each finished
        // step reports its usage, and `result` reports the sum once more.
        let mut tracker = CliUsageTracker::default();
        tracker.observe(
            CliBackend::Antigravity,
            &json!({"event":"init","init":{"model":"gemini-3.8-flash-low"}}),
        );
        let step = json!({"event":"step_update","step_update":{"step_index":4,"state":"DONE",
            "step_type":"agent_response","usage":{"input_tokens":2317,"output_tokens":41,
            "thinking_tokens":39,"cache_read_tokens":14983,"total_tokens":2358}}});
        tracker.observe(CliBackend::Antigravity, &step);
        tracker.observe(CliBackend::Antigravity, &step);
        assert_eq!(tracker.output_tokens_so_far(), 41);

        let (replace, changes) = tracker.observe(
            CliBackend::Antigravity,
            &json!({"event":"result","result":{"status":"SUCCESS","usage":{"input_tokens":4379,
            "output_tokens":84,"thinking_tokens":80,"cache_read_tokens":29966,"total_tokens":4463}}}),
        );
        assert!(replace);
        assert_eq!(
            changes[0].1.observed_model.as_deref(),
            Some("gemini-3.8-flash-low")
        );
        assert_eq!(tracker.units.len(), 1);
        let totals = tracker.totals();
        assert_eq!(totals.input_tokens, Some(4379 + 29966));
        assert_eq!(totals.cache_read_tokens, Some(29966));
        assert_eq!(totals.output_tokens, Some(84));
        assert_eq!(totals.reasoning_tokens, Some(80));
    }
}
