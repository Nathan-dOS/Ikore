use ikore_core::chat_service::ChatMessage;
use ikore_core::orchestration::SpecificationQuestionSet;

/// One resolution conversation, opened lazily the first time its chip is
/// expanded (ADR-022 clarification loop, P4/A4.1). Distinct from the
/// host-rendered `specification_conversations` row the chip's own
/// `SpecificationQuestionSet.conversation_id` points at -- this is a real
/// Companion turn history, keyed by `question_set_id` on the host side via
/// `ensure_resolution_conversation`.
#[derive(Debug, Clone, Default)]
pub(crate) struct ResolutionConversation {
    pub(crate) conversation_id: String,
    pub(crate) messages: Vec<ChatMessage>,
    /// The editable "Reply to the agent" draft, populated once the developer
    /// asks the Companion to synthesize a `FILES_NEW:`/`FILES_MOD:`/`FILES:`
    /// reply. `None` until the first synthesis reply lands.
    pub(crate) synthesized_draft: Option<String>,
    /// The generation started by `synthesize_resolution_reply`, so the poll
    /// loop knows to route THIS turn's completed content into
    /// `synthesized_draft` rather than an ordinary conversational reply.
    pub(crate) pending_synthesis_generation_id: Option<String>,
    pub(crate) busy: bool,
}

impl ResolutionConversation {
    pub(crate) fn is_streaming(&self) -> bool {
        self.messages.iter().any(|message| {
            message.role == "assistant"
                && matches!(message.status.as_str(), "pending" | "streaming")
        })
    }

    pub(crate) fn active_generation_id(&self) -> Option<String> {
        self.messages
            .iter()
            .rev()
            .find(|message| {
                message.role == "assistant"
                    && matches!(message.status.as_str(), "pending" | "streaming")
            })
            .and_then(|message| message.generation_id.clone())
    }

    /// A completed assistant reply for the exact generation started by
    /// `synthesize_resolution_reply`, whether it landed already in the
    /// response to `send_message` itself or was picked up later by the poll.
    pub(crate) fn completed_content_for_generation(&self, generation_id: &str) -> Option<&str> {
        self.messages
            .iter()
            .find(|message| {
                message.role == "assistant"
                    && message.status == "completed"
                    && message.generation_id.as_deref() == Some(generation_id)
            })
            .map(|message| message.content.as_str())
    }
}

#[derive(Debug, Default)]
pub struct ResolutionModel {
    /// Every clarification checkpoint currently open across the whole
    /// project, regardless of which pulsation is selected in Pulsations --
    /// the whole point of the global chip list (A4.1).
    pub(crate) chips: Vec<SpecificationQuestionSet>,
    /// Accordion: at most one chip expanded at a time.
    pub(crate) expanded: Option<String>,
    pub(crate) conversation: Option<ResolutionConversation>,
    pub(crate) chips_loading: bool,
    pub(crate) error: Option<String>,
}

impl ResolutionModel {
    pub(crate) fn chip(&self, question_set_id: &str) -> Option<&SpecificationQuestionSet> {
        self.chips.iter().find(|chip| chip.id == question_set_id)
    }

    /// Replaces the chip list, dropping the expanded conversation if its
    /// checkpoint is no longer open (answered, or the pulsation moved on).
    pub(crate) fn apply_chips(&mut self, chips: Vec<SpecificationQuestionSet>) {
        if let Some(expanded) = &self.expanded
            && !chips.iter().any(|chip| &chip.id == expanded)
        {
            self.expanded = None;
            self.conversation = None;
        }
        self.chips = chips;
        self.chips_loading = false;
        self.error = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ikore_core::orchestration::{ExecutionFence, SpecificationQuestion};

    fn fence(pulsation_id: &str) -> ExecutionFence {
        ExecutionFence {
            project_id: "project-1".into(),
            pulsation_id: Some(pulsation_id.into()),
            input_revision: Some(1),
            job_id: "job-1".into(),
            generation: 1,
            state_version: 1,
        }
    }

    fn chip(id: &str, pulsation_id: &str) -> SpecificationQuestionSet {
        SpecificationQuestionSet {
            id: id.into(),
            checkpoint_id: format!("checkpoint-{id}"),
            fence: fence(pulsation_id),
            conversation_id: format!("host-conv-{id}"),
            questions: vec![SpecificationQuestion {
                id: format!("question-{id}"),
                question: "Where should this file live?".into(),
                rationale: "No planned-destination handle exists yet.".into(),
            }],
        }
    }

    fn message(role: &str, status: &str, content: &str, generation_id: &str) -> ChatMessage {
        ChatMessage {
            id: format!("message-{generation_id}"),
            conversation_id: "conversation-1".into(),
            ordinal: 1,
            role: role.into(),
            content: content.into(),
            status: status.into(),
            execution_id: Some(format!("execution-{generation_id}")),
            generation_id: Some(generation_id.into()),
            last_sequence: 1,
            input_tokens: None,
            output_tokens: None,
            estimated_cost_usd: None,
            error_code: None,
            error_message: None,
            target_label: None,
            created_at: "now".into(),
            updated_at: "now".into(),
        }
    }

    #[test]
    fn applying_chips_collapses_the_expanded_conversation_once_its_checkpoint_is_answered() {
        let mut model = ResolutionModel {
            chips: vec![chip("set-a", "pulse-a")],
            expanded: Some("set-a".into()),
            conversation: Some(ResolutionConversation {
                conversation_id: "conversation-1".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        model.apply_chips(Vec::new());
        assert!(model.expanded.is_none());
        assert!(model.conversation.is_none());
    }

    #[test]
    fn applying_chips_keeps_the_expanded_conversation_when_its_checkpoint_survives() {
        let mut model = ResolutionModel {
            chips: vec![chip("set-a", "pulse-a")],
            expanded: Some("set-a".into()),
            conversation: Some(ResolutionConversation {
                conversation_id: "conversation-1".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        model.apply_chips(vec![chip("set-a", "pulse-a"), chip("set-b", "pulse-b")]);
        assert_eq!(model.expanded.as_deref(), Some("set-a"));
        assert!(model.conversation.is_some());
    }

    #[test]
    fn active_generation_id_only_reports_an_in_flight_assistant_turn() {
        let mut conversation = ResolutionConversation::default();
        conversation
            .messages
            .push(message("user", "completed", "hi", "gen-user"));
        assert_eq!(conversation.active_generation_id(), None);
        conversation
            .messages
            .push(message("assistant", "streaming", "", "gen-1"));
        assert_eq!(
            conversation.active_generation_id().as_deref(),
            Some("gen-1")
        );
        assert!(conversation.is_streaming());
    }

    #[test]
    fn completed_content_for_generation_matches_only_the_requested_turn() {
        let mut conversation = ResolutionConversation::default();
        conversation
            .messages
            .push(message("assistant", "completed", "first answer", "gen-1"));
        conversation
            .messages
            .push(message("assistant", "streaming", "", "gen-2"));
        assert_eq!(
            conversation.completed_content_for_generation("gen-1"),
            Some("first answer")
        );
        assert_eq!(conversation.completed_content_for_generation("gen-2"), None);
    }
}
