//! Product planning and task backlogs. Only the pulsation host integrates code.
pub mod pipeline;
mod publication;
mod runtime;
pub(crate) mod service;

use crate::orchestration::SourceRevision;
pub use runtime::*;
use serde::{Deserialize, Serialize};
pub use service::*;

pub const BATCH_PIPELINE_VERSION: u8 = 3;

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeatureDependency {
    pub feature_id: String,
    pub prerequisite_id: String,
    pub rationale: String,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatePulsationFromTasksRequest {
    pub project_id: String,
    pub request_id: String,
    pub title: String,
    pub tasks: Vec<TaskSelection>,
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
