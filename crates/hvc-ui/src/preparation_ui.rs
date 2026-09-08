//! Typed human actions; inputs are scoped to the displayed revision/question set.
use super::*;
use crate::pulsations::PulsationAction;
use hvc_core::orchestration::*;

impl AppModel {
    pub(super) fn sync_preparation_inputs(&mut self, cx: &mut Context<Self>) {
        let detail = self.pulsations.read(cx).detail().cloned();
        let key = detail.as_ref().map(|d| {
            (
                d.pulsation_id.clone(),
                d.input_revision,
                d.questions.as_ref().map(|q| q.id.clone()),
            )
        });
        if key == self.preparation_scope {
            return;
        }
        self.preparation_scope = key;
        self.learning_graph_decisions.clear();
        self.question_subscriptions.clear();
        self.specification_answer_inputs.clear();
        let description = self
            .pulsations
            .read(cx)
            .active()
            .map(|p| p.feature_name.clone())
            .unwrap_or_default();
        self.description_review_input
            .update(cx, |input, cx| input.set_value(description, cx));
        self.stage_feedback_input
            .update(cx, |input, cx| input.clear(cx));
        if let Some(questions) = detail.and_then(|d| d.questions) {
            for question in questions.questions {
                let input =
                    cx.new(|cx| NativeTextInput::new("Your answer (up to 16,000 characters)", cx));
                self.question_subscriptions
                    .push(cx.observe(&input, |_, _, cx| cx.notify()));
                self.specification_answer_inputs.push((question.id, input));
            }
        }
    }

    pub(crate) fn revise_description(&mut self, cx: &mut Context<Self>) {
        let Some(detail) = self.pulsations.read(cx).detail().cloned() else {
            return;
        };
        let request = RevisePulsationDescriptionRequest {
            pulsation_id: detail.pulsation_id,
            expected_input_revision: detail.input_revision,
            description: self.description_review_input.read(cx).value().to_owned(),
        };
        if let Err(error) = request.validate() {
            self.pulsations
                .update(cx, |p, _| p.error = Some(error.to_string()));
            cx.notify();
            return;
        }
        self.run_pulsation_action(PulsationAction::Revise(request), cx);
    }

    pub(crate) fn confirm_description(&mut self, cx: &mut Context<Self>) {
        let Some(detail) = self.pulsations.read(cx).detail().cloned() else {
            return;
        };
        self.run_pulsation_action(
            PulsationAction::Confirm(ConfirmDescriptionReviewRequest {
                pulsation_id: detail.pulsation_id,
                expected_input_revision: detail.input_revision,
            }),
            cx,
        );
    }

    pub(crate) fn answer_specification(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self
            .pulsations
            .read(cx)
            .detail()
            .and_then(|d| d.questions.clone())
        else {
            return;
        };
        let request = SubmitSpecificationAnswersRequest {
            pulsation_id: pending.fence.pulsation_id.clone().expect("pulse question"),
            question_set_id: pending.id.clone(),
            checkpoint_id: pending.checkpoint_id.clone(),
            expected_input_revision: pending.fence.input_revision.expect("revision"),
            expected_state_version: pending.fence.state_version,
            answers: self
                .specification_answer_inputs
                .iter()
                .map(|(id, input)| SpecificationAnswer {
                    question_id: id.clone(),
                    answer: input.read(cx).value().into(),
                })
                .collect(),
        };
        if let Err(error) = validate_specification_answers(&request, &pending) {
            self.pulsations
                .update(cx, |p, _| p.error = Some(error.to_string()));
            cx.notify();
            return;
        }
        self.run_pulsation_action(PulsationAction::Answer(request), cx);
    }

    pub(crate) fn learning_action(&mut self, retry: bool, cx: &mut Context<Self>) {
        let Some(run) = self
            .pulsations
            .read(cx)
            .detail()
            .and_then(|d| d.learning_run.clone())
        else {
            return;
        };
        let request = LearningActionRequest {
            job_id: run.job_id,
            expected_generation: run.generation,
        };
        self.run_pulsation_action(
            if retry {
                PulsationAction::RetryLearning(request)
            } else {
                PulsationAction::DismissLearning(request)
            },
            cx,
        );
    }

    pub(crate) fn toggle_learning(&mut self, cx: &mut Context<Self>) {
        if self.settings.read(cx).busy {
            return;
        }
        let Some(backend) = self.settings_backend.clone() else {
            return;
        };
        let Some(mut settings) = self
            .settings
            .read(cx)
            .snapshot
            .as_ref()
            .map(|s| s.preparation.clone())
        else {
            return;
        };
        settings.learning_enabled = !settings.learning_enabled;
        self.run_settings_operation(Box::pin(async move {
            backend.update_preparation(settings).await?;
            Ok("Learning settings updated; no history will be reprocessed and call limits are preserved.".into())
        }), cx);
    }

    pub(crate) fn toggle_learning_refinement_mode(&mut self, cx: &mut Context<Self>) {
        if self.settings.read(cx).busy {
            return;
        }
        let Some(backend) = self.settings_backend.clone() else {
            return;
        };
        let Some(mut settings) = self
            .settings
            .read(cx)
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.preparation.clone())
        else {
            return;
        };
        settings.learning_refinement_mode = match settings.learning_refinement_mode {
            LearningRefinementMode::Observe => LearningRefinementMode::Propose,
            LearningRefinementMode::Propose => LearningRefinementMode::Observe,
        };
        self.run_settings_operation(
            Box::pin(async move {
                backend.update_preparation(settings).await?;
                Ok("Learning refinement mode updated; pending publications respect the kill switch.".into())
            }),
            cx,
        );
    }

    pub(crate) fn choose_learning_graph_decision(
        &mut self,
        proposal_id: String,
        decision: LearningGraphDecision,
        cx: &mut Context<Self>,
    ) {
        self.learning_graph_decisions.insert(proposal_id, decision);
        cx.notify();
    }

    pub(crate) fn submit_learning_graph_review(&mut self, cx: &mut Context<Self>) {
        let Some(run) = self
            .pulsations
            .read(cx)
            .detail()
            .and_then(|detail| detail.learning_run.clone())
        else {
            return;
        };
        let pending = run
            .graph_proposals
            .iter()
            .filter(|proposal| proposal.decision == "pending")
            .collect::<Vec<_>>();
        if pending.is_empty()
            || pending
                .iter()
                .any(|proposal| !self.learning_graph_decisions.contains_key(&proposal.id))
        {
            self.pulsations.update(cx, |pulsations, _| {
                pulsations.error =
                    Some("Approve or reject each pending proposal individually.".into());
            });
            cx.notify();
            return;
        }
        let decisions = pending
            .into_iter()
            .map(|proposal| ReviewLearningGraphDecision {
                proposal_id: proposal.id.clone(),
                decision: *self
                    .learning_graph_decisions
                    .get(&proposal.id)
                    .expect("pending proposal decision"),
            })
            .collect();
        self.learning_graph_decisions.clear();
        self.run_pulsation_action(
            PulsationAction::ReviewLearningGraph(ReviewLearningGraphRequest {
                job_id: run.job_id,
                expected_generation: run.generation,
                decisions,
            }),
            cx,
        );
    }

    pub(crate) fn cycle_brownfield_llm_budget(&mut self, cx: &mut Context<Self>) {
        if self.settings.read(cx).busy {
            return;
        }
        let Some(backend) = self.settings_backend.clone() else {
            return;
        };
        let Some(mut settings) = self
            .settings
            .read(cx)
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.preparation.clone())
        else {
            return;
        };
        settings.brownfield_llm_budget = match settings.brownfield_llm_budget {
            0 => 1,
            1 => 2,
            2 => 4,
            4 => 8,
            _ => 0,
        };
        self.run_settings_operation(
            Box::pin(async move {
                backend.update_preparation(settings).await?;
                Ok("Brownfield LLM budget updated; zero disables opportunistic calls.".into())
            }),
            cx,
        );
    }

    pub(crate) fn toggle_brownfield_expansion_radius(&mut self, cx: &mut Context<Self>) {
        if self.settings.read(cx).busy {
            return;
        }
        let Some(backend) = self.settings_backend.clone() else {
            return;
        };
        let Some(mut settings) = self
            .settings
            .read(cx)
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.preparation.clone())
        else {
            return;
        };
        settings.brownfield_expansion_radius = if settings.brownfield_expansion_radius == 1 {
            2
        } else {
            1
        };
        self.run_settings_operation(
            Box::pin(async move {
                backend.update_preparation(settings).await?;
                Ok("Brownfield expansion radius updated for subsequent batches.".into())
            }),
            cx,
        );
    }

    fn run_pulsation_action(&mut self, action: PulsationAction, cx: &mut Context<Self>) {
        if self.preparation_action_task.is_some() || self.lifecycle.is_disposed() {
            return;
        }
        let Some(backend) = self.pulsations_backend.clone() else {
            return;
        };
        let project = self.pulsations.read(cx).project_id.clone();
        let pulse = self.pulsations.read(cx).active_id();
        self.pulsations.update(cx, |p, _| {
            p.action_busy = true;
            p.error = None;
        });
        self.preparation_action_task = Some(cx.spawn(async move |this, cx| {
            let result = backend.action(action).await;
            let _ = this.update(cx, |model, cx| {
                model.preparation_action_task = None;
                if model.lifecycle.is_disposed() || model.pulsations.read(cx).project_id != project
                {
                    return;
                }
                let success = result.is_ok();
                model.pulsations.update(cx, |p, _| {
                    p.action_busy = false;
                    match result {
                        Ok(()) => {
                            p.notice =
                                Some("Action recorded; artifact approval remains separate.".into())
                        }
                        Err(error) => p.error = Some(error),
                    }
                });
                if success {
                    model.refresh_pulsations_with_preferred(pulse, cx);
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(crate) fn preview_artifact(
        &mut self,
        pulse: String,
        artifact: String,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.pulsations_backend.clone() else {
            return;
        };
        let project = self.pulsations.read(cx).project_id.clone();
        self.artifact_preview_page = 0;
        self.pulsations.update(cx, |p, _| {
            p.artifact_preview = Some((artifact.clone(), "Loading persisted content…".into()))
        });
        self.artifact_preview_task = Some(cx.spawn(async move |this, cx| {
            let result = backend
                .artifact_content(pulse.clone(), artifact.clone())
                .await;
            let _ = this.update(cx, |model, cx| {
                if model.lifecycle.is_disposed()
                    || model.pulsations.read(cx).project_id != project
                    || model.pulsations.read(cx).active_id().as_deref() != Some(&pulse)
                {
                    return;
                }
                model.pulsations.update(cx, |p, _| {
                    if p.artifact_preview
                        .as_ref()
                        .is_none_or(|(id, _)| id != &artifact)
                    {
                        return;
                    }
                    match result {
                        Ok(content) => p.artifact_preview = Some((artifact, content)),
                        Err(error) => {
                            p.artifact_preview = None;
                            p.error = Some(error);
                        }
                    }
                });
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(crate) fn open_specification_conversation(&mut self, cx: &mut Context<Self>) {
        let Some(detail) = self.pulsations.read(cx).detail().cloned() else {
            return;
        };
        let Some(questions) = detail.questions else {
            return;
        };
        self.select_view(AppView::Chat, cx);
        self.hydrate_chat_project(detail.project_id, Some(questions.conversation_id), cx);
    }

    pub(super) fn fill_preparation_threshold(
        &mut self,
        snapshot: &crate::settings::SettingsSnapshot,
        cx: &mut Context<Self>,
    ) {
        if self.context_limit_input.read(cx).value().is_empty() {
            self.context_limit_input.update(cx, |input, cx| {
                input.set_value(snapshot.config.context_chars_limit.to_string(), cx)
            });
        }
        if self.preparation_threshold_input.read(cx).value().is_empty() {
            self.preparation_threshold_input.update(cx, |input, cx| {
                input.set_value(
                    snapshot.preparation.vagueness_threshold.get().to_string(),
                    cx,
                )
            });
        }
    }

    pub(crate) fn save_preparation_settings(&mut self, toggle: bool, cx: &mut Context<Self>) {
        let Some(backend) = self.settings_backend.clone() else {
            return;
        };
        let Some(mut settings) = self
            .settings
            .read(cx)
            .snapshot
            .as_ref()
            .map(|s| s.preparation.clone())
        else {
            return;
        };
        if self.settings.read(cx).busy {
            return;
        }
        if toggle {
            settings.preparation_enabled = !settings.preparation_enabled;
        } else {
            let threshold = self
                .preparation_threshold_input
                .read(cx)
                .value()
                .trim()
                .parse::<u8>()
                .ok()
                .and_then(|n| VaguenessScore::try_from(n).ok());
            let Some(threshold) = threshold else {
                self.notices.push(
                    HeaderNoticeKind::Warning,
                    "Use an integer threshold from 0 to 100.",
                );
                cx.notify();
                return;
            };
            settings.vagueness_threshold = threshold;
        }
        self.run_settings_operation(Box::pin(async move{backend.update_preparation(settings).await?;Ok("Preparation saved; the threshold applies to the next evaluation. Pending reviews still require human action.".into())}),cx);
    }

    pub(crate) fn save_context_limit(&mut self, cx: &mut Context<Self>) {
        if self.settings.read(cx).busy {
            return;
        }
        let limit = self
            .context_limit_input
            .read(cx)
            .value()
            .trim()
            .parse::<usize>()
            .ok()
            .filter(|n| (2000..=80000).contains(n));
        let Some(limit) = limit else {
            self.notices.push(
                HeaderNoticeKind::Warning,
                "Use between 2,000 and 80,000 context characters.",
            );
            cx.notify();
            return;
        };
        let mut request = runtime_config_update();
        request.context_chars_limit = Some(limit);
        self.update_runtime_config(
            request,
            "Context budget saved; approved artifacts are not truncated.",
            cx,
        );
    }
}
