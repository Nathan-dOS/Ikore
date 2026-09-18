//! "Where is the active pulsation right now" — shared by the flow-bar status pill
//! (`shell.rs`, visible in every view) and the Agents header stepper (`agents_view.rs`).

use gpui::{Rgba, rgb};
use hvc_core::llm::AgentRole;
use hvc_core::persistence_service::{Pulsation, PulsationStage, PulsationStatus};
use hvc_core::terminals::{TerminalRunStatus, TerminalsSnapshot, canonical_agent};

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
            // Same blue the Pulsations view uses for in-flight states.
            Self::Active => rgb(0x73b7ff),
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
        // Prefer the run still in flight for this pulsation; otherwise the latest one for it.
        let run = snapshot.and_then(|snapshot| {
            let agent = snapshot
                .agents
                .iter()
                .find(|agent| agent.config.agent_role == role)?;
            let mine = |run: &&hvc_core::terminals::TerminalRun| {
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

    /// e.g. "Code · BackendAgent · Running"
    pub(crate) fn summary(&self) -> String {
        format!(
            "{} · {} · {}",
            compact_stage_label(self.stage),
            self.agent_name,
            self.state
        )
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
            _ => theme::accent(),
        }
    }
}

pub(crate) fn compact_stage_label(stage: PulsationStage) -> &'static str {
    match stage {
        PulsationStage::Intake => "Int",
        PulsationStage::Specification => "Spec",
        PulsationStage::Contract => "Ctr",
        PulsationStage::Code => "Code",
        PulsationStage::Test => "Test",
        PulsationStage::Integrator => "Intg",
    }
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
        assert_eq!(running.summary(), "Code · BackendAgent · Running");
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
        assert_eq!(done.current_fill(), theme::accent());

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
