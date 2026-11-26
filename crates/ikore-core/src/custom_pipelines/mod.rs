//! Feature 25: the developer's own pipelines around the Ikore pipeline.
//!
//! A pipeline is versioned (`custom_pipeline_versions`), and a project shows
//! it as a box through its activation (`custom_pipeline_activations`, T3). A
//! run follows the definition it resolved when it started (T4) and moves one
//! step at a time on the automation lane, which admits steps through
//! `automation_slots` and never through the main lane's slot (E3, T12).
//! Agents only read (E1, E2): a step returns text or a verdict, and the host
//! owns state, identifiers and decisions.
//!
//! The commands live on `PersistenceService` (`store.rs`), the lane next to
//! the supervisor's other two (`engine.rs`), the events of the Ikore pipeline
//! that start pipelines in `triggers.rs` (M3), and the model input here
//! (`prompt.rs`).

pub(crate) mod prompt;

use std::ops::RangeInclusive;

use serde::{Deserialize, Serialize};

pub use ikore_contracts::custom_pipeline::*;

/// A project's own pipelines that are not archived (R4).
pub const MAX_PROJECT_PIPELINES: usize = 5;
/// Global pipelines that are not archived (R4a).
pub const MAX_GLOBAL_PIPELINES: usize = 5;
/// Boxes a project shows besides the Ikore pipeline's (R4, T9).
pub const MAX_ACTIVE_PIPELINES: usize = 7;
/// `runtime_settings` key of the automation lane's global ceiling.
pub const AUTOMATION_MAX_CONCURRENT_KEY: &str = "automation.max_concurrent";
pub const AUTOMATION_MAX_CONCURRENT_DEFAULT: u32 = 2;
pub const AUTOMATION_MAX_CONCURRENT_RANGE: RangeInclusive<u32> = 1..=3;
/// What the developer may add when deciding a gate.
pub const MAX_GATE_NOTE_BYTES: usize = 2 * 1024;
/// Runs the snapshot carries per pipeline, newest first.
pub const RECENT_RUNS_PER_PIPELINE: usize = 10;

/// The terminal slot of a pipeline step (T5): one per project, pipeline and
/// step, so a step's history stays its own.
pub fn automation_slot_id(project_id: &str, pipeline_id: &str, step_id: &str) -> String {
    format!("automation:{project_id}:{pipeline_id}:{step_id}")
}

/// The label of a step's slot and dock tab: `{pipeline} · {step}`.
pub fn automation_slot_label(pipeline: &str, step: &str) -> String {
    format!("{pipeline} \u{b7} {step}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelineRunStatus {
    Queued,
    Running,
    WaitingGate,
    Done,
    Failed,
    Cancelled,
    Interrupted,
}

impl PipelineRunStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::WaitingGate => "waiting_gate",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "waiting_gate" => Self::WaitingGate,
            "done" => Self::Done,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            "interrupted" => Self::Interrupted,
            _ => return None,
        })
    }

    /// Queued, running or waiting at a gate: `Cancel` applies.
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::WaitingGate)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepRunStatus {
    Queued,
    Running,
    WaitingGate,
    Passed,
    Rejected,
    Done,
    Failed,
    Cancelled,
    Interrupted,
}

impl StepRunStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::WaitingGate => "waiting_gate",
            Self::Passed => "passed",
            Self::Rejected => "rejected",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "waiting_gate" => Self::WaitingGate,
            "passed" => Self::Passed,
            "rejected" => Self::Rejected,
            "done" => Self::Done,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            "interrupted" => Self::Interrupted,
            _ => return None,
        })
    }
}

/// A pipeline as a project shows it: the version the project adopted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomPipelineView {
    pub id: String,
    /// `project` or `global`.
    pub scope: String,
    pub project_id: Option<String>,
    pub name: String,
    pub description: String,
    pub current_version: u32,
    pub adopted_version: u32,
    pub position: u32,
    /// `false` when the box is `Needs edit`: what it adopted no longer fits
    /// (P4) or its trigger needs this project's choice.
    pub ready: bool,
    pub needs_edit_reason: Option<String>,
    pub archived: bool,
    pub definition: CustomPipelineDefinitionV1,
    /// Runs of this pipeline in the project; one is enough to keep it.
    pub run_count: u32,
    pub updated_at: String,
    /// The pipeline this one is based on (P1).
    #[serde(default)]
    pub parent_pipeline_id: Option<String>,
    #[serde(default)]
    pub parent_name: Option<String>,
    /// The parent's version this project's box resolves against.
    #[serde(default)]
    pub adopted_parent_version: Option<u32>,
    /// Pipelines based on this one, in any project.
    #[serde(default)]
    pub children: u32,
    /// Projects that show this pipeline.
    #[serde(default)]
    pub projects: u32,
    /// This project's own trigger for the pipeline (R4b, M3); the
    /// definition's when `None`.
    #[serde(default)]
    pub trigger_override: Option<PipelineTrigger>,
}

impl CustomPipelineView {
    pub fn is_global(&self) -> bool {
        self.scope == "global"
    }

    /// A newer version of the pipeline or of its parent waits for this
    /// project to open again (P2).
    pub fn behind(&self) -> bool {
        self.adopted_version < self.current_version
    }

    /// The trigger this project's box follows.
    pub fn effective_trigger(&self) -> &PipelineTrigger {
        self.trigger_override
            .as_ref()
            .unwrap_or(&self.definition.trigger)
    }
}

/// A global pipeline this project does not show yet: `Add global pipeline…`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalPipelineSummary {
    pub id: String,
    pub name: String,
    pub description: String,
    pub current_version: u32,
    pub parent_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineRunView {
    pub id: String,
    pub pipeline_id: String,
    pub version: u32,
    pub trigger_kind: String,
    pub status: PipelineRunStatus,
    pub current_step: u32,
    pub error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub finished_at: Option<String>,
    /// The definition the run follows (T4).
    pub definition: CustomPipelineDefinitionV1,
    /// Every attempt of every step that started, in order.
    pub steps: Vec<PipelineStepRunView>,
    /// The pulsation whose event started the run (M3).
    #[serde(default)]
    pub source_pulsation_id: Option<String>,
    #[serde(default)]
    pub source_pulsation_title: Option<String>,
    /// The event that started the run, as [`trigger_label`] reads it.
    #[serde(default)]
    pub trigger_label: Option<String>,
    /// The source pulsation failed after the event ("Source pulsation
    /// failed").
    #[serde(default)]
    pub source_pulsation_failed: bool,
}

impl PipelineRunView {
    /// The latest attempt of a step.
    pub fn step(&self, index: u32) -> Option<&PipelineStepRunView> {
        self.steps
            .iter()
            .filter(|step| step.step_index == index)
            .max_by_key(|step| step.attempt)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineStepRunView {
    pub step_index: u32,
    pub attempt: u32,
    pub step_id: String,
    pub kind: String,
    pub status: StepRunStatus,
    pub terminal_run_id: Option<String>,
    pub llm_execution_id: Option<String>,
    #[serde(default)]
    pub git_operation_id: Option<String>,
    pub output_text: Option<String>,
    pub verdict: Option<StageVerdictV1>,
    pub gate_decision: Option<String>,
    pub gate_feedback: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub error: Option<String>,
}

/// What the Agents canvas shows of a project's pipelines.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomPipelinesSnapshot {
    pub project_id: String,
    /// The boxes, in canvas order.
    pub pipelines: Vec<CustomPipelineView>,
    pub archived: Vec<CustomPipelineView>,
    /// Newest first, at most [`RECENT_RUNS_PER_PIPELINE`] per pipeline.
    pub runs: Vec<PipelineRunView>,
    /// The automation lane's global ceiling.
    pub max_concurrent: u32,
    /// Global pipelines without a box here.
    #[serde(default)]
    pub available_globals: Vec<GlobalPipelineSummary>,
}

impl CustomPipelinesSnapshot {
    pub fn runs_of<'a>(
        &'a self,
        pipeline_id: &'a str,
    ) -> impl Iterator<Item = &'a PipelineRunView> {
        self.runs
            .iter()
            .filter(move |run| run.pipeline_id == pipeline_id)
    }
}

/// A stage's name in a trigger (the database spells Specification in
/// capitals).
pub fn trigger_stage_label(stage: &str) -> &str {
    match stage {
        "SPECIFICATION" => "Specification",
        other => other,
    }
}

/// How a trigger reads on a box, a run and the editor.
pub fn trigger_label(trigger: &PipelineTrigger) -> String {
    match trigger {
        PipelineTrigger::Manual => "Manual".into(),
        PipelineTrigger::DefaultEvent {
            event: DefaultEvent::PulsationDone,
            ..
        } => "Pulsation done".into(),
        PipelineTrigger::DefaultEvent {
            event: DefaultEvent::StagePassed,
            stage,
        } => format!(
            "Stage passed \u{b7} {}",
            trigger_stage_label(stage.as_deref().unwrap_or_default())
        ),
    }
}

/// Changes or clears this project's own trigger for a pipeline (R4b, M3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetPipelineTriggerRequest {
    pub project_id: String,
    pub pipeline_id: String,
    /// `None` goes back to the pipeline's own trigger.
    pub trigger: Option<PipelineTrigger>,
}

/// Creates a pipeline (`pipeline_id` empty) or saves a new version of one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveCustomPipelineRequest {
    pub project_id: String,
    pub pipeline_id: Option<String>,
    /// The version the form was opened on; a newer one refuses the save.
    pub expected_version: Option<u32>,
    pub name: String,
    pub description: String,
    pub definition: CustomPipelineDefinitionV1,
    /// On create: `project` (default) or `global`.
    #[serde(default)]
    pub scope: Option<String>,
    /// On create: the pipeline the new one is based on; the definition is
    /// then stored as its changes over the parent (P1).
    #[serde(default)]
    pub parent_pipeline_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "outcome")]
pub enum SaveCustomPipelineOutcome {
    Saved {
        pipeline: Box<CustomPipelineView>,
    },
    /// Every problem at once, for the form.
    Invalid {
        errors: Vec<PipelineValidationError>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecidePipelineGateRequest {
    pub project_id: String,
    pub run_id: String,
    pub step_index: u32,
    pub approve: bool,
    pub note: Option<String>,
}
