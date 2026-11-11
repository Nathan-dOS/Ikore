//! Normalization only; legacy `LlmUsage` remains unchanged for cost calculation.
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};

use super::{TokenCounts, UsageContext, UsageMeasurement, record_usage};
use crate::llm::domain::ProviderProtocol;
use serde_json::Value;

fn number(value: &Value, pointer: &str) -> Option<i64> {
    value
        .pointer(pointer)
        .and_then(Value::as_i64)
        .filter(|n| *n >= 0)
}

pub fn parse_usage(protocol: ProviderProtocol, value: &Value) -> UsageMeasurement {
    let value = value
        .get("response")
        .or_else(|| value.get("message"))
        .unwrap_or(value);
    let observed_model = value
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let Some(usage) = value.get("usage").filter(|v| v.is_object()) else {
        return UsageMeasurement {
            observed_model,
            ..Default::default()
        };
    };
    let mut counts = TokenCounts::default();
    match protocol {
        ProviderProtocol::OpenaiResponses => {
            counts.input_tokens = number(usage, "/input_tokens");
            counts.output_tokens = number(usage, "/output_tokens");
            counts.total_tokens = number(usage, "/total_tokens");
            counts.cache_read_tokens = number(usage, "/input_tokens_details/cached_tokens");
            counts.reasoning_tokens = number(usage, "/output_tokens_details/reasoning_tokens");
        }
        ProviderProtocol::OpenaiChat => {
            counts.input_tokens = number(usage, "/prompt_tokens");
            counts.output_tokens = number(usage, "/completion_tokens");
            counts.total_tokens = number(usage, "/total_tokens");
            counts.cache_read_tokens = number(usage, "/prompt_tokens_details/cached_tokens")
                .or_else(|| number(usage, "/prompt_cache_hit_tokens"));
            counts.reasoning_tokens = number(usage, "/completion_tokens_details/reasoning_tokens");
        }
        ProviderProtocol::AnthropicMessages => {
            counts.cache_read_tokens = number(usage, "/cache_read_input_tokens");
            counts.cache_write_tokens = number(usage, "/cache_creation_input_tokens");
            counts.input_tokens = number(usage, "/input_tokens").and_then(|input| {
                input
                    .checked_add(counts.cache_read_tokens.unwrap_or(0))?
                    .checked_add(counts.cache_write_tokens.unwrap_or(0))
            });
            counts.output_tokens = number(usage, "/output_tokens");
        }
    }
    // Missing optional breakdowns are not automatically zero in storage.
    UsageMeasurement {
        counts,
        observed_model,
        partial: false,
    }
}

pub fn merge_usage(target: &mut UsageMeasurement, update: &UsageMeasurement) {
    if update.observed_model.is_some() {
        target.observed_model.clone_from(&update.observed_model);
    }
    macro_rules! merge { ($($field:ident),*) => { $(
        if update.counts.$field.is_some() { target.counts.$field=update.counts.$field; }
    )* }; }
    merge!(
        input_tokens,
        output_tokens,
        total_tokens,
        cache_read_tokens,
        cache_write_tokens,
        reasoning_tokens
    );
    target.partial |= update.partial;
}

/// Retains the last reported snapshot if a future is dropped by cancellation.
/// No provider content is held here. Normal exits propagate persistence errors.
pub struct UsageRecorder {
    path: PathBuf,
    context: UsageContext,
    measurement: Mutex<UsageMeasurement>,
    persisted: bool,
}

impl UsageRecorder {
    pub fn new(path: &Path, context: UsageContext) -> Self {
        Self {
            path: path.to_owned(),
            context,
            measurement: Mutex::new(UsageMeasurement::default()),
            persisted: false,
        }
    }
    pub fn observe(&self, measurement: &UsageMeasurement) {
        merge_usage(
            &mut self.measurement.lock().unwrap_or_else(|e| e.into_inner()),
            measurement,
        );
    }
    pub fn snapshot(&self) -> UsageMeasurement {
        self.measurement
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub fn finish(mut self, complete: bool) -> rusqlite::Result<()> {
        let mut measurement = self.snapshot();
        measurement.partial |= !complete;
        let result = record_usage(&self.path, &self.context, "response", &measurement);
        self.persisted = result.is_ok();
        result
    }
}

impl Drop for UsageRecorder {
    fn drop(&mut self) {
        if !self.persisted {
            let mut measurement = self.snapshot();
            measurement.partial = true;
            if let Err(error) = record_usage(&self.path, &self.context, "response", &measurement) {
                eprintln!("Unable to retain interrupted token usage: {error}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cache_is_normalized_once_for_each_api_protocol() {
        let anthropic = parse_usage(
            ProviderProtocol::AnthropicMessages,
            &json!({"usage":{
            "input_tokens":3,"output_tokens":5,"cache_read_input_tokens":100,"cache_creation_input_tokens":10}}),
        );
        assert_eq!(anthropic.counts.input_tokens, Some(113));
        let openai = parse_usage(
            ProviderProtocol::OpenaiResponses,
            &json!({"model":"model-a","usage":{
            "input_tokens":113,"output_tokens":5,"input_tokens_details":{"cached_tokens":100},
            "output_tokens_details":{"reasoning_tokens":2}}}),
        );
        assert_eq!(openai.counts.input_tokens, Some(113));
        assert_eq!(openai.counts.output_tokens, Some(5));
        assert_eq!(openai.counts.reasoning_tokens, Some(2));
        assert_eq!(openai.observed_model.as_deref(), Some("model-a"));
    }

    #[test]
    fn anthropic_stream_merges_input_then_final_output_without_summing_snapshots() {
        let mut measurement = parse_usage(
            ProviderProtocol::AnthropicMessages,
            &json!({"message":{
            "model":"claude-test","usage":{"input_tokens":3,"output_tokens":0,"cache_read_input_tokens":100}}}),
        );
        let update = parse_usage(
            ProviderProtocol::AnthropicMessages,
            &json!({"usage":{"output_tokens":8}}),
        );
        merge_usage(&mut measurement, &update);
        merge_usage(&mut measurement, &update);
        assert_eq!(measurement.counts.input_tokens, Some(103));
        assert_eq!(measurement.counts.output_tokens, Some(8));
        assert_eq!(
            parse_usage(ProviderProtocol::OpenaiChat, &json!({})).counts,
            TokenCounts::default()
        );
    }
}
