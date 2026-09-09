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
fn snake_counts(value: &Value) -> TokenCounts {
    TokenCounts {
        input_tokens: number(value, "/input_tokens"),
        output_tokens: number(value, "/output_tokens"),
        total_tokens: number(value, "/total_tokens"),
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
}
