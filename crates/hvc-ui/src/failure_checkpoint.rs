//! Decision state for a stage failure parked on a checkpoint.
//!
//! Mirrors [`crate::hitl::HitlReviewState`] deliberately: the same generation fencing
//! for the async detail load, and the same optimistic-concurrency stamp on the request,
//! so a snapshot arriving mid-decision can never apply a stale choice.
//!
//! The difference from a HITL review is which buttons exist at all. The host persists
//! `allow_retry`/`allow_revision` per checkpoint, and this state only ever offers what
//! the host said it would accept.

use gpui::{App, KeyBinding, actions};
use hvc_core::persistence_service::{
    ResolveStageFailureRequest, StageFailureCheckpoint, StageFailureDecision, StageFailureDetail,
};

actions!(
    failure,
    [RetryFailedStage, ReviseFailedStage, AbortFailedStage]
);

pub(crate) fn register_failure_keybindings(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new(
            "enter",
            RetryFailedStage,
            Some("FailureCheckpoint && !NativeTextInput"),
        ),
        KeyBinding::new(
            "r",
            ReviseFailedStage,
            Some("FailureCheckpoint && !NativeTextInput"),
        ),
        KeyBinding::new(
            "d",
            AbortFailedStage,
            Some("FailureCheckpoint && !NativeTextInput"),
        ),
    ]);
}

#[derive(Debug, Default)]
pub(crate) struct FailureCheckpointState {
    pub(crate) checkpoint: Option<StageFailureCheckpoint>,
    pub(crate) detail: Option<StageFailureDetail>,
    pub(crate) detail_loading: bool,
    pub(crate) detail_error: Option<String>,
    pub(crate) show_feedback: bool,
    pub(crate) busy: bool,
    pub(crate) error: Option<String>,
    load_generation: u64,
}

fn same_checkpoint(
    left: Option<&StageFailureCheckpoint>,
    right: Option<&StageFailureCheckpoint>,
) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => {
            left.pulsation_id == right.pulsation_id
                && left.checkpoint_id == right.checkpoint_id
                && left.state_version == right.state_version
        }
        (None, None) => true,
        _ => false,
    }
}

impl FailureCheckpointState {
    pub(crate) fn load_checkpoint(&mut self, checkpoint: Option<StageFailureCheckpoint>) {
        if !same_checkpoint(self.checkpoint.as_ref(), checkpoint.as_ref()) {
            self.load_generation = self.load_generation.wrapping_add(1);
            self.detail = None;
            self.detail_loading = false;
            self.detail_error = None;
            self.show_feedback = false;
            self.busy = false;
            self.error = None;
        }
        self.checkpoint = checkpoint;
    }

    pub(crate) fn title(&self) -> Option<String> {
        self.checkpoint
            .as_ref()
            .map(|c| format!("{:?} failed — {}", c.stage, c.feature_name))
    }

    /// A one-line explanation of why these particular buttons are the ones offered.
    pub(crate) fn rationale(&self) -> Option<&'static str> {
        let checkpoint = self.checkpoint.as_ref()?;
        Some(match checkpoint.failure_class.as_str() {
            "retry_budget_exhausted" => {
                "The automatic attempts ran out. Retrying hands the agent its own rejected output."
            }
            "provider_configuration" => {
                "A credential, billing or routing problem. Fix it in Settings before retrying."
            }
            "provider_transport" => "The provider connection failed. Retrying usually clears it.",
            "infrastructure" => "Local storage failed. Retrying usually clears it.",
            "executor_setup" => "The executor died before the agent started. Nothing was produced.",
            "host_contract" => {
                "The output broke a host rule, so an identical retry would break it again. Guide the agent in writing."
            }
            "model_reported_failure" => {
                "The agent assessed the work and gave up. Its answer is already durable, so only written guidance changes the outcome."
            }
            _ => "The stage failed and is waiting on a decision.",
        })
    }

    pub(crate) fn allows(&self, decision: StageFailureDecision) -> bool {
        self.checkpoint.as_ref().is_some_and(|c| match decision {
            StageFailureDecision::RetryStage => c.allow_retry,
            StageFailureDecision::RequestRevision => c.allow_revision,
            StageFailureDecision::Abort => true,
        })
    }

    pub(crate) fn begin_detail(&mut self) -> Option<(u64, String)> {
        if self.detail_loading || self.detail.is_some() || self.detail_error.is_some() {
            return None;
        }
        let checkpoint_id = self.checkpoint.as_ref()?.checkpoint_id.clone();
        self.load_generation = self.load_generation.wrapping_add(1);
        self.detail_loading = true;
        Some((self.load_generation, checkpoint_id))
    }

    pub(crate) fn finish_detail(
        &mut self,
        generation: u64,
        checkpoint_id: &str,
        result: Result<StageFailureDetail, String>,
    ) -> bool {
        if generation != self.load_generation
            || self
                .checkpoint
                .as_ref()
                .is_none_or(|c| c.checkpoint_id != checkpoint_id)
        {
            return false;
        }
        self.detail_loading = false;
        match result {
            Ok(detail) => {
                self.detail = Some(detail);
                self.detail_error = None;
            }
            Err(error) => self.detail_error = Some(error),
        }
        true
    }

    pub(crate) fn retry_detail(&mut self) {
        self.detail_error = None;
    }

    pub(crate) fn begin(
        &mut self,
        decision: StageFailureDecision,
    ) -> Option<ResolveStageFailureRequest> {
        if self.busy || !self.allows(decision) {
            return None;
        }
        let checkpoint = self.checkpoint.as_ref()?;
        self.busy = true;
        self.error = None;
        Some(ResolveStageFailureRequest {
            checkpoint_id: checkpoint.checkpoint_id.clone(),
            expected_state_version: checkpoint.state_version,
            decision,
            feedback: None,
        })
    }

    pub(crate) fn finish(&mut self, result: Result<(), String>) {
        self.busy = false;
        match result {
            Ok(()) => {
                self.load_checkpoint(None);
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hvc_core::persistence_service::PulsationStage;

    fn checkpoint(id: &str, retry: bool, revision: bool) -> StageFailureCheckpoint {
        StageFailureCheckpoint {
            checkpoint_id: format!("checkpoint-{id}"),
            pulsation_id: id.into(),
            project_id: "project-1".into(),
            feature_name: "Native editor".into(),
            stage: PulsationStage::Test,
            failure_class: "retry_budget_exhausted".into(),
            error_code: "provider_timeout".into(),
            diagnostic: "the provider timed out".into(),
            allow_retry: retry,
            allow_revision: revision,
            revision_stage: revision.then_some(PulsationStage::Code),
            attempts: 3,
            state_version: 7,
            created_at: "2026-09-09T00:00:00Z".into(),
        }
    }

    fn detail() -> StageFailureDetail {
        StageFailureDetail {
            checkpoint_id: "checkpoint-pulse-1".into(),
            raw_output: Some("{\"broken\":".into()),
            validation_error: Some("unexpected end of input".into()),
        }
    }

    #[test]
    fn never_offers_a_decision_the_host_would_reject() {
        let mut state = FailureCheckpointState::default();
        // The agent gave up: no retry, revision only.
        state.load_checkpoint(Some(checkpoint("pulse-1", false, true)));
        assert!(state.begin(StageFailureDecision::RetryStage).is_none());
        assert!(state.begin(StageFailureDecision::RequestRevision).is_some());
        assert!(state.busy);
        // Single-flight: a second click while in flight is dropped.
        assert!(state.begin(StageFailureDecision::Abort).is_none());
        state.finish(Ok(()));
        assert!(!state.busy);
        assert!(state.checkpoint.is_none());
    }

    #[test]
    fn provider_faults_offer_retry_and_abort_but_never_revision() {
        let mut state = FailureCheckpointState::default();
        state.load_checkpoint(Some(checkpoint("pulse-1", true, false)));
        assert!(state.allows(StageFailureDecision::RetryStage));
        assert!(!state.allows(StageFailureDecision::RequestRevision));
        assert!(state.allows(StageFailureDecision::Abort));
        assert!(state.begin(StageFailureDecision::RequestRevision).is_none());
    }

    #[test]
    fn no_checkpoint_means_no_decision_at_all() {
        let mut state = FailureCheckpointState::default();
        for decision in [
            StageFailureDecision::RetryStage,
            StageFailureDecision::RequestRevision,
            StageFailureDecision::Abort,
        ] {
            assert!(!state.allows(decision));
            assert!(state.begin(decision).is_none());
        }
    }

    #[test]
    fn stamps_the_state_version_it_was_shown() {
        let mut state = FailureCheckpointState::default();
        state.load_checkpoint(Some(checkpoint("pulse-1", true, true)));
        let request = state.begin(StageFailureDecision::RetryStage).unwrap();
        assert_eq!(request.checkpoint_id, "checkpoint-pulse-1");
        assert_eq!(request.expected_state_version, 7);
    }

    #[test]
    fn rejects_late_detail_and_resets_on_a_newer_state_version() {
        let mut state = FailureCheckpointState::default();
        state.load_checkpoint(Some(checkpoint("first", true, true)));
        let (stale_generation, stale_id) = state.begin_detail().unwrap();
        assert!(state.begin_detail().is_none());

        state.load_checkpoint(Some(checkpoint("second", true, true)));
        let (generation, id) = state.begin_detail().unwrap();
        assert!(!state.finish_detail(stale_generation, &stale_id, Ok(detail())));
        assert!(state.detail.is_none());
        assert!(state.detail_loading);
        assert!(state.finish_detail(generation, &id, Ok(detail())));
        assert!(state.detail.is_some());

        // Same identity, newer version: the failure was re-opened, so drop what we hold.
        let mut advanced = checkpoint("second", true, true);
        advanced.state_version += 1;
        state.load_checkpoint(Some(advanced));
        assert!(state.detail.is_none());
        assert!(!state.finish_detail(generation, &id, Ok(detail())));
    }

    #[test]
    fn a_failed_detail_read_can_be_retried_and_never_blocks_deciding() {
        let mut state = FailureCheckpointState::default();
        state.load_checkpoint(Some(checkpoint("pulse-1", true, true)));
        let (generation, id) = state.begin_detail().unwrap();
        state.finish_detail(generation, &id, Err("read failed".into()));
        assert_eq!(state.detail_error.as_deref(), Some("read failed"));
        assert!(state.begin_detail().is_none());
        state.retry_detail();
        assert!(state.begin_detail().is_some());
        // Losing the model output must not strand the operator: deciding still works.
        assert!(state.begin(StageFailureDecision::Abort).is_some());
    }

    #[test]
    fn every_persisted_class_has_its_own_rationale() {
        let mut state = FailureCheckpointState::default();
        let mut seen = std::collections::HashSet::new();
        for class in [
            "retry_budget_exhausted",
            "provider_configuration",
            "provider_transport",
            "infrastructure",
            "executor_setup",
            "host_contract",
            "model_reported_failure",
        ] {
            let mut c = checkpoint("pulse-1", true, true);
            c.failure_class = class.into();
            state.load_checkpoint(Some(c));
            let rationale = state.rationale().expect("rationale");
            assert!(seen.insert(rationale), "{class} reused another rationale");
        }
    }
}
