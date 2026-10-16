//! Resolution modal actions (ADR-022 clarification loop, P4/A4.1): the chip
//! list, the accordion, and the free-form Companion conversation used to turn
//! a Specification clarification question into a `FILES_NEW:`/`FILES_MOD:`/
//! `FILES:` answer. Reuses `chat_backend` for the actual turn machinery --
//! this module owns no LLM dispatch of its own, only the chip/conversation
//! state and when to poll for a turn's completion.
use super::*;
use hvc_core::orchestration::{ClarificationAnswerV1, SubmitSpecificationAnswersRequest};

/// Host-authored instruction sent on the developer's behalf when they ask
/// the Companion to turn the conversation so far into a confirmable answer.
/// Mirrors the tag contract `orchestration::composition::recipes::parse_clarification_tags`
/// already reads from a submitted answer's `text_answer`.
const SYNTHESIS_PROMPT: &str = "Based on this conversation, write the final answer to the Specification clarification question. Start with a short prose summary, then list file paths using this exact tag format, one per line: `FILES_NEW:` for files that do not exist yet, `FILES_MOD:` for files that already exist and will change, `FILES:` for a path you have not determined new-vs-modified. Separate multiple paths on the same tag line with commas. Only cite paths you are reasonably confident about.";

/// Host-authored first message for a brand-new resolution conversation: the
/// chip's question/rationale text is only ever rendered in the modal's own
/// header, never sent to the Companion on its own, so without this the
/// Companion has nothing to go on and free-associates "References"/"Handles"
/// as if they were Obsidian references or domain entities instead of the
/// pipeline's own file-ownership bookkeeping.
fn opening_briefing(chip: &hvc_core::orchestration::SpecificationQuestionSet) -> String {
    let mut message = String::from(
        "I'm the developer resolving a clarification an automated code-generation pipeline raised while implementing this feature. In this pipeline, \"References table\" and \"Handles\" mean its own internal bookkeeping of which file paths a stage is allowed to read or write -- not Obsidian references, domain entities, or anything else. Here is the open question:\n\n",
    );
    for question in &chip.questions {
        message.push_str("Question: ");
        message.push_str(&question.question);
        message.push_str("\nWhy it's asking: ");
        message.push_str(&question.rationale);
        message.push_str("\n\n");
    }
    message.push_str(
        "Help me figure out the right file path(s) to confirm. Once we agree, I'll ask you to write the final answer using FILES_NEW:/FILES_MOD:/FILES: tags.",
    );
    message
}

impl AppModel {
    pub(crate) fn refresh_resolution_chips(&mut self, cx: &mut Context<Self>) {
        let Some(backend) = self.pulsations_backend.clone() else {
            return;
        };
        let Some(project_id) = self
            .workspace
            .read(cx)
            .project_id
            .as_ref()
            .map(ToString::to_string)
        else {
            return;
        };
        if self.resolution_chips_in_flight {
            return;
        }
        self.resolution_chips_in_flight = true;
        self.resolution_chips_task = Some(cx.spawn(async move |this, cx| {
            let result = backend.resolution_chips(project_id.clone()).await;
            let _ = this.update(cx, |model, cx| {
                model.resolution_chips_in_flight = false;
                if model.lifecycle.is_disposed()
                    || model
                        .workspace
                        .read(cx)
                        .project_id
                        .as_ref()
                        .map(AsRef::as_ref)
                        != Some(project_id.as_str())
                {
                    return;
                }
                match result {
                    Ok(chips) => model.resolution.update(cx, |resolution, _| {
                        resolution.apply_chips(chips);
                    }),
                    Err(error) => model.resolution.update(cx, |resolution, _| {
                        resolution.chips_loading = false;
                        resolution.error = Some(error);
                    }),
                }
                cx.notify();
            });
        }));
    }

    /// Accordion toggle: expanding a chip collapses whichever one was open
    /// and lazily opens its resolution conversation; expanding the one
    /// that's already open collapses it instead.
    pub(crate) fn toggle_resolution_chip(
        &mut self,
        question_set_id: String,
        cx: &mut Context<Self>,
    ) {
        let already_expanded =
            self.resolution.read(cx).expanded.as_deref() == Some(question_set_id.as_str());
        self.resolution_draft_input
            .update(cx, |input, cx| input.clear(cx));
        self.resolution_synthesis_input
            .update(cx, |input, cx| input.clear(cx));
        if already_expanded {
            self.resolution.update(cx, |resolution, _| {
                resolution.expanded = None;
                resolution.conversation = None;
            });
            cx.notify();
            return;
        }
        self.resolution.update(cx, |resolution, _| {
            resolution.expanded = Some(question_set_id.clone());
            resolution.conversation = None;
        });
        cx.notify();
        self.load_resolution_conversation(question_set_id, cx);
    }

    fn load_resolution_conversation(&mut self, question_set_id: String, cx: &mut Context<Self>) {
        let Some(pulsations_backend) = self.pulsations_backend.clone() else {
            return;
        };
        let Some(chat_backend) = self.chat_backend.clone() else {
            return;
        };
        let Some(project_id) = self
            .workspace
            .read(cx)
            .project_id
            .as_ref()
            .map(ToString::to_string)
        else {
            return;
        };
        self.resolution_conversation_task = Some(cx.spawn(async move |this, cx| {
            let conversation_id = pulsations_backend
                .ensure_resolution_conversation(project_id.clone(), question_set_id.clone())
                .await;
            let conversation_id = match conversation_id {
                Ok(id) => id,
                Err(error) => {
                    let _ = this.update(cx, |model, cx| {
                        model.resolution.update(cx, |resolution, _| {
                            resolution.error = Some(error);
                        });
                        cx.notify();
                    });
                    return;
                }
            };
            let snapshot = chat_backend
                .load_conversation(conversation_id.clone())
                .await;
            let _ = this.update(cx, |model, cx| {
                if model.resolution.read(cx).expanded.as_deref() != Some(question_set_id.as_str()) {
                    return;
                }
                let mut needs_briefing = false;
                match snapshot {
                    Ok(snapshot) => {
                        needs_briefing = snapshot.messages.is_empty();
                        model.resolution.update(cx, |resolution, _| {
                            resolution.conversation =
                                Some(crate::resolution::ResolutionConversation {
                                    conversation_id,
                                    messages: snapshot.messages,
                                    synthesized_draft: None,
                                    pending_synthesis_generation_id: None,
                                    busy: false,
                                });
                            resolution.error = None;
                        });
                    }
                    Err(error) => model.resolution.update(cx, |resolution, _| {
                        resolution.error = Some(error.presentable());
                    }),
                }
                if needs_briefing
                    && let Some(chip) = model.resolution.read(cx).chip(&question_set_id).cloned()
                {
                    model.send_resolution_content(opening_briefing(&chip), false, cx);
                }
                cx.notify();
            });
        }));
    }

    fn send_resolution_content(
        &mut self,
        content: String,
        is_synthesis: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.chat_backend.clone() else {
            return;
        };
        let content = content.trim().to_string();
        if content.is_empty() {
            return;
        }
        let Some(conversation_id) = self
            .resolution
            .read(cx)
            .conversation
            .as_ref()
            .filter(|conversation| !conversation.busy && !conversation.is_streaming())
            .map(|conversation| conversation.conversation_id.clone())
        else {
            return;
        };
        self.resolution.update(cx, |resolution, _| {
            if let Some(conversation) = &mut resolution.conversation {
                conversation.busy = true;
            }
        });
        cx.notify();
        self.resolution_conversation_task = Some(cx.spawn(async move |this, cx| {
            let result = backend
                .send_message(conversation_id.clone(), content, None)
                .await;
            let _ = this.update(cx, |model, cx| {
                let mut newly_synthesized = None;
                model.resolution.update(cx, |resolution, _| {
                    let Some(conversation) = &mut resolution.conversation else {
                        return;
                    };
                    conversation.busy = false;
                    if conversation.conversation_id != conversation_id {
                        return;
                    }
                    match result {
                        Ok((started, snapshot)) => {
                            conversation.messages = snapshot.messages;
                            if is_synthesis {
                                if let Some(content) = conversation
                                    .completed_content_for_generation(&started.generation_id)
                                    .map(str::to_string)
                                {
                                    conversation.synthesized_draft = Some(content.clone());
                                    newly_synthesized = Some(content);
                                } else {
                                    conversation.pending_synthesis_generation_id =
                                        Some(started.generation_id);
                                }
                            }
                        }
                        Err(error) => resolution.error = Some(error.presentable()),
                    }
                });
                if let Some(content) = newly_synthesized {
                    model
                        .resolution_synthesis_input
                        .update(cx, |input, cx| input.set_value(content, cx));
                }
                cx.notify();
            });
        }));
    }

    pub(crate) fn send_resolution_message(&mut self, cx: &mut Context<Self>) {
        let content = self
            .resolution_draft_input
            .read(cx)
            .value()
            .trim()
            .to_string();
        if content.is_empty() {
            return;
        }
        self.resolution_draft_input
            .update(cx, |input, cx| input.clear(cx));
        self.send_resolution_content(content, false, cx);
    }

    pub(crate) fn synthesize_resolution_reply(&mut self, cx: &mut Context<Self>) {
        self.send_resolution_content(SYNTHESIS_PROMPT.to_string(), true, cx);
    }

    pub(crate) fn update_resolution_synthesis_draft(&mut self, cx: &mut Context<Self>) {
        let value = self.resolution_synthesis_input.read(cx).value().to_string();
        self.resolution.update(cx, |resolution, _| {
            if let Some(conversation) = &mut resolution.conversation {
                conversation.synthesized_draft = Some(value);
            }
        });
    }

    /// Called from the 1.2s background poll (`start_lifecycle`): the only
    /// place that observes whether a resolution turn already completed,
    /// since this module deliberately does not subscribe to the live chat
    /// event stream (see the module doc comment).
    pub(crate) fn poll_resolution_turn_if_pending(&mut self, cx: &mut Context<Self>) {
        if self.resolution_turn_in_flight {
            return;
        }
        let Some(backend) = self.chat_backend.clone() else {
            return;
        };
        let Some((conversation_id, generation_id)) = self
            .resolution
            .read(cx)
            .conversation
            .as_ref()
            .and_then(|conversation| {
                conversation
                    .active_generation_id()
                    .map(|generation_id| (conversation.conversation_id.clone(), generation_id))
            })
        else {
            return;
        };
        self.resolution_turn_in_flight = true;
        self.resolution_turn_task = Some(cx.spawn(async move |this, cx| {
            let result = backend.turn_snapshot(generation_id.clone()).await;
            let _ = this.update(cx, |model, cx| {
                model.resolution_turn_in_flight = false;
                let mut newly_synthesized = None;
                model.resolution.update(cx, |resolution, _| {
                    let Some(conversation) = &mut resolution.conversation else {
                        return;
                    };
                    if conversation.conversation_id != conversation_id {
                        return;
                    }
                    if let Ok(snapshot) = result {
                        let completed = snapshot.message.status == "completed";
                        conversation
                            .messages
                            .retain(|message| message.id != snapshot.message.id);
                        conversation.messages.push(snapshot.message);
                        conversation.messages.sort_by_key(|message| message.ordinal);
                        if completed
                            && conversation.pending_synthesis_generation_id.as_deref()
                                == Some(generation_id.as_str())
                            && let Some(content) = conversation
                                .completed_content_for_generation(&generation_id)
                                .map(str::to_string)
                        {
                            conversation.synthesized_draft = Some(content.clone());
                            conversation.pending_synthesis_generation_id = None;
                            newly_synthesized = Some(content);
                        }
                    }
                });
                if let Some(content) = newly_synthesized {
                    model
                        .resolution_synthesis_input
                        .update(cx, |input, cx| input.set_value(content, cx));
                }
                cx.notify();
            });
        }));
    }

    pub(crate) fn confirm_resolution_answer(&mut self, cx: &mut Context<Self>) {
        let Some(backend) = self.pulsations_backend.clone() else {
            return;
        };
        let Some(question_set_id) = self.resolution.read(cx).expanded.clone() else {
            return;
        };
        let Some(chip) = self.resolution.read(cx).chip(&question_set_id).cloned() else {
            return;
        };
        let Some(text_answer) = self
            .resolution
            .read(cx)
            .conversation
            .as_ref()
            .and_then(|conversation| conversation.synthesized_draft.clone())
            .filter(|draft| !draft.trim().is_empty())
        else {
            return;
        };
        let Some(pulsation_id) = chip.fence.pulsation_id.clone() else {
            return;
        };
        let Some(expected_input_revision) = chip.fence.input_revision else {
            return;
        };
        let request = SubmitSpecificationAnswersRequest {
            pulsation_id,
            question_set_id: chip.id.clone(),
            checkpoint_id: chip.checkpoint_id.clone(),
            expected_input_revision,
            expected_state_version: chip.fence.state_version,
            answers: chip
                .questions
                .iter()
                .map(|question| ClarificationAnswerV1 {
                    question_id: question.id.clone(),
                    text_answer: text_answer.clone(),
                    planned_paths: Vec::new(),
                    modified_paths: Vec::new(),
                })
                .collect(),
        };
        self.resolution.update(cx, |resolution, _| {
            if let Some(conversation) = &mut resolution.conversation {
                conversation.busy = true;
            }
        });
        cx.notify();
        self.resolution_action_task = Some(cx.spawn(async move |this, cx| {
            let result = backend
                .action(crate::pulsations::PulsationAction::Answer(request))
                .await;
            let _ = this.update(cx, |model, cx| {
                model.resolution.update(cx, |resolution, _| {
                    if let Some(conversation) = &mut resolution.conversation {
                        conversation.busy = false;
                    }
                    match result {
                        Ok(()) => {
                            resolution.chips.retain(|chip| chip.id != question_set_id);
                            if resolution.expanded.as_deref() == Some(question_set_id.as_str()) {
                                resolution.expanded = None;
                                resolution.conversation = None;
                            }
                        }
                        Err(error) => resolution.error = Some(error),
                    }
                });
                cx.notify();
            });
        }));
    }
}
