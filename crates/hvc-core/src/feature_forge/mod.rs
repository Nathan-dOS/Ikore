//! Product planning and task backlogs. Only the pulsation host integrates code.
mod failure;
pub mod pipeline;
mod publication;
mod runtime;
pub(crate) mod service;

use crate::orchestration::SourceRevision;
pub use failure::*;
pub use runtime::*;
use serde::{Deserialize, Serialize};
pub use service::*;

pub const BATCH_PIPELINE_VERSION: u8 = crate::orchestration::PipelineVersion::BATCH.as_u8();

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    Bug,
    Adjustment,
    Improvement,
    Refactor,
    Feature,
    Foundation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeatureState {
    Incubating,
    InTriage,
    Approved,
    Planned,
    Executing,
    ReadyForReview,
    Done,
    Archived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Wave {
    Current,
    Next,
    Horizon,
}

/// How a batch satisfied the dispatch-time file-path anchoring gate (ADR-022
/// clarification loop). Read downstream by the preventive HITL gate (P5),
/// which skips its own check when the forge gate already confirmed paths.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathConfirmationSource {
    /// The dispatch gate found an explicit path on at least one task.
    ForgeGate,
    /// The developer confirmed paths through a preventive HITL checkpoint.
    ManualHitl,
    /// No path was confirmed; the batch qualified for the greenfield
    /// exception (feature coverage `unavailable`, or an empty workspace).
    #[default]
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AcceptanceCriterion {
    pub id: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskDraft {
    pub title: String,
    pub kind: TaskKind,
    pub description: String,
    pub criteria: Vec<AcceptanceCriterion>,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub feature_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkTask {
    pub id: String,
    pub project_id: String,
    pub revision: u64,
    pub draft: TaskDraft,
    pub feature_revision: Option<u64>,
    pub state: String,
    pub active_pulsation_id: Option<String>,
    #[serde(default)]
    pub completed_pulsation_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FeatureBrief {
    pub objective: String,
    pub audience: String,
    pub behavior: String,
    pub scope: String,
    pub exclusions: String,
    pub alternatives: Vec<String>,
    pub decisions: Vec<String>,
    pub pending_decisions: Vec<String>,
    pub criteria: Vec<AcceptanceCriterion>,
    pub tasks: Vec<TaskDraft>,
    pub sources: Vec<SourceRevision>,
    pub coverage: String,
    pub foundation: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FeatureEvaluation {
    pub value: String,
    pub alignment: String,
    pub feasibility: String,
    pub usability: String,
    pub impact: Option<u8>,
    pub confidence: Option<u8>,
    pub effort: Option<u8>,
}
impl FeatureEvaluation {
    pub fn score(&self) -> Option<f64> {
        Some(f64::from(self.impact?) * f64::from(self.confidence?) / f64::from(self.effort?.max(1)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Feature {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub original_idea: String,
    pub revision: u64,
    pub approved_revision: Option<u64>,
    pub state: FeatureState,
    pub brief: FeatureBrief,
    pub evaluation: FeatureEvaluation,
    pub structural_level: u8,
    pub wave: Option<Wave>,
    pub position: i64,
    pub archive_reason: Option<String>,
}

/// Who authored a dependency edge. Agent-authored edges can be reviewed or
/// removed in bulk without disturbing relations a developer confirmed by hand.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyOrigin {
    #[default]
    Human,
    Agent,
}

impl DependencyOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Agent => "agent",
        }
    }

    /// Reads the stored column. Infallible on purpose: the table's CHECK
    /// constraint admits only these two values, and a row that somehow held
    /// anything else is safer treated as human-authored -- that is the value
    /// the agent pass refuses to overwrite.
    pub(crate) fn from_db(value: &str) -> Self {
        match value {
            "agent" => Self::Agent,
            _ => Self::Human,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeatureDependency {
    pub feature_id: String,
    pub prerequisite_id: String,
    pub rationale: String,
    /// Host-owned. The FeatureAgent schema for `suggestedDependencies` does not
    /// carry this field, so `default` is what keeps a model response
    /// deserializable; the host stamps the real origin when it stores the edge.
    #[serde(default)]
    pub origin: DependencyOrigin,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForgeSnapshot {
    pub features: Vec<Feature>,
    pub tasks: Vec<WorkTask>,
    pub dependencies: Vec<FeatureDependency>,
    pub current_wave_limit: u32,
    pub navigation: ForgeNavigation,
    pub integration_evidence: Vec<TaskEvidence>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForgeNavigation {
    pub selected_feature_id: Option<String>,
    pub section: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTasksRequest {
    pub project_id: String,
    pub request_id: String,
    pub tasks: Vec<TaskDraft>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateFeatureRequest {
    pub project_id: String,
    pub request_id: String,
    pub title: String,
    pub idea: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskSelection {
    pub task_id: String,
    pub expected_revision: u64,
    /// A developer-supplied path that satisfies the dispatch-time anchoring
    /// gate (ADR-022 clarification loop, P1/A1.3) for this task without
    /// persisting it to the task's stored draft. This is the mechanism for
    /// correcting a feature-owned task at dispatch time: `WorkTaskService::revise`
    /// refuses tasks with a `feature_id`, so a transient hint on the selection
    /// is the only way to satisfy the gate short of reopening Incubation.
    #[serde(default)]
    pub manual_path_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatePulsationFromTasksRequest {
    pub project_id: String,
    pub request_id: String,
    pub title: String,
    pub tasks: Vec<TaskSelection>,
}

/// Preview of the dispatch-time file-path anchoring gate (ADR-022
/// clarification loop, P1/A1.3) for a candidate task selection, computed by
/// the same check `create_pulsation_from_tasks` runs so the two can never
/// diverge.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchGateStatus {
    pub blocked: bool,
    /// Whether the batch qualified for the greenfield exception (a task's
    /// feature has no observed sources, or its workspace has no code files).
    pub greenfield: bool,
    pub tasks_missing_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchManifest {
    pub schema_version: u8,
    pub title: String,
    pub tasks: Vec<WorkTask>,
    pub features: Vec<Feature>,
    #[serde(default)]
    pub feature_dependencies: Vec<FeatureDependency>,
    #[serde(default)]
    pub path_confirmation_source: PathConfirmationSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskEvidence {
    pub task_id: String,
    pub criterion_ids: Vec<String>,
    pub artifact_positions: Vec<usize>,
    pub summary: String,
}

#[derive(Debug, Clone)]
pub struct ForgeEvent {
    pub project_id: String,
    pub feature_id: Option<String>,
}
