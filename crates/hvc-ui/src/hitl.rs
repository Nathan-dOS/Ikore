use gpui::{App, KeyBinding, actions};
use hvc_core::orchestration::review_brief::ReviewBrief;
use hvc_core::persistence_service::{
    ApplyHitlDecisionRequest, HitlCheckpoint, HitlDecision, HitlReviewDocument, Pulsation,
};
use hvc_editor::{MarkdownBlock, parse_markdown};

actions!(hitl, [ApproveHitl, RequestHitlRevision, RejectHitl]);

pub(crate) fn register_hitl_keybindings(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("enter", ApproveHitl, Some("HitlReview && !NativeTextInput")),
        KeyBinding::new(
            "r",
            RequestHitlRevision,
            Some("HitlReview && !NativeTextInput"),
        ),
        KeyBinding::new("d", RejectHitl, Some("HitlReview && !NativeTextInput")),
    ]);
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReviewShortcut {
    Approve,
    RequestRevision,
    Reject,
}

#[cfg(test)]
impl ReviewShortcut {
    fn from_key(key: &str, typing_target: bool) -> Option<Self> {
        if typing_target {
            return None;
        }
        match key.to_ascii_lowercase().as_str() {
            "enter" => Some(Self::Approve),
            "r" => Some(Self::RequestRevision),
            "d" => Some(Self::Reject),
            _ => None,
        }
    }

    fn decision(self) -> HitlDecision {
        match self {
            Self::Approve => HitlDecision::Approve,
            Self::RequestRevision => HitlDecision::RequestRevision,
            Self::Reject => HitlDecision::Reject,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct HitlReviewState {
    pub(crate) expected_checkpoint: Option<HitlCheckpoint>,
    pub(crate) pulsation_id: Option<String>,
    pub(crate) title: Option<String>,
    pub(crate) documents: Vec<ReviewDocument>,
    pub(crate) selected_document: usize,
    pub(crate) show_source: bool,
    pub(crate) show_feedback: bool,
    pub(crate) documents_loading: bool,
    pub(crate) documents_error: Option<String>,
    /// Derived view of the same checkpoint (ADR-024). Never gates approval.
    pub(crate) brief: Option<ReviewBrief>,
    pub(crate) brief_error: Option<String>,
    /// The Brief tab is showing instead of a document.
    pub(crate) brief_selected: bool,
    pub(crate) busy: bool,
    pub(crate) error: Option<String>,
    load_generation: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct ReviewDocument {
    pub(crate) artifact: HitlReviewDocument,
    pub(crate) preview: Vec<MarkdownBlock>,
}

fn same_checkpoint(left: Option<&HitlCheckpoint>, right: Option<&HitlCheckpoint>) -> bool {
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

impl HitlReviewState {
    pub(crate) fn load_checkpoint(
        &mut self,
        pulsation: Option<&Pulsation>,
        checkpoint: Option<HitlCheckpoint>,
    ) {
        let checkpoint = checkpoint.filter(|checkpoint| {
            pulsation.is_some_and(|pulse| pulse.id == checkpoint.pulsation_id)
        });
        if !same_checkpoint(self.expected_checkpoint.as_ref(), checkpoint.as_ref()) {
            self.load_generation = self.load_generation.wrapping_add(1);
            self.documents.clear();
            self.selected_document = 0;
            self.show_source = false;
            self.show_feedback = false;
            self.busy = false;
            self.documents_loading = false;
            self.documents_error = None;
            self.brief = None;
            self.brief_error = None;
            self.brief_selected = false;
            self.error = None;
        }
        self.expected_checkpoint = checkpoint;
        self.pulsation_id = self
            .expected_checkpoint
            .as_ref()
            .map(|c| c.pulsation_id.clone());
        self.title = pulsation
            .filter(|_| self.expected_checkpoint.is_some())
            .map(|pulse| format!("{:?} Review — {}", pulse.stage, pulse.feature_name));
    }

    pub(crate) fn begin_documents(&mut self) -> Option<(u64, HitlCheckpoint)> {
        if self.documents_loading || !self.documents.is_empty() || self.documents_error.is_some() {
            return None;
        }
        let checkpoint = self.expected_checkpoint.clone()?;
        self.load_generation = self.load_generation.wrapping_add(1);
        self.documents_loading = true;
        Some((self.load_generation, checkpoint))
    }

    pub(crate) fn finish_documents(
        &mut self,
        generation: u64,
        checkpoint: &HitlCheckpoint,
        result: Result<Vec<HitlReviewDocument>, String>,
    ) -> bool {
        if generation != self.load_generation
            || !same_checkpoint(self.expected_checkpoint.as_ref(), Some(checkpoint))
        {
            return false;
        }
        self.documents_loading = false;
        match result {
            Ok(documents) if !documents.is_empty() => {
                self.documents = documents
                    .into_iter()
                    .map(|artifact| ReviewDocument {
                        preview: parse_markdown(&artifact.content),
                        artifact,
                    })
                    .collect();
                self.documents_error = None;
            }
            Ok(_) => {
                self.documents_error =
                    Some("No persisted documents are available for this checkpoint.".into())
            }
            Err(error) => self.documents_error = Some(error),
        }
        true
    }

    /// Applies the brief read that followed `finish_documents` for the same
    /// generation. A brief opens on its own tab; a failed brief read only
    /// adds a tab that explains it, so the documents stay the default view.
    pub(crate) fn finish_brief(
        &mut self,
        generation: u64,
        checkpoint: &HitlCheckpoint,
        result: Result<Option<ReviewBrief>, String>,
    ) -> bool {
        if generation != self.load_generation
            || !same_checkpoint(self.expected_checkpoint.as_ref(), Some(checkpoint))
        {
            return false;
        }
        match result {
            Ok(brief) => {
                self.brief_selected = brief.is_some() && !self.documents.is_empty();
                self.brief = brief;
                self.brief_error = None;
            }
            Err(error) => {
                self.brief = None;
                self.brief_error = Some(error);
                self.brief_selected = false;
            }
        }
        true
    }

    pub(crate) fn has_brief_tab(&self) -> bool {
        self.brief.is_some() || self.brief_error.is_some()
    }

    pub(crate) fn retry_documents(&mut self) {
        self.documents_error = None;
        self.brief = None;
        self.brief_error = None;
        self.brief_selected = false;
    }

    pub(crate) fn can_approve(&self) -> bool {
        !self.busy
            && !self.documents_loading
            && !self.documents.is_empty()
            && self.documents_error.is_none()
            && self.expected_checkpoint.is_some()
    }

    pub(crate) fn begin(&mut self, decision: HitlDecision) -> Option<ApplyHitlDecisionRequest> {
        if self.busy || (decision == HitlDecision::Approve && !self.can_approve()) {
            return None;
        }
        let pulsation_id = self.pulsation_id.clone()?;
        let checkpoint = self.expected_checkpoint.as_ref()?;
        if checkpoint.pulsation_id != pulsation_id {
            return None;
        }
        self.busy = true;
        self.error = None;
        Some(ApplyHitlDecisionRequest {
            pulsation_id,
            decision,
            checkpoint_id: checkpoint.checkpoint_id.clone(),
            expected_state_version: checkpoint.state_version,
            feedback: None,
        })
    }

    pub(crate) fn finish(&mut self, result: Result<(), String>) {
        self.busy = false;
        match result {
            Ok(()) => {
                self.load_checkpoint(None, None);
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hvc_core::persistence_service::{PulsationStage, PulsationStatus};

    #[gpui::test]
    fn handles_review_shortcuts_outside_typing_targets(_: &mut gpui::TestAppContext) {
        assert_eq!(
            ReviewShortcut::from_key("Enter", false).map(ReviewShortcut::decision),
            Some(HitlDecision::Approve)
        );
        assert_eq!(
            ReviewShortcut::from_key("R", false).map(ReviewShortcut::decision),
            Some(HitlDecision::RequestRevision)
        );
        assert_eq!(ReviewShortcut::from_key("Enter", true), None);
        assert_eq!(ReviewShortcut::from_key("r", true), None);
    }

    fn pulse(id: &str) -> Pulsation {
        Pulsation {
            terminal_reason: None,
            id: id.into(),
            project_id: "project-1".into(),
            feature_name: "Native editor".into(),
            stage: PulsationStage::Code,
            status: PulsationStatus::Checkpoint,
            created_at: "2026-08-24T00:00:00Z".into(),
            updated_at: "2026-08-24T00:00:00Z".into(),
        }
    }

    fn checkpoint(id: &str) -> HitlCheckpoint {
        HitlCheckpoint {
            pulsation_id: id.into(),
            checkpoint_id: format!("checkpoint-{id}"),
            state_version: 1,
        }
    }

    fn documents() -> Vec<HitlReviewDocument> {
        vec![HitlReviewDocument {
            artifact_id: "artifact-1".into(),
            name: "Contract".into(),
            kind: "stage_output".into(),
            relative_path: None,
            content: "# Complete contract\n\nReview the entire document.\n".into(),
        }]
    }

    #[test]
    fn requires_real_content_before_approval_and_blocks_duplicate_decisions() {
        let mut state = HitlReviewState::default();
        state.load_checkpoint(Some(&pulse("pulse-1")), Some(checkpoint("pulse-1")));
        assert_eq!(state.title.as_deref(), Some("Code Review — Native editor"));
        assert!(state.begin(HitlDecision::Approve).is_none());
        let (generation, checkpoint) = state.begin_documents().unwrap();
        assert!(state.begin_documents().is_none());
        assert!(state.finish_documents(generation, &checkpoint, Ok(documents())));
        assert!(state.begin(HitlDecision::Approve).is_some());
        assert!(state.busy);
        assert!(state.begin(HitlDecision::Reject).is_none());
        state.finish(Ok(()));
        assert!(!state.busy);
    }

    #[test]
    fn rejects_late_documents_and_preserves_same_checkpoint_reader_state() {
        let mut state = HitlReviewState::default();
        state.load_checkpoint(Some(&pulse("first")), Some(checkpoint("first")));
        let (old_generation, old_checkpoint) = state.begin_documents().unwrap();
        state.load_checkpoint(Some(&pulse("second")), Some(checkpoint("second")));
        let (generation, current) = state.begin_documents().unwrap();
        assert!(!state.finish_documents(old_generation, &old_checkpoint, Ok(documents())));
        assert!(state.documents.is_empty());
        assert!(state.documents_loading);
        assert!(state.finish_documents(generation, &current, Ok(documents())));
        state.show_source = true;
        state.load_checkpoint(Some(&pulse("second")), Some(checkpoint("second")));
        assert!(state.show_source);
        assert_eq!(state.documents[0].artifact.content, documents()[0].content);
        let mut advanced = checkpoint("second");
        advanced.state_version += 1;
        state.load_checkpoint(Some(&pulse("second")), Some(advanced));
        assert!(state.documents.is_empty());
        assert!(!state.show_source);
        assert!(!state.finish_documents(generation, &current, Ok(documents())));
    }

    fn brief() -> ReviewBrief {
        ReviewBrief {
            stage: PulsationStage::Specification,
            next_stage: Some(PulsationStage::Contract),
            objective: "Reject expired tokens.".into(),
            facts: Default::default(),
            judgement: Vec::new(),
            verified: Vec::new(),
            tasks: Vec::new(),
            files: Vec::new(),
            unattached_interfaces: Vec::new(),
            units: Vec::new(),
        }
    }

    #[test]
    fn a_brief_opens_first_but_never_replaces_the_documents_or_the_approval_gate() {
        let mut state = HitlReviewState::default();
        state.load_checkpoint(Some(&pulse("first")), Some(checkpoint("first")));
        let (generation, first) = state.begin_documents().unwrap();
        assert!(state.finish_documents(generation, &first, Ok(documents())));
        assert!(state.finish_brief(generation, &first, Ok(Some(brief()))));
        assert!(state.brief_selected && state.has_brief_tab());
        assert!(state.can_approve(), "approval still rests on the documents");

        // A newer checkpoint drops the brief and its selection; a late
        // brief of the old one is ignored.
        state.load_checkpoint(Some(&pulse("second")), Some(checkpoint("second")));
        assert!(state.brief.is_none() && !state.brief_selected);
        assert!(!state.finish_brief(generation, &first, Ok(Some(brief()))));
        assert!(!state.has_brief_tab());
    }

    #[test]
    fn a_failed_brief_read_keeps_documents_in_front_and_explains_itself() {
        let mut state = HitlReviewState::default();
        state.load_checkpoint(Some(&pulse("first")), Some(checkpoint("first")));
        let (generation, first) = state.begin_documents().unwrap();
        state.finish_documents(generation, &first, Ok(documents()));
        state.finish_brief(generation, &first, Err("payload unreadable".into()));
        assert!(!state.brief_selected);
        assert!(state.has_brief_tab());
        assert_eq!(state.brief_error.as_deref(), Some("payload unreadable"));
        assert!(state.can_approve());

        // Stages without a brief show no tab at all.
        let mut legacy = HitlReviewState::default();
        legacy.load_checkpoint(Some(&pulse("legacy")), Some(checkpoint("legacy")));
        let (generation, current) = legacy.begin_documents().unwrap();
        legacy.finish_documents(generation, &current, Ok(documents()));
        legacy.finish_brief(generation, &current, Ok(None));
        assert!(!legacy.has_brief_tab() && !legacy.brief_selected);
    }

    #[test]
    fn failed_read_can_retry_or_request_revision_without_fabricated_content() {
        let mut state = HitlReviewState::default();
        state.load_checkpoint(Some(&pulse("first")), Some(checkpoint("first")));
        let (generation, checkpoint) = state.begin_documents().unwrap();
        state.finish_documents(generation, &checkpoint, Err("Read failed".into()));
        assert!(state.documents.is_empty());
        assert!(!state.can_approve());
        assert!(state.begin_documents().is_none());
        state.retry_documents();
        assert!(state.begin_documents().is_some());
        assert!(state.begin(HitlDecision::RequestRevision).is_some());
    }
}
