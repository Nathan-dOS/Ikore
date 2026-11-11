//! Admission uses the same manifest projection delivered to stage agents.
//! Frozen storage still retains the complete feature and its backlog drafts.

use serde_json::Value;

use super::BatchManifest;
use crate::orchestration::{PipelineVersion, composition::budget::prompt_character_limit};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BatchContextBudget {
    pub required_chars: usize,
    pub limit_chars: usize,
}

impl BatchContextBudget {
    pub fn fits(self) -> bool {
        self.required_chars <= self.limit_chars
    }
}

/// Only the selected, identified tasks are executable. Feature-level drafts
/// duplicate those tasks and include unselected backlog, so the stage prompt
/// omits them (ADR-018). Every other approved field remains complete.
pub(crate) fn batch_prompt_projection(
    manifest: &BatchManifest,
) -> Result<Value, serde_json::Error> {
    let mut value = serde_json::to_value(manifest)?;
    if let Some(features) = value.get_mut("features").and_then(Value::as_array_mut) {
        for feature in features {
            if let Some(tasks) = feature
                .get_mut("brief")
                .and_then(|brief| brief.get_mut("tasks"))
            {
                *tasks = Value::Array(Vec::new());
            }
        }
    }
    Ok(value)
}

/// Reserve half of the applicable prompt capacity for instructions, other
/// handoff data and repository context. V3 retains its existing global-limit
/// reserve; V4 uses the composed-stage capacity (SC-013).
pub(crate) fn assess_batch_context(
    manifest: &BatchManifest,
    pipeline_version: PipelineVersion,
    context_chars_limit: usize,
) -> Result<BatchContextBudget, serde_json::Error> {
    let projection = serde_json::to_string(&batch_prompt_projection(manifest)?)?;
    let capacity = if pipeline_version.supports_composition() {
        prompt_character_limit(context_chars_limit) as usize
    } else {
        context_chars_limit
    };
    Ok(BatchContextBudget {
        required_chars: projection.chars().count(),
        limit_chars: capacity / 2,
    })
}

#[cfg(test)]
#[path = "batch_budget_tests.rs"]
mod tests;
