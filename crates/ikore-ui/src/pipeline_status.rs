//! "Where is the active pulsation right now" — shared by the flow-bar status pill
//! (`shell.rs`, visible in every view) and the Agents header stepper (`agents_view.rs`).

use gpui::Rgba;
use ikore_core::llm::AgentRole;
use ikore_core::persistence_service::{Pulsation, PulsationStage, PulsationStatus};
use ikore_core::terminals::{TerminalRunStatus, TerminalsSnapshot, canonical_agent};

use crate::agents_view::run_status;
use crate::pulsations::{PIPELINE_STAGES, stage_label, status_label};
use crate::theme;

/// Coarse colour class of a run or pulsation state; the dot in the flow-bar pill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StatusTone {
    Active,
    Attention,
    Failure,
    Success,
    Idle,
}

impl StatusTone {
    pub(crate) fn for_run(status: TerminalRunStatus) -> Self {
        match status {
            TerminalRunStatus::Queued
            | TerminalRunStatus::Starting
            | TerminalRunStatus::Running => Self::Active,
            TerminalRunStatus::AwaitingFallback => Self::Attention,
            TerminalRunStatus::Failed
            | TerminalRunStatus::Cancelled
            | TerminalRunStatus::Interrupted => Self::Failure,
            TerminalRunStatus::Completed => Self::Success,
        }
    }

    pub(crate) fn for_pulsation(status: PulsationStatus) -> Self {
        match status {
            PulsationStatus::Running => Self::Active,
            PulsationStatus::Checkpoint => Self::Attention,
            PulsationStatus::Failed => Self::Failure,
            PulsationStatus::Done => Self::Success,
            PulsationStatus::Idle | PulsationStatus::Planning => Self::Idle,
        }
    }

    pub(crate) fn color(self) -> Rgba {
        match self {
            Self::Active => theme::running(),
            Self::Attention => theme::warning(),
            Self::Failure => theme::danger(),
            Self::Success => theme::success(),
            Self::Idle => theme::text_muted(),
        }
    }
}

/// Snapshot of the active pulsation's position in the pipeline. Everything is `Copy`/`'static`
/// so callers can drop their entity borrows before building elements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PipelineStatus {
    pub stage: PulsationStage,
    pub stage_index: usize,
    pub pulsation_status: PulsationStatus,
    pub agent_name: &'static str,
    pub state: &'static str,
    pub tone: StatusTone,
}

impl PipelineStatus {
    pub(crate) fn derive(active: &Pulsation, snapshot: Option<&TerminalsSnapshot>) -> Self {
        let stage = active.stage;
        let stage_index = PIPELINE_STAGES
            .iter()
            .position(|candidate| *candidate == stage)
            .unwrap_or_default();
        let role = AgentRole::for_stage(stage_label(stage));
        // A settled pulsation speaks for itself: its last run may have completed before the
        // pulsation was cancelled or failed, and "Completed" on a failed pulsation misleads.
        let settled = matches!(
            active.status,
            PulsationStatus::Done | PulsationStatus::Failed | PulsationStatus::Checkpoint
        );
        // Prefer the run still in flight for this pulsation; otherwise the latest one for it.
        let run = snapshot.filter(|_| !settled).and_then(|snapshot| {
            let agent = snapshot
                .agents
                .iter()
                .find(|agent| agent.config.agent_role == role)?;
            let mine = |run: &&ikore_core::terminals::TerminalRun| {
                run.pulsation_id.as_deref() == Some(active.id.as_str())
            };
            agent
                .recent_runs
                .iter()
                .find(|run| mine(run) && run.status.is_active())
                .or_else(|| agent.recent_runs.iter().find(mine))
                .map(|run| run.status)
        });
        let (state, tone) = match run {
            Some(status) => (run_status(status), StatusTone::for_run(status)),
            None => (
                status_label(active.status, active.terminal_reason.as_deref()),
                StatusTone::for_pulsation(active.status),
            ),
        };
        Self {
            stage,
            stage_index,
            pulsation_status: active.status,
            agent_name: canonical_agent(role).name,
            state,
            tone,
        }
    }

    pub(crate) fn stage_complete(&self, index: usize) -> bool {
        index < self.stage_index
            || (self.pulsation_status == PulsationStatus::Done && index == self.stage_index)
    }

    /// Fill of the current step's circle in the stepper.
    pub(crate) fn current_fill(&self) -> Rgba {
        match self.pulsation_status {
            PulsationStatus::Failed => theme::danger(),
            PulsationStatus::Checkpoint => theme::warning(),
            PulsationStatus::Done => theme::success(),
            _ => theme::running(),
        }
    }
}

/// The header's centre readout: who is executing right now, or — when no runtime slot is held —
/// the selected pulsation's settled state.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NowRunning {
    pub dot: Rgba,
    pub text: String,
    /// `true` while some job holds a runtime slot; the header tints its pill only then.
    pub busy: bool,
    /// The other slot's work when both a stage and a background job run.
    pub also: Option<String>,
}

impl NowRunning {
    pub(crate) fn derive(
        pulsations: &crate::pulsations::PulsationsModel,
        snapshot: Option<&TerminalsSnapshot>,
        now: chrono::DateTime<chrono::Local>,
    ) -> Self {
        let mut jobs = pulsations.active_jobs.iter();
        if let Some(job) = jobs.next() {
            let feature = job
                .feature_name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty());
            let stage_run = job
                .kind
                .strip_prefix("run_stage::")
                .and(job.pulsation_id.as_deref())
                .and_then(|id| pulsations.pulsations.iter().find(|p| p.id == id));
            let mut text = match stage_run {
                Some(pulsation) => {
                    let status = PipelineStatus::derive(pulsation, snapshot);
                    format!(
                        "{} · {} · {}",
                        step_label(status.stage),
                        status.agent_name,
                        status.state
                    )
                }
                None => {
                    let (actor, activity) = job_activity(&job.kind);
                    format!("{activity} · {actor}")
                }
            };
            if let Some(feature) = feature {
                text = format!("{feature} · {text}");
            }
            if let Some(elapsed) = elapsed_since(&job.acquired_at, now) {
                text = format!("{text} · {elapsed}");
            }
            return Self {
                dot: theme::running(),
                text,
                busy: true,
                also: jobs
                    .next()
                    .map(|other| job_activity(&other.kind).1.to_string()),
            };
        }
        match pulsations.active() {
            Some(active) => {
                let status = PipelineStatus::derive(active, snapshot);
                let feature = active.feature_name.trim();
                let settled = format!("{} · {}", step_label(status.stage), status.state);
                Self {
                    dot: status.tone.color(),
                    text: if feature.is_empty() {
                        format!("Idle · last: {settled}")
                    } else {
                        format!("Idle · last: {feature} · {settled}")
                    },
                    busy: false,
                    also: None,
                }
            }
            None => Self {
                dot: theme::text_muted(),
                text: "Idle · no pulsation yet".into(),
                busy: false,
                also: None,
            },
        }
    }
}

/// `(actor, activity)` for a held slot's `kind`: a `jobs.kind`, or a slot token prefix for work
/// without a job row (see `ActiveRuntimeJob`).
pub(crate) fn job_activity(kind: &str) -> (&'static str, &'static str) {
    if let Some(stage) = kind.strip_prefix("run_stage::")
        && let Some(stage) = PIPELINE_STAGES
            .into_iter()
            .find(|candidate| stage_label(*candidate).eq_ignore_ascii_case(stage))
    {
        return (
            canonical_agent(AgentRole::for_stage(stage_label(stage))).name,
            step_label(stage),
        );
    }
    match kind {
        "learning_report" => ("LearningAgent", "Learning report"),
        "visualize_checkpoint" => ("Visualizer", "Writing review brief notes"),
        "learning-publish" => ("LearningAgent", "Publishing learning notes"),
        "index_embedding" | "index-scan" => ("Embeddings", "Indexing knowledge"),
        "prepare_context" => ("Preparation", "Preparing context"),
        "brownfield_map" => ("Brownfield", "Mapping the codebase"),
        "brownfield_enrich" => ("Brownfield", "Enriching component notes"),
        "brownfield_resolve" => ("Brownfield", "Resolving references"),
        "component-publish" => ("Brownfield", "Publishing component notes"),
        "user_task" => ("Brownfield", "Running a task"),
        "publication" => ("Publisher", "Publishing artifacts"),
        "busy" => ("Cleanup", "Removing leftover files"),
        _ => ("Runtime", "Background work"),
    }
}

/// `m:ss` since `acquired_at`, or `None` when the timestamp does not parse.
fn elapsed_since(acquired_at: &str, now: chrono::DateTime<chrono::Local>) -> Option<String> {
    let started = crate::time::parse_to_local(acquired_at)?;
    let seconds = (now - started).num_seconds().max(0);
    Some(if seconds >= 3600 {
        format!(
            "{}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        )
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    })
}

pub(crate) fn step_label(stage: PulsationStage) -> &'static str {
    match stage {
        PulsationStage::Intake => "Intake",
        PulsationStage::Specification => "Spec",
        PulsationStage::Contract => "Contract",
        PulsationStage::Code => "Code",
        PulsationStage::Test => "Test",
        PulsationStage::Integrator => "Integrator",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ikore_core::persistence_service::ActiveRuntimeJob;

    fn pulsation(
        stage: PulsationStage,
        status: PulsationStatus,
        terminal_reason: Option<&str>,
    ) -> Pulsation {
        Pulsation {
            terminal_reason: terminal_reason.map(str::to_string),
            id: "pulse".into(),
            project_id: "project".into(),
            feature_name: "Feature".into(),
            stage,
            status,
            created_at: "now".into(),
            updated_at: "now".into(),
        }
    }

    #[test]
    fn summary_and_tone_follow_the_pulsation_when_no_run_exists() {
        let running = PipelineStatus::derive(
            &pulsation(PulsationStage::Code, PulsationStatus::Running, None),
            None,
        );
        assert_eq!(
            format!("{} · {}", running.agent_name, running.state),
            "BackendAgent · Running"
        );
        assert_eq!(running.tone, StatusTone::Active);
        assert_eq!(running.stage_index, 3);
        assert!(running.stage_complete(2));
        assert!(!running.stage_complete(3));

        let done = PipelineStatus::derive(
            &pulsation(PulsationStage::Integrator, PulsationStatus::Done, None),
            None,
        );
        assert!(done.stage_complete(5));
        assert_eq!(done.tone, StatusTone::Success);
        assert_eq!(done.current_fill(), theme::success());

        let cancelled = PipelineStatus::derive(
            &pulsation(
                PulsationStage::Test,
                PulsationStatus::Failed,
                Some("user_cancel"),
            ),
            None,
        );
        assert_eq!(cancelled.state, "Cancelled");
        assert_eq!(cancelled.tone, StatusTone::Failure);
        assert_eq!(cancelled.current_fill(), theme::danger());
    }

    fn job(class: &str, kind: &str, pulse: Option<&str>) -> ActiveRuntimeJob {
        ActiveRuntimeJob {
            job_class: class.into(),
            kind: kind.into(),
            pulsation_id: pulse.map(str::to_string),
            feature_name: pulse.map(|_| "Feature".to_string()),
            acquired_at: "2026-09-24T12:00:00Z".into(),
        }
    }

    fn at(offset_seconds: i64) -> chrono::DateTime<chrono::Local> {
        chrono::DateTime::parse_from_rfc3339("2026-09-24T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Local)
            + chrono::Duration::seconds(offset_seconds)
    }

    #[test]
    fn header_names_the_background_job_that_outlives_its_pulsation() {
        let mut model = crate::pulsations::PulsationsModel {
            pulsations: vec![pulsation(
                PulsationStage::Integrator,
                PulsationStatus::Done,
                None,
            )],
            ..Default::default()
        };

        // Nothing holds a slot: the readout says so and shows the settled state in green.
        let idle = NowRunning::derive(&model, None, at(0));
        assert!(!idle.busy);
        assert_eq!(idle.text, "Idle · last: Feature · Integrator · Done");
        assert_eq!(idle.dot, theme::success());

        // The Learning report that runs after the pulsation finished is named, with its age.
        model.active_jobs = vec![job("background", "learning_report", Some("pulse"))];
        let learning = NowRunning::derive(&model, None, at(42));
        assert!(learning.busy);
        assert_eq!(learning.dot, theme::running());
        assert_eq!(
            learning.text,
            "Feature · Learning report · LearningAgent · 0:42"
        );
        assert_eq!(learning.also, None);
    }

    #[test]
    fn header_prefers_the_stage_run_and_mentions_the_background_one() {
        let model = crate::pulsations::PulsationsModel {
            pulsations: vec![pulsation(
                PulsationStage::Code,
                PulsationStatus::Running,
                None,
            )],
            active_jobs: vec![
                job("main", "run_stage::Code", Some("pulse")),
                job("background", "index_embedding", None),
            ],
            ..Default::default()
        };
        let now = NowRunning::derive(&model, None, at(3_725));
        assert_eq!(
            now.text,
            "Feature · Code · BackendAgent · Running · 1:02:05"
        );
        assert_eq!(now.also.as_deref(), Some("Indexing knowledge"));
    }

    #[test]
    fn slot_kinds_without_a_job_row_still_have_a_name() {
        assert_eq!(job_activity("run_stage::SPECIFICATION").1, "Spec");
        assert_eq!(
            job_activity("publication"),
            ("Publisher", "Publishing artifacts")
        );
        assert_eq!(job_activity("busy").1, "Removing leftover files");
        assert_eq!(job_activity("something-new").1, "Background work");
    }

    #[test]
    fn attention_states_share_the_warning_tone() {
        assert_eq!(
            StatusTone::for_run(TerminalRunStatus::AwaitingFallback),
            StatusTone::Attention
        );
        assert_eq!(
            StatusTone::for_pulsation(PulsationStatus::Checkpoint),
            StatusTone::Attention
        );
        assert_eq!(StatusTone::Attention.color(), theme::warning());
    }
}
