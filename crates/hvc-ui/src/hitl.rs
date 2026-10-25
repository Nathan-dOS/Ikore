use gpui::{App, KeyBinding, actions};
use hvc_core::orchestration::review_brief::{JudgementItem, JudgementKind, ReviewBrief};
use hvc_core::persistence_service::{
    ApplyHitlDecisionRequest, HitlCheckpoint, HitlDecision, HitlReviewDocument, Pulsation,
};
use hvc_editor::{MarkdownBlock, parse_markdown};

actions!(
    hitl,
    [
        ApproveHitl,
        RequestHitlRevision,
        RejectHitl,
        ConfirmBriefItem,
        FlagBriefItem,
        NextBriefItem,
        PreviousBriefItem
    ]
);

pub(crate) fn register_hitl_keybindings(cx: &mut App) {
    const REVIEW: &str = "HitlReview && !NativeTextInput";
    cx.bind_keys([
        KeyBinding::new("enter", ApproveHitl, Some(REVIEW)),
        KeyBinding::new("r", RequestHitlRevision, Some(REVIEW)),
        KeyBinding::new("d", RejectHitl, Some(REVIEW)),
        KeyBinding::new("c", ConfirmBriefItem, Some(REVIEW)),
        KeyBinding::new("f", FlagBriefItem, Some(REVIEW)),
        KeyBinding::new("tab", NextBriefItem, Some(REVIEW)),
        KeyBinding::new("shift-tab", PreviousBriefItem, Some(REVIEW)),
    ]);
}

/// The developer's mark on one judgement item of the brief (ADR-027 M2).
/// Session-local and keyed by the checkpoint it was made on; never a host gate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum ItemMark {
    #[default]
    Open,
    Confirmed,
    Flagged,
}

/// How a judgement kind is named to the developer and in revision feedback.
pub(crate) const fn judgement_kind_label(kind: JudgementKind) -> &'static str {
    match kind {
        JudgementKind::Decision => "Decision taken by the agent",
        JudgementKind::ReadingOnlyProof => "Proof by reading only",
        JudgementKind::NotApplicable => "Marked not applicable",
        JudgementKind::InterfaceChange => "Existing interface changes",
        JudgementKind::Exclusions => "Left out of scope",
        JudgementKind::NewInterface => "New interface",
        JudgementKind::ExistingTestsOnly => "Covered only by existing tests",
        JudgementKind::UnexecutedCheck => "Required check without a passing run",
        JudgementKind::Contradicted => "Contradicted by the review",
        JudgementKind::Unverified => "Not verified by the review",
        JudgementKind::Blocker => "Blocker reported by the reviewer",
        JudgementKind::DismissedConflict => "Architecture warning dismissed at triage",
    }
}

/// The Request revision feedback: one line per flagged item, in the order the
/// brief shows them, each carrying the item's own text so the revising agent
/// does not depend on labels it may not remember; then any free-text feedback.
/// With nothing flagged the free text goes out unchanged, as before M2.
pub(crate) fn compose_revision_feedback(
    items: &[JudgementItem],
    marks: &[ItemMark],
    note: impl Fn(usize) -> Option<String>,
    free_text: &str,
) -> String {
    let free_text = free_text.trim();
    let lines: Vec<String> = items
        .iter()
        .enumerate()
        .filter(|(index, _)| marks.get(*index) == Some(&ItemMark::Flagged))
        .map(|(index, item)| {
            let subject = match item.kind {
                JudgementKind::Exclusions => item.text.clone(),
                _ => format!("{}: {}", item.label, item.text),
            };
            let request = note(index)
                .map(|note| note.trim().to_owned())
                .filter(|note| !note.is_empty())
                .unwrap_or_else(|| "flagged for revision without a note.".to_owned());
            format!(
                "- [{}] {subject} — {request}",
                judgement_kind_label(item.kind)
            )
        })
        .collect();
    if lines.is_empty() {
        return free_text.to_owned();
    }
    let mut feedback = format!(
        "Flagged by the developer in the review brief:\n{}",
        lines.join("\n")
    );
    if !free_text.is_empty() {
        feedback.push_str("\n\nAdditional feedback: ");
        feedback.push_str(free_text);
    }
    feedback
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
    /// Derived view of the same checkpoint (ADR-027). Never gates approval.
    pub(crate) brief: Option<ReviewBrief>,
    pub(crate) brief_error: Option<String>,
    /// The Visualizer's note on the same checkpoint (ADR-027 M5). Never a
    /// gate: an error or an absent note leaves the brief as it is.
    pub(crate) visualization:
        Option<hvc_core::orchestration::review_brief::CheckpointVisualization>,
    /// The Brief tab is showing instead of a document.
    pub(crate) brief_selected: bool,
    /// One mark per judgement item of `brief`, in its order.
    pub(crate) marks: Vec<ItemMark>,
    /// The judgement item the C/F keys act on.
    pub(crate) brief_cursor: usize,
    /// What the Integrator reviewed (ADR-032), or the legacy notice. Never
    /// a gate: approval does not depend on it.
    pub(crate) review_inputs:
        Option<hvc_core::orchestration::composition::review_boundary::CheckpointReviewInputs>,
    /// The pulsation's revision rounds (ADR-032 M4). Beyond the threshold a
    /// revision needs an explicit decision, which the host enforces.
    pub(crate) convergence: Option<hvc_core::persistence_service::ConvergenceStatus>,
    /// Risk and coverage of the review (ADR-034 M3). An Approve of a
    /// high-risk review with unverified or unrun items needs a reason, which
    /// the host enforces.
    pub(crate) risk: Option<hvc_core::persistence_service::CheckpointRiskStatus>,
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
            self.visualization = None;
            self.brief_selected = false;
            self.marks.clear();
            self.brief_cursor = 0;
            self.review_inputs = None;
            self.convergence = None;
            self.risk = None;
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
                self.marks =
                    vec![ItemMark::Open; brief.as_ref().map_or(0, |brief| brief.judgement.len())];
                self.brief = brief;
                self.brief_error = None;
            }
            Err(error) => {
                self.brief = None;
                self.brief_error = Some(error);
                self.brief_selected = false;
                self.marks.clear();
            }
        }
        self.brief_cursor = 0;
        true
    }

    /// The Visualizer's note read with the brief of this load.
    pub(crate) fn finish_visualization(
        &mut self,
        generation: u64,
        checkpoint: &HitlCheckpoint,
        result: Result<
            Option<hvc_core::orchestration::review_brief::CheckpointVisualization>,
            String,
        >,
    ) -> bool {
        if generation != self.load_generation
            || !same_checkpoint(self.expected_checkpoint.as_ref(), Some(checkpoint))
        {
            return false;
        }
        self.visualization = result.ok().flatten();
        true
    }

    /// The review inputs and the round count read with this load. A failed
    /// read leaves them absent: neither ever blocks the review.
    pub(crate) fn finish_review_context(
        &mut self,
        generation: u64,
        checkpoint: &HitlCheckpoint,
        review_inputs: Result<
            Option<hvc_core::orchestration::composition::review_boundary::CheckpointReviewInputs>,
            String,
        >,
        convergence: Result<Option<hvc_core::persistence_service::ConvergenceStatus>, String>,
    ) -> bool {
        if generation != self.load_generation
            || !same_checkpoint(self.expected_checkpoint.as_ref(), Some(checkpoint))
        {
            return false;
        }
        self.review_inputs = review_inputs.ok().flatten();
        self.convergence = convergence.ok().flatten();
        true
    }

    /// The risk read with this load. A failed read leaves it absent; the host
    /// still refuses an Approve that needs an acknowledgement.
    pub(crate) fn finish_risk(
        &mut self,
        generation: u64,
        checkpoint: &HitlCheckpoint,
        risk: Result<Option<hvc_core::persistence_service::CheckpointRiskStatus>, String>,
    ) -> bool {
        if generation != self.load_generation
            || !same_checkpoint(self.expected_checkpoint.as_ref(), Some(checkpoint))
        {
            return false;
        }
        self.risk = risk.ok().flatten().filter(|status| status.applies);
        true
    }

    #[cfg(test)]
    pub(crate) fn current_load_generation(&self) -> u64 {
        self.load_generation
    }

    /// Whether an Approve needs an acknowledgement with a reason.
    pub(crate) fn risk_acknowledgement_required(&self) -> bool {
        self.risk
            .as_ref()
            .is_some_and(|status| status.acknowledgement_required)
    }

    /// Whether a further Request revision needs an explicit decision.
    pub(crate) fn beyond_threshold(&self) -> bool {
        self.convergence
            .as_ref()
            .is_some_and(hvc_core::persistence_service::ConvergenceStatus::threshold_reached)
    }

    /// A later read of a pending note; ignored once the checkpoint changed.
    pub(crate) fn update_visualization(
        &mut self,
        checkpoint: &HitlCheckpoint,
        result: Result<
            Option<hvc_core::orchestration::review_brief::CheckpointVisualization>,
            String,
        >,
    ) -> bool {
        if self.brief.is_none()
            || !same_checkpoint(self.expected_checkpoint.as_ref(), Some(checkpoint))
        {
            return false;
        }
        if let Ok(visualization) = result {
            self.visualization = visualization;
        }
        true
    }

    /// The checkpoint whose note is still being written, if any.
    pub(crate) fn pending_visualization(&self) -> Option<HitlCheckpoint> {
        self.visualization
            .as_ref()
            .is_some_and(|visualization| {
                visualization.state
                    == hvc_core::orchestration::review_brief::VisualizationState::Pending
            })
            .then(|| self.expected_checkpoint.clone())
            .flatten()
    }

    pub(crate) fn has_brief_tab(&self) -> bool {
        self.brief.is_some() || self.brief_error.is_some()
    }

    pub(crate) fn retry_documents(&mut self) {
        self.documents_error = None;
        self.brief = None;
        self.brief_error = None;
        self.visualization = None;
        self.review_inputs = None;
        self.convergence = None;
        self.risk = None;
        self.brief_selected = false;
        self.marks.clear();
        self.brief_cursor = 0;
    }

    /// Sets `mark` on item `index`, or reopens it when it already carries that
    /// mark. Moves the cursor to the item. Returns the item's new mark.
    pub(crate) fn toggle_mark(&mut self, index: usize, mark: ItemMark) -> Option<ItemMark> {
        if self.busy {
            return None;
        }
        let current = self.marks.get_mut(index)?;
        *current = if *current == mark {
            ItemMark::Open
        } else {
            mark
        };
        self.brief_cursor = index;
        Some(*current)
    }

    /// Moves the cursor to the next (or previous) item, wrapping around.
    pub(crate) fn move_brief_cursor(&mut self, forward: bool) {
        let count = self.marks.len();
        if count == 0 {
            return;
        }
        self.brief_cursor = if forward {
            (self.brief_cursor + 1) % count
        } else {
            (self.brief_cursor + count - 1) % count
        };
    }

    /// `(confirmed, flagged, open)` over the brief's judgement items.
    pub(crate) fn mark_counts(&self) -> (usize, usize, usize) {
        self.marks
            .iter()
            .fold((0, 0, 0), |(confirmed, flagged, open), mark| match mark {
                ItemMark::Confirmed => (confirmed + 1, flagged, open),
                ItemMark::Flagged => (confirmed, flagged + 1, open),
                ItemMark::Open => (confirmed, flagged, open + 1),
            })
    }

    pub(crate) fn flag_count(&self) -> usize {
        self.mark_counts().1
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
    use hvc_core::orchestration::review_brief::{SpecificationVisual, StageVisual};
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
            objective: Some("Reject expired tokens.".into()),
            counts: Vec::new(),
            revision: None,
            judgement: Vec::new(),
            verified: Vec::new(),
            visual: StageVisual::Specification(SpecificationVisual {
                facts: Default::default(),
                exclusions: Vec::new(),
                tasks: Vec::new(),
                files: Vec::new(),
                unattached_interfaces: Vec::new(),
                units: Vec::new(),
            }),
        }
    }

    fn visualization(
        state: hvc_core::orchestration::review_brief::VisualizationState,
    ) -> hvc_core::orchestration::review_brief::CheckpointVisualization {
        hvc_core::orchestration::review_brief::CheckpointVisualization {
            state,
            note: None,
            diagnostic: None,
        }
    }

    #[test]
    fn a_visualizer_note_belongs_to_its_checkpoint_and_never_gates_approval() {
        use hvc_core::orchestration::review_brief::VisualizationState;
        let mut state = HitlReviewState::default();
        state.load_checkpoint(Some(&pulse("first")), Some(checkpoint("first")));
        let (generation, first) = state.begin_documents().unwrap();
        state.finish_documents(generation, &first, Ok(documents()));
        state.finish_brief(generation, &first, Ok(Some(brief())));
        assert!(state.finish_visualization(
            generation,
            &first,
            Ok(Some(visualization(VisualizationState::Pending)))
        ));
        assert_eq!(
            state
                .pending_visualization()
                .map(|checkpoint| checkpoint.checkpoint_id),
            Some(first.checkpoint_id.clone())
        );
        assert!(state.can_approve(), "a pending note never blocks approval");

        // A poll result for the same checkpoint replaces the note; an error
        // keeps what was there.
        assert!(
            state
                .update_visualization(&first, Ok(Some(visualization(VisualizationState::Omitted))))
        );
        assert!(state.update_visualization(&first, Err("read failed".into())));
        assert_eq!(
            state.visualization.as_ref().unwrap().state,
            VisualizationState::Omitted
        );
        assert!(state.pending_visualization().is_none());

        // A newer checkpoint drops the note; late answers for the old one are ignored.
        state.load_checkpoint(Some(&pulse("second")), Some(checkpoint("second")));
        assert!(state.visualization.is_none());
        assert!(
            !state
                .update_visualization(&first, Ok(Some(visualization(VisualizationState::Pending))))
        );
        assert!(!state.finish_visualization(
            generation,
            &first,
            Ok(Some(visualization(VisualizationState::Pending)))
        ));
        assert!(state.visualization.is_none());
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

    fn item(kind: JudgementKind, label: &str, text: &str) -> JudgementItem {
        JudgementItem {
            kind,
            label: label.into(),
            text: text.into(),
            detail: None,
            refs: Vec::new(),
        }
    }

    fn brief_with_items() -> ReviewBrief {
        ReviewBrief {
            judgement: vec![
                item(JudgementKind::Decision, "D1", "Keep the public signature."),
                item(
                    JudgementKind::ReadingOnlyProof,
                    "AC-4",
                    "An empty range writes only the header row.",
                ),
                item(
                    JudgementKind::Exclusions,
                    "2",
                    "No scheduled export · No per-call rows",
                ),
            ],
            ..brief()
        }
    }

    #[test]
    fn marks_toggle_move_the_cursor_and_reset_with_the_checkpoint() {
        let mut state = HitlReviewState::default();
        state.load_checkpoint(Some(&pulse("first")), Some(checkpoint("first")));
        let (generation, first) = state.begin_documents().unwrap();
        state.finish_documents(generation, &first, Ok(documents()));
        state.finish_brief(generation, &first, Ok(Some(brief_with_items())));
        assert_eq!(state.marks, [ItemMark::Open; 3]);
        assert_eq!(state.mark_counts(), (0, 0, 3));

        assert_eq!(
            state.toggle_mark(1, ItemMark::Flagged),
            Some(ItemMark::Flagged)
        );
        assert_eq!(state.brief_cursor, 1);
        assert_eq!(
            state.toggle_mark(0, ItemMark::Confirmed),
            Some(ItemMark::Confirmed)
        );
        assert_eq!(state.mark_counts(), (1, 1, 1));
        // Confirming a flagged item replaces the flag; the same mark twice reopens it.
        assert_eq!(
            state.toggle_mark(1, ItemMark::Confirmed),
            Some(ItemMark::Confirmed)
        );
        assert_eq!(
            state.toggle_mark(1, ItemMark::Confirmed),
            Some(ItemMark::Open)
        );
        assert_eq!(state.toggle_mark(9, ItemMark::Flagged), None);

        state.brief_cursor = 2;
        state.move_brief_cursor(true);
        assert_eq!(state.brief_cursor, 0, "the cursor wraps forward");
        state.move_brief_cursor(false);
        assert_eq!(state.brief_cursor, 2, "and backward");

        // Marks never touch the approval gate, and a busy decision freezes them.
        assert!(state.can_approve());
        state.busy = true;
        assert_eq!(state.toggle_mark(2, ItemMark::Flagged), None);
        state.busy = false;

        state.load_checkpoint(Some(&pulse("second")), Some(checkpoint("second")));
        assert!(state.marks.is_empty() && state.brief_cursor == 0);
    }

    #[test]
    fn flagged_items_become_ordered_self_explaining_revision_feedback() {
        let items = brief_with_items().judgement;
        let marks = [ItemMark::Flagged, ItemMark::Confirmed, ItemMark::Flagged];
        let notes = |index: usize| (index == 0).then(|| "  Keep it, but document why.  ".into());
        assert_eq!(
            compose_revision_feedback(&items, &marks, notes, "Also rename the module."),
            "Flagged by the developer in the review brief:\n\
             - [Decision taken by the agent] D1: Keep the public signature. — Keep it, but document why.\n\
             - [Left out of scope] No scheduled export · No per-call rows — flagged for revision without a note.\n\
             \n\
             Additional feedback: Also rename the module."
        );
        // Without flags the free text goes out exactly as before M2.
        assert_eq!(
            compose_revision_feedback(&items, &[ItemMark::Open; 3], |_| None, " As typed. "),
            "As typed."
        );
        assert_eq!(
            compose_revision_feedback(&items, &[ItemMark::Open; 3], |_| None, ""),
            ""
        );
    }

    #[test]
    fn items_sharing_a_label_keep_independent_marks_and_notes() {
        // Two interface changes on the same path carry the same label; marks
        // and notes are keyed by position, never by label.
        let items = vec![
            item(
                JudgementKind::InterfaceChange,
                "src/auth.rs",
                "validate_token changes shape.",
            ),
            item(
                JudgementKind::InterfaceChange,
                "src/auth.rs",
                "Token gains an issuer field.",
            ),
        ];
        let mut state = HitlReviewState::default();
        state.load_checkpoint(Some(&pulse("first")), Some(checkpoint("first")));
        let (generation, first) = state.begin_documents().unwrap();
        state.finish_documents(generation, &first, Ok(documents()));
        state.finish_brief(
            generation,
            &first,
            Ok(Some(ReviewBrief {
                judgement: items.clone(),
                ..brief()
            })),
        );
        state.toggle_mark(0, ItemMark::Flagged);
        state.toggle_mark(1, ItemMark::Confirmed);
        assert_eq!(state.marks, [ItemMark::Flagged, ItemMark::Confirmed]);
        state.toggle_mark(1, ItemMark::Flagged);
        let notes = |index: usize| Some(format!("note {index}"));
        let feedback = compose_revision_feedback(&items, &state.marks, notes, "");
        assert_eq!(
            feedback.lines().skip(1).collect::<Vec<_>>(),
            [
                "- [Existing interface changes] src/auth.rs: validate_token changes shape. — note 0",
                "- [Existing interface changes] src/auth.rs: Token gains an issuer field. — note 1",
            ]
        );
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

    /// ADR-032: the review inputs and the round count belong to the load
    /// that read them; a failed read leaves them absent and never blocks.
    #[test]
    fn review_context_belongs_to_its_checkpoint_and_never_gates_the_review() {
        use hvc_core::persistence_service::ConvergenceStatus;
        let reached = ConvergenceStatus {
            applies: true,
            threshold: 2,
            rounds: 2,
            ..Default::default()
        };
        let mut state = HitlReviewState::default();
        state.load_checkpoint(Some(&pulse("first")), Some(checkpoint("first")));
        let (generation, first) = state.begin_documents().unwrap();
        state.finish_documents(generation, &first, Ok(documents()));
        assert!(!state.finish_review_context(
            generation + 1,
            &first,
            Ok(None),
            Ok(Some(reached.clone()))
        ));
        assert!(!state.beyond_threshold(), "a stale read is ignored");
        assert!(state.finish_review_context(
            generation,
            &first,
            Err("unreadable".into()),
            Ok(Some(reached.clone()))
        ));
        assert!(state.review_inputs.is_none());
        assert!(state.beyond_threshold());
        assert!(state.can_approve(), "rounds never gate approval");
        // Below the threshold, or before pipeline v4, nothing is required.
        state.convergence = Some(ConvergenceStatus {
            rounds: 1,
            ..reached.clone()
        });
        assert!(!state.beyond_threshold());
        state.convergence = Some(ConvergenceStatus {
            applies: false,
            ..reached.clone()
        });
        assert!(!state.beyond_threshold());
        // A new checkpoint starts without the previous one's context.
        state.convergence = Some(reached);
        state.load_checkpoint(Some(&pulse("second")), Some(checkpoint("second")));
        assert!(state.convergence.is_none() && state.review_inputs.is_none());
    }

    /// ADR-034 M3: the risk status belongs to the load that read it; only a
    /// pipeline v4 status that says so requires an acknowledgement, and a
    /// failed read leaves it to the host to refuse.
    #[test]
    fn the_risk_status_belongs_to_its_checkpoint() {
        use hvc_core::persistence_service::CheckpointRiskStatus;
        let required = CheckpointRiskStatus {
            applies: true,
            acknowledgement_required: true,
            ..Default::default()
        };
        let mut state = HitlReviewState::default();
        state.load_checkpoint(Some(&pulse("first")), Some(checkpoint("first")));
        let (generation, first) = state.begin_documents().unwrap();
        state.finish_documents(generation, &first, Ok(documents()));
        assert!(!state.finish_risk(generation + 1, &first, Ok(Some(required.clone()))));
        assert!(
            !state.risk_acknowledgement_required(),
            "a stale read is ignored"
        );
        assert!(state.finish_risk(generation, &first, Err("unreadable".into())));
        assert!(state.risk.is_none() && !state.risk_acknowledgement_required());
        assert!(state.finish_risk(
            generation,
            &first,
            Ok(Some(CheckpointRiskStatus {
                applies: false,
                ..required.clone()
            }))
        ));
        assert!(state.risk.is_none(), "earlier pipelines have no risk card");
        assert!(state.finish_risk(generation, &first, Ok(Some(required))));
        assert!(state.risk_acknowledgement_required());
        assert!(
            state.can_approve(),
            "the host, not the UI, decides the refusal"
        );
        state.load_checkpoint(Some(&pulse("second")), Some(checkpoint("second")));
        assert!(state.risk.is_none());
    }
}
