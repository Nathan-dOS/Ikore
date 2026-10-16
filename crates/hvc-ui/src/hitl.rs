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

/// The developer's mark on one judgement item of the brief (ADR-024 M2).
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
    /// Derived view of the same checkpoint (ADR-024). Never gates approval.
    pub(crate) brief: Option<ReviewBrief>,
    pub(crate) brief_error: Option<String>,
    /// The Brief tab is showing instead of a document.
    pub(crate) brief_selected: bool,
    /// One mark per judgement item of `brief`, in its order.
    pub(crate) marks: Vec<ItemMark>,
    /// The judgement item the C/F keys act on.
    pub(crate) brief_cursor: usize,
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
            self.marks.clear();
            self.brief_cursor = 0;
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

    pub(crate) fn has_brief_tab(&self) -> bool {
        self.brief.is_some() || self.brief_error.is_some()
    }

    pub(crate) fn retry_documents(&mut self) {
        self.documents_error = None;
        self.brief = None;
        self.brief_error = None;
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
            revision: None,
            judgement: Vec::new(),
            exclusions: Vec::new(),
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
}
