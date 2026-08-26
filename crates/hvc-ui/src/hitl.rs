use gpui::{App, KeyBinding, actions};
use hvc_core::persistence_service::{HitlDecision, Pulsation};

actions!(hitl, [ApproveHitl, RequestHitlRevision]);

pub(crate) fn register_hitl_keybindings(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("enter", ApproveHitl, Some("HitlReview")),
        KeyBinding::new("r", RequestHitlRevision, Some("HitlReview")),
    ]);
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReviewShortcut {
    Approve,
    RequestRevision,
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
            _ => None,
        }
    }

    fn decision(self) -> HitlDecision {
        match self {
            Self::Approve => HitlDecision::Approve,
            Self::RequestRevision => HitlDecision::RequestRevision,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct HitlReviewState {
    pub(crate) pulsation_id: Option<String>,
    pub(crate) title: Option<String>,
    pub(crate) input: String,
    pub(crate) generated: String,
    pub(crate) busy: bool,
    pub(crate) error: Option<String>,
}

impl HitlReviewState {
    pub(crate) fn load_checkpoint(&mut self, pulsation: Option<&Pulsation>) {
        let Some(pulsation) = pulsation else {
            self.pulsation_id = None;
            self.title = None;
            self.input.clear();
            self.generated.clear();
            return;
        };
        let stage = format!("{:?}", pulsation.stage);
        let status = format!("{:?}", pulsation.status);
        self.pulsation_id = Some(pulsation.id.clone());
        self.title = Some(format!("{stage} Review — {}", pulsation.feature_name));
        self.input = format!(
            "- Pulsation: {}\n- Stage: {stage}\n- Status: {status}\n- Source snapshot loaded from the orchestration runtime",
            pulsation.feature_name
        );
        self.generated = format!(
            "- Pulsation: {}\n- Stage: {stage}\n- Runtime status: {status}\n- Generated checkpoint draft ready for a HITL decision",
            pulsation.feature_name
        );
    }

    pub(crate) fn begin(&mut self, decision: HitlDecision) -> Option<(String, HitlDecision)> {
        if self.busy {
            return None;
        }
        let pulsation_id = self.pulsation_id.clone()?;
        self.busy = true;
        self.error = None;
        Some((pulsation_id, decision))
    }

    pub(crate) fn finish(&mut self, result: Result<(), String>) {
        self.busy = false;
        match result {
            Ok(()) => {
                self.pulsation_id = None;
                self.title = None;
                self.input.clear();
                self.generated.clear();
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

    #[gpui::test]
    fn disables_decisions_while_request_is_pending(_: &mut gpui::TestAppContext) {
        let mut state = HitlReviewState::default();
        state.load_checkpoint(Some(&Pulsation {
            id: "pulse-1".into(),
            project_id: "project-1".into(),
            feature_name: "Editor nativo".into(),
            stage: PulsationStage::Code,
            status: PulsationStatus::Checkpoint,
            created_at: "2026-08-24T00:00:00Z".into(),
            updated_at: "2026-08-24T00:00:00Z".into(),
        }));
        assert_eq!(state.title.as_deref(), Some("Code Review — Editor nativo"));
        assert!(state.input.contains("Source snapshot loaded"));
        assert!(state.generated.contains("Generated checkpoint draft"));
        assert!(state.begin(HitlDecision::Approve).is_some());
        assert!(state.busy);
        assert!(state.begin(HitlDecision::Reject).is_none());
        state.finish(Ok(()));
        assert!(!state.busy);
    }
}
