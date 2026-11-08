//! Feature Agent questions answered in the floating dock without changing the
//! full Forge selection or its in-progress composer.

use gpui::{AnyElement, Context, IntoElement, SharedString, div, prelude::*};
use hvc_core::feature_forge::{AnswerFeatureQuestionsRequest, FeatureQuestionAnswer};
use hvc_core::orchestration::decisions::{PendingDecision, PendingForgeQuestions};

use crate::{AppModel, theme};

fn forge_answer_key(decision: &str, index: usize) -> String {
    format!("forge-answer:{decision}:{index}")
}

fn button(
    id: String,
    label: String,
    enabled: bool,
    cx: &mut Context<AppModel>,
    click: impl Fn(&mut AppModel, &mut Context<AppModel>) + 'static,
) -> AnyElement {
    let selector = id.clone();
    div()
        .id(SharedString::from(id))
        .debug_selector(move || selector.clone())
        .tab_index(0)
        .px_2()
        .py_1()
        .rounded_md()
        .text_xs()
        .border_1()
        .border_color(theme::border())
        .bg(theme::surface_raised())
        .text_color(if enabled {
            theme::text()
        } else {
            theme::text_subtle()
        })
        .when(enabled, |control| {
            control
                .cursor_pointer()
                .hover(|style| style.border_color(theme::accent()))
                .on_click(cx.listener(move |model, _, _, cx| {
                    click(model, cx);
                    cx.stop_propagation();
                }))
        })
        .child(label)
        .into_any_element()
}

pub(crate) fn question_card(
    model: &AppModel,
    decision: &PendingDecision,
    conversation: Option<&str>,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let mut card = div()
        .p_2()
        .rounded_md()
        .border_1()
        .border_color(theme::accent())
        .bg(theme::accent_soft())
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child("Feature Agent"),
        );
    let Some(source) = &decision.forge_questions else {
        return card
            .child("Open Feature Forge to review the current questions.")
            .into_any_element();
    };
    let busy = model.companion_dock.action_busy.contains(&decision.id);
    for (index, question) in source.questions.iter().enumerate() {
        let selector = format!("dock-forge-question-{}-{index}", decision.id);
        card = card.child(
            div()
                .debug_selector(move || selector.clone())
                .text_sm()
                .child(question.clone()),
        );
        if let Some(detail) = source
            .question_details
            .iter()
            .find(|detail| detail.question_index == index)
        {
            if !detail.explanation.is_empty() {
                card = card.child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(detail.explanation.clone()),
                );
            }
            for (option_index, option) in detail.options.iter().enumerate() {
                let decision = decision.clone();
                let label = if detail.recommended_option == Some(option_index) {
                    format!("{} · Recommended", option.label)
                } else {
                    option.label.clone()
                };
                card = card.child(button(
                    format!("dock-forge-option-{}-{index}-{option_index}", decision.id),
                    label,
                    !busy,
                    cx,
                    move |model, cx| {
                        model.choose_companion_forge_option(&decision, index, option_index, cx)
                    },
                ));
                if !option.explanation.is_empty() {
                    card = card.child(
                        div()
                            .text_xs()
                            .text_color(theme::text_muted())
                            .child(option.explanation.clone()),
                    );
                }
            }
        }
        let key = forge_answer_key(&decision.id, index);
        if model
            .companion_dock
            .form_draft_is_stale(&key, decision.state_version)
        {
            card = card.child(div().text_xs().text_color(theme::warning()).child(
                "This answer was drafted for an earlier revision. Review it before submitting.",
            ));
        }
        if let Some(input) = model.companion_dock.forms.get(&key) {
            card = card.child(input.clone());
        }
        if let Some(conversation) = conversation {
            let conversation = conversation.to_owned();
            card = card.child(button(
                format!("dock-forge-use-{key}"),
                "Use latest Companion reply as draft".into(),
                !busy,
                cx,
                move |model, cx| model.use_companion_answer(&conversation, &key, cx),
            ));
        }
    }
    let ready = !busy && model.collect_companion_forge_answers(decision, cx).is_ok();
    let submit = decision.clone();
    let open = decision.clone();
    card.child(button(
        format!("dock-forge-submit-{}", decision.id),
        if busy {
            "Sending answers…"
        } else {
            "Submit answers"
        }
        .into(),
        ready,
        cx,
        move |model, cx| model.submit_companion_forge_answers(submit.clone(), cx),
    ))
    .child(button(
        format!("dock-forge-open-{}", decision.id),
        "Open full Feature Forge".into(),
        true,
        cx,
        move |model, cx| model.open_companion_decision_detail(open.clone(), cx),
    ))
    .into_any_element()
}

impl AppModel {
    pub(crate) fn ensure_companion_forge_forms(
        &mut self,
        decision: &PendingDecision,
        cx: &mut Context<Self>,
    ) {
        if let Some(source) = &decision.forge_questions {
            for index in 0..source.questions.len() {
                self.ensure_companion_form(
                    forge_answer_key(&decision.id, index),
                    decision,
                    "Your answer",
                    cx,
                );
            }
        }
    }

    fn current_companion_forge_source(
        &self,
        decision: &PendingDecision,
    ) -> Option<&PendingForgeQuestions> {
        if self.companion_dock.project_id.as_deref() != Some(decision.project_id.as_str()) {
            return None;
        }
        self.companion_dock
            .pending
            .iter()
            .find(|current| {
                current.id == decision.id && current.state_version == decision.state_version
            })
            .and_then(|current| current.forge_questions.as_ref())
            .filter(|current| decision.forge_questions.as_ref() == Some(*current))
    }

    pub(crate) fn choose_companion_forge_option(
        &mut self,
        decision: &PendingDecision,
        question_index: usize,
        option_index: usize,
        cx: &mut Context<Self>,
    ) {
        if self.companion_dock.shutting_down
            || self.companion_dock.action_busy.contains(&decision.id)
        {
            return;
        }
        let label = self
            .current_companion_forge_source(decision)
            .and_then(|source| {
                source
                    .question_details
                    .iter()
                    .find(|detail| detail.question_index == question_index)
            })
            .and_then(|detail| detail.options.get(option_index))
            .map(|option| option.label.clone());
        if let Some(label) = label {
            self.ensure_companion_forge_forms(decision, cx);
            if let Some(input) = self
                .companion_dock
                .forms
                .get(&forge_answer_key(&decision.id, question_index))
            {
                input.update(cx, |input, cx| input.set_value(label, cx));
            }
            cx.notify();
        }
    }

    fn collect_companion_forge_answers(
        &self,
        decision: &PendingDecision,
        cx: &gpui::App,
    ) -> Result<Vec<FeatureQuestionAnswer>, String> {
        let source = self
            .current_companion_forge_source(decision)
            .ok_or("The Feature Agent questions changed. Review the current card.")?;
        let mut answers = Vec::new();
        for index in 0..source.questions.len() {
            let text = self
                .companion_dock
                .forms
                .get(&forge_answer_key(&decision.id, index))
                .map(|input| input.read(cx).value().to_owned())
                .unwrap_or_default();
            if text.trim().is_empty() {
                return Err("Answer every current question before submitting.".into());
            }
            answers.push(FeatureQuestionAnswer {
                question_index: index,
                text_answer: text,
            });
        }
        Ok(answers)
    }

    pub(crate) fn submit_companion_forge_answers(
        &mut self,
        decision: PendingDecision,
        cx: &mut Context<Self>,
    ) {
        if self.companion_dock.shutting_down
            || self.companion_dock.action_busy.contains(&decision.id)
        {
            return;
        }
        let answers = match self.collect_companion_forge_answers(&decision, cx) {
            Ok(answers) => answers,
            Err(error) => {
                self.companion_dock.error = Some(error);
                cx.notify();
                return;
            }
        };
        let Some(source) = decision.forge_questions.clone() else {
            return;
        };
        let Some(core) = self.core.clone() else {
            return;
        };
        let sent = answers
            .iter()
            .map(|answer| {
                (
                    forge_answer_key(&decision.id, answer.question_index),
                    answer.text_answer.clone(),
                )
            })
            .collect::<Vec<_>>();
        let request = AnswerFeatureQuestionsRequest {
            project_id: decision.project_id.clone(),
            feature_id: source.feature_id,
            expected_revision: source.feature_revision,
            expected_turn_id: source.turn_id,
            request_id: uuid::Uuid::new_v4().to_string(),
            answers,
        };
        let runtime = core.runtime_handle().clone();
        let id = decision.id.clone();
        let project = decision.project_id;
        self.companion_dock.action_busy.insert(id.clone());
        self.companion_dock.error = None;
        let task_key = format!("action:forge:{id}");
        let task = cx.spawn(async move |this, cx| {
            let result = runtime
                .spawn_blocking(move || core.feature_agent().answer_questions(request))
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result.map_err(|error| error.to_string()));
            let _ = this.update(cx, |model, cx| {
                model.finish_companion_forge_admission(&project, &id, sent, result.map(|_| ()), cx);
            });
        });
        self.companion_dock.save_tasks.insert(task_key, task);
        cx.notify();
    }
    pub(crate) fn finish_companion_forge_admission(
        &mut self,
        project: &str,
        id: &str,
        sent: Vec<(String, String)>,
        result: Result<(), String>,
        cx: &mut Context<Self>,
    ) {
        if self.companion_dock.project_id.as_deref() != Some(project) {
            return;
        }
        self.companion_dock.action_busy.remove(id);
        match result {
            Ok(()) => {
                for (key, sent) in sent {
                    if let Some(input) = self.companion_dock.forms.get(&key)
                        && input.read(cx).value() == sent
                    {
                        input.update(cx, |input, cx| input.set_value("", cx));
                    }
                }
                self.companion_dock
                    .pending
                    .retain(|pending| pending.id != id);
                if !self.companion_dock.shutting_down {
                    self.refresh_forge(cx);
                }
            }
            Err(error) => self.companion_dock.error = Some(error),
        }
        self.invalidate_companion_decisions();
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Modifiers, TestAppContext, px, size};
    use hvc_core::feature_forge::{FeatureQuestion, FeatureQuestionOption};
    use hvc_core::orchestration::decisions::{PendingDecisionAction, PendingDecisionKind};

    fn fixture() -> PendingDecision {
        PendingDecision {
            id: "forge:feature-a:3:turn-a".into(),
            project_id: "project-a".into(),
            kind: PendingDecisionKind::ForgeQuestions,
            source_id: "feature-a".into(),
            pulsation_id: None,
            checkpoint_id: None,
            feature_id: Some("feature-a".into()),
            input_revision: Some(3),
            state_version: 3,
            stage: None,
            title: "Export format".into(),
            description: "Choose the format and audience.".into(),
            actions: vec![PendingDecisionAction::OpenForge],
            question_set: None,
            forge_questions: Some(PendingForgeQuestions {
                feature_id: "feature-a".into(),
                feature_revision: 3,
                turn_id: "turn-a".into(),
                questions: vec!["Which format?".into(), "Who reads it?".into()],
                question_details: vec![FeatureQuestion {
                    question_index: 0,
                    explanation: "Choose the consumer's format.".into(),
                    options: vec![
                        FeatureQuestionOption {
                            label: "JSON".into(),
                            explanation: "Preserves structure.".into(),
                        },
                        FeatureQuestionOption {
                            label: "CSV".into(),
                            explanation: "Opens in spreadsheets.".into(),
                        },
                    ],
                    recommended_option: Some(0),
                }],
            }),
        }
    }

    #[gpui::test]
    fn selecting_a_dock_option_prefills_only_its_editable_answer(cx: &mut TestAppContext) {
        let model = cx.new(AppModel::new_for_test);
        let decision = fixture();
        model.update(cx, |model, cx| {
            model.companion_dock.project_id = Some("project-a".into());
            model.companion_dock.pending.push(decision.clone());
            model.forge.selected_feature = Some("unrelated-feature".into());
            model.forge.message.update(cx, |input, cx| {
                input.set_value("Unsent full Forge conversation", cx)
            });
            model.ensure_companion_forge_forms(&decision, cx);
            model.choose_companion_forge_option(&decision, 0, 0, cx);
        });
        cx.run_until_parked();
        model.read_with(cx, |model, cx| {
            assert_eq!(
                model.companion_dock.forms[&forge_answer_key(&decision.id, 0)]
                    .read(cx)
                    .value(),
                "JSON"
            );
            assert_eq!(
                model.companion_dock.forms[&forge_answer_key(&decision.id, 1)]
                    .read(cx)
                    .value(),
                ""
            );
            assert_eq!(
                model.forge.selected_feature.as_deref(),
                Some("unrelated-feature")
            );
            assert_eq!(
                model.forge.message.read(cx).value(),
                "Unsent full Forge conversation"
            );
            assert!(
                model.companion_dock.action_busy.is_empty(),
                "an option click must never send"
            );
            assert!(
                model
                    .collect_companion_forge_answers(&decision, cx)
                    .is_err(),
                "all questions need answers"
            );
        });
        model.update(cx, |model, cx| {
            model.companion_dock.forms[&forge_answer_key(&decision.id, 0)]
                .update(cx, |input, cx| {
                    input.set_value("JSON with a custom envelope", cx)
                });
            model.companion_dock.forms[&forge_answer_key(&decision.id, 1)]
                .update(cx, |input, cx| input.set_value("Our accounting team", cx));
        });
        cx.run_until_parked();
        model.read_with(cx, |model, cx| {
            let answers = model
                .collect_companion_forge_answers(&decision, cx)
                .unwrap();
            assert_eq!(answers[0].text_answer, "JSON with a custom envelope");
            assert_eq!(answers[1].text_answer, "Our accounting team");
        });
    }

    #[gpui::test]
    fn stale_dock_option_does_not_overwrite_a_saved_answer(cx: &mut TestAppContext) {
        let model = cx.new(AppModel::new_for_test);
        let decision = fixture();
        model.update(cx, |model, cx| {
            model.companion_dock.project_id = Some("project-a".into());
            model.companion_dock.pending.push(decision.clone());
            model.ensure_companion_forge_forms(&decision, cx);
            model.companion_dock.forms[&forge_answer_key(&decision.id, 0)]
                .update(cx, |input, cx| input.set_value("Keep this draft", cx));
            model.companion_dock.pending.clear();
            model.choose_companion_forge_option(&decision, 0, 1, cx);
        });
        model.read_with(cx, |model, cx| {
            assert_eq!(
                model.companion_dock.forms[&forge_answer_key(&decision.id, 0)]
                    .read(cx)
                    .value(),
                "Keep this draft"
            );
            assert!(
                model
                    .collect_companion_forge_answers(&decision, cx)
                    .is_err()
            );
        });
    }

    #[gpui::test]
    fn forge_question_options_render_and_click_inside_the_native_dock(cx: &mut TestAppContext) {
        let decision = fixture();
        let (model, visual) = cx.add_window_view(|_, cx| {
            let mut model = crate::layout_tests::fixture(cx);
            model.workspace.update(cx, |workspace, _| {
                workspace.project_id = Some("project-a".into())
            });
            model.chat.update(cx, |chat, _| {
                chat.begin_hydration("project-a".into(), None);
                chat.finish_hydration_without_backend();
            });
            model.companion_dock.project_id = Some("project-a".into());
            model.companion_dock.open = true;
            model.companion_dock.pending.push(decision.clone());
            model
                .companion_dock
                .windows
                .push(crate::companion_dock::DockWindow {
                    key: format!("decision:{}", decision.id),
                    conversation_id: None,
                    decision_id: Some(decision.id.clone()),
                    minimized: false,
                });
            model.ensure_companion_forge_forms(&decision, cx);
            model
        });
        visual.simulate_resize(size(px(960.0), px(640.0)));
        visual.run_until_parked();
        let bounds = visual
            .debug_bounds("dock-forge-option-forge:feature-a:3:turn-a-0-0")
            .expect("the recommended option renders in the dock");
        assert!(bounds.size.width > px(0.0) && bounds.size.height > px(0.0));
        visual.simulate_click(bounds.center(), Modifiers::default());
        visual.run_until_parked();
        model.read_with(visual, |model, cx| {
            assert_eq!(
                model.companion_dock.forms[&forge_answer_key(&decision.id, 0)]
                    .read(cx)
                    .value(),
                "JSON"
            );
        });
    }
}
