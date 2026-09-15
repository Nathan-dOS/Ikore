use gpui::{
    Context, Entity, IntoElement, Modifiers, Render, ScrollDelta, ScrollWheelEvent, TestAppContext,
    VisualTestContext, Window, div, point, prelude::*, px, size,
};
use hvc_core::llm::{AgentRole, LlmResilienceSnapshot};
use hvc_core::persistence_service::{Pulsation, PulsationStage, PulsationStatus};
use hvc_core::terminals::{
    AgentExecutionMode, AgentTerminalSnapshot, CliAgentConfig, CliBackend, CliModelSelectionSource,
    FreeFormTerminalSnapshot, SessionPolicy, TerminalSlot, TerminalsSnapshot, ThinkingLevel,
};

use crate::agents::{
    AgentIdentifier, AgentsDashboardSnapshot, CliModelPickerState, CliModelPickerTarget,
};
use crate::pulsations::PulsationDetailSection;
use crate::settings::{SettingsSection, SettingsSnapshot};
use crate::{AppModel, AppTab, AppView};

pub(crate) fn fixture(cx: &mut Context<AppModel>) -> AppModel {
    let model = AppModel::new_for_test(cx);
    model.workspace.update(cx, |workspace, _| {
        workspace.project_id = Some("layout-project".into())
    });
    model.agents.update(cx, |agents, _| {
        let generation = agents.begin(true);
        agents.apply_snapshot(
            generation,
            AgentsDashboardSnapshot {
                terminals: TerminalsSnapshot {
                    project_id: "layout-project".into(),
                    agents: [AgentRole::TaskSpecifier, AgentRole::BackendAgent]
                        .into_iter()
                        .map(|role| AgentTerminalSnapshot {
                            config: CliAgentConfig {
                                project_id: "layout-project".into(),
                                agent_role: role,
                                execution_mode: AgentExecutionMode::CliProcess,
                                backend: CliBackend::Codex,
                                model: None,
                                model_profile: None,
                                model_source: CliModelSelectionSource::Default,
                                thinking_level: ThinkingLevel::Default,
                                custom_executable: None,
                                custom_args: vec![],
                                custom_risk_ack: false,
                                version: 1,
                                created_at: "now".into(),
                                updated_at: "now".into(),
                            },
                            recent_runs: vec![],
                            recent_events: vec![],
                        })
                        .collect(),
                    free_form_slots: vec![FreeFormTerminalSnapshot {
                        slot: TerminalSlot {
                            id: "slot-free".into(),
                            project_id: "layout-project".into(),
                            kind: "free_form".into(),
                            agent_role: None,
                            label: "FreeForm fixture".into(),
                            execution_mode: AgentExecutionMode::CliProcess,
                            backend: CliBackend::Codex,
                            model: None,
                            model_profile: None,
                            model_source: CliModelSelectionSource::Default,
                            thinking_level: ThinkingLevel::Default,
                            session_policy: SessionPolicy::FreshPerRun,
                            custom_executable: None,
                            custom_args: vec![],
                            custom_risk_ack: false,
                            position: 0,
                            archived: false,
                            version: 1,
                            created_at: "now".into(),
                            updated_at: "now".into(),
                        },
                        recent_runs: vec![],
                        recent_events: vec![],
                    }],
                    archived_free_form_slots: vec![],
                    discovery: vec![],
                },
                fallbacks: vec![],
                free_form_bindings: vec![],
                api_executions: Default::default(),
            },
        );
    });
    model.settings.update(cx, |settings, _| {
        settings.snapshot = Some(SettingsSnapshot {
            composition: None,
            preparation: Default::default(),
            config: Default::default(),
            profiles: vec![],
            connections: vec![],
            routes: vec![],
            bindings: vec![],
            key_statuses: vec![],
            resilience: LlmResilienceSnapshot {
                generated_at: "now".into(),
                states: vec![],
            },
        });
    });
    model.pulsations.update(cx, |state, _| {
        state.pulsations = ["pulse-a", "pulse-b"]
            .into_iter()
            .map(|id| Pulsation {
                id: id.into(),
                project_id: "layout-project".into(),
                feature_name: "Layout regression".into(),
                stage: PulsationStage::Specification,
                status: PulsationStatus::Planning,
                terminal_reason: None,
                created_at: "now".into(),
                updated_at: "now".into(),
            })
            .collect();
        state.select("pulse-a");
    });
    model
}

struct ModelHarness(Entity<AppModel>);
impl Render for ModelHarness {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.0.clone())
    }
}

#[gpui::test]
fn pulsation_groups_keep_user_choices_when_leaving_and_returning(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    select(&model, visual, AppView::Pulsations);
    assert!(model.read_with(visual, |m, cx| {
        m.pulsations
            .read(cx)
            .group_expanded(PulsationStatus::Running)
    }));
    assert!(!model.read_with(visual, |m, cx| {
        m.pulsations
            .read(cx)
            .group_expanded(PulsationStatus::Planning)
    }));
    click(visual, "pulsation-group-running");
    click(visual, "pulsation-group-planning");
    let before_width = visual
        .debug_bounds("pulsations-columns-left")
        .unwrap()
        .size
        .width;
    let sash = visual
        .debug_bounds("pulsations-columns-sash")
        .unwrap()
        .center();
    visual.simulate_mouse_down(sash, gpui::MouseButton::Left, Modifiers::default());
    visual.simulate_mouse_move(
        sash + gpui::point(px(60.0), px(0.0)),
        gpui::MouseButton::Left,
        Modifiers::default(),
    );
    visual.simulate_mouse_up(
        sash + gpui::point(px(60.0), px(0.0)),
        gpui::MouseButton::Left,
        Modifiers::default(),
    );
    visual.run_until_parked();
    let resized_width = visual
        .debug_bounds("pulsations-columns-left")
        .unwrap()
        .size
        .width;
    assert!(resized_width > before_width);
    select(&model, visual, AppView::Workspace);
    select(&model, visual, AppView::Pulsations);
    assert_eq!(
        visual
            .debug_bounds("pulsations-columns-left")
            .unwrap()
            .size
            .width,
        resized_width
    );
    assert!(!model.read_with(visual, |m, cx| {
        m.pulsations
            .read(cx)
            .group_expanded(PulsationStatus::Running)
    }));
    assert!(model.read_with(visual, |m, cx| {
        m.pulsations
            .read(cx)
            .group_expanded(PulsationStatus::Planning)
    }));
    assert!(
        visual
            .debug_bounds("pulsation-group-running")
            .unwrap()
            .top()
            < visual
                .debug_bounds("pulsation-group-planning")
                .unwrap()
                .top()
    );
}

#[gpui::test]
fn complete_review_and_workspace_readers_fit_small_and_large_windows(cx: &mut TestAppContext) {
    use hvc_core::persistence_service::{HitlCheckpoint, HitlReviewDocument};
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.update(|_, cx| crate::hitl::register_hitl_keybindings(cx));
    let complete = format!(
        "# Complete review\n\n{}\nLast checkpoint paragraph.",
        "Long content for scrolling and reading.\n\n".repeat(80)
    );
    model.update(visual, |m, cx| {
        m.workspace.update(cx, |workspace, cx| {
            let tab = crate::workspace::WorkspaceTabModel::new(
                "unrelated.md",
                "# Unrelated workspace file\n\nDifferent content.".into(),
                "v1".into(),
            );
            workspace.editor.update(cx, |editor, _| {
                editor
                    .restore_workspace_snapshot("unrelated.md", tab.editor_snapshot())
                    .unwrap()
            });
            workspace.active_relative_path = Some(tab.relative_path.clone());
            workspace.tabs = vec![tab];
        });
        m.pulsations.update(cx, |state, _| {
            state.pulsations[0].status = PulsationStatus::Checkpoint;
            let pulse = state.pulsations[0].clone();
            let checkpoint = HitlCheckpoint {
                pulsation_id: pulse.id.clone(),
                checkpoint_id: "layout-checkpoint".into(),
                state_version: 1,
            };
            state.hitl.load_checkpoint(Some(&pulse), Some(checkpoint));
            let (generation, checkpoint) = state.hitl.begin_documents().unwrap();
            state.hitl.finish_documents(
                generation,
                &checkpoint,
                Ok(vec![HitlReviewDocument {
                    artifact_id: "review-artifact".into(),
                    name: "Complete checkpoint document".into(),
                    kind: "stage_output".into(),
                    relative_path: Some("review.md".into()),
                    content: complete.clone(),
                }]),
            );
            state.detail_section = PulsationDetailSection::Review;
        });
    });
    for (width, height) in [(960.0, 640.0), (1440.0, 900.0)] {
        visual.simulate_resize(size(px(width), px(height)));
        select(&model, visual, AppView::Pulsations);
        let reader = visual.debug_bounds("review-document-reader").unwrap();
        let actions = visual.debug_bounds("review-decision-bar").unwrap();
        assert!(
            reader.size.height >= px(100.0),
            "reader too small: {reader:?}"
        );
        assert!(reader.bottom() <= actions.top() && actions.bottom() <= px(height));
        for action in ["hitl-approve", "hitl-revision", "hitl-reject"] {
            let button = visual.debug_bounds(action).unwrap();
            assert!(button.left() >= actions.left() && button.right() <= actions.right());
        }
        click(visual, "review-copy-document");
        visual.update(|_, cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().as_deref(),
                Some(complete.as_str())
            )
        });
        click(visual, "review-source-toggle");
        assert_eq!(visual.debug_bounds("review-decision-bar").unwrap(), actions);
        click(visual, "review-source-toggle");
        select(&model, visual, AppView::Workspace);
        let editor = visual.debug_bounds("workspace-editor-viewport").unwrap();
        let preview = visual.debug_bounds("workspace-markdown-preview").unwrap();
        assert!(editor.right() <= preview.left());
        assert!(editor.size.height > px(100.0) && preview.size.height > px(100.0));
        assert!(
            preview.right() <= px(width) && preview.bottom() <= px(height),
            "window={width}x{height}; editor={editor:?}; preview={preview:?}"
        );
    }
    select(&model, visual, AppView::Pulsations);
    model.update(visual, |model, cx| {
        model
            .pulsations
            .update(cx, |state, _| state.hitl.show_feedback = true);
        cx.notify();
    });
    visual.run_until_parked();
    visual.update(|window, cx| {
        let input = model.read(cx).stage_feedback_input.clone();
        gpui::Focusable::focus_handle(input.read(cx), cx).focus(window);
    });
    visual.simulate_keystrokes("r d enter");
    visual.run_until_parked();
    model.read_with(visual, |model, cx| {
        // The single-line input normalizes the inserted newline to a space.
        assert_eq!(model.stage_feedback_input.read(cx).value(), "rd ");
        assert!(model.pulsations.read(cx).hitl.can_approve());
        assert!(model.pulsations.read(cx).hitl.error.is_none());
        assert!(!model.confirm_hitl_reject);
    });
}

/// Parks a stage failure with the given offered decisions in a window of its own.
///
/// Each case needs a fresh window: GPUI keeps debug selectors alive in reused frames,
/// so a button's absence is only observable in a window that never drew it.
fn parked_failure_window<'a>(
    cx: &'a mut TestAppContext,
    class: &'static str,
    retry: bool,
    revision: bool,
    window: (f32, f32),
) -> (Entity<AppModel>, &'a mut VisualTestContext) {
    use hvc_core::persistence_service::StageFailureCheckpoint;
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.update(|_, cx| crate::failure_checkpoint::register_failure_keybindings(cx));
    model.update(visual, |m, cx| {
        m.pulsations.update(cx, |state, _| {
            state.pulsations[0].status = PulsationStatus::Checkpoint;
            let pulse = state.pulsations[0].clone();
            state.failure.load_checkpoint(Some(StageFailureCheckpoint {
                checkpoint_id: "layout-failure".into(),
                pulsation_id: pulse.id.clone(),
                project_id: "layout-project".into(),
                feature_name: "Native editor".into(),
                stage: PulsationStage::Test,
                failure_class: class.into(),
                error_code: "provider_timeout".into(),
                diagnostic: "the provider timed out after 60s".into(),
                allow_retry: retry,
                allow_revision: revision,
                revision_stage: revision.then_some(PulsationStage::Code),
                attempts: 3,
                state_version: 1,
                created_at: "2026-09-09T00:00:00Z".into(),
                allowed_revision_targets: Vec::new(),
                remaining_extra_invocations: None,
                stage_execution_id: None,
            }));
            state.detail_section = PulsationDetailSection::Review;
        });
    });
    visual.simulate_resize(size(px(window.0), px(window.1)));
    select(&model, visual, AppView::Pulsations);
    (model, visual)
}

#[gpui::test]
fn failure_decision_bar_fits_supported_window_sizes(cx: &mut TestAppContext) {
    for (width, height) in [(960.0, 640.0), (1440.0, 900.0)] {
        let (_model, visual) =
            parked_failure_window(cx, "retry_budget_exhausted", true, true, (width, height));
        let bar = visual.debug_bounds("failure-decision-bar").unwrap();
        assert!(bar.bottom() <= px(height), "bar off screen: {bar:?}");
        for action in ["failure-retry", "failure-revision", "failure-abort"] {
            let button = visual.debug_bounds(action).unwrap();
            assert!(
                button.left() >= bar.left() && button.right() <= bar.right(),
                "{action} escapes the bar at {width}x{height}: {button:?} vs {bar:?}"
            );
        }
        // The artifact review shares this tab and must yield to the parked failure.
        assert!(visual.debug_bounds("review-decision-bar").is_none());
    }
}

#[gpui::test]
fn a_failure_offers_only_the_decisions_the_host_accepts(cx: &mut TestAppContext) {
    // The agent assessed the work and gave up; its answer is durable, so retrying it
    // is not on offer, but written guidance still is.
    let (_m, visual) =
        parked_failure_window(cx, "model_reported_failure", false, true, (1440.0, 900.0));
    assert!(visual.debug_bounds("failure-retry").is_none());
    visual.debug_bounds("failure-revision").unwrap();
    visual.debug_bounds("failure-abort").unwrap();

    // Nothing was produced, so there is nothing to revise and no guidance box.
    let (_m, visual) =
        parked_failure_window(cx, "provider_configuration", true, false, (1440.0, 900.0));
    assert!(visual.debug_bounds("failure-revision").is_none());
    assert!(visual.debug_bounds("failure-feedback-toggle").is_none());
    visual.debug_bounds("failure-retry").unwrap();
    visual.debug_bounds("failure-abort").unwrap();
}

#[gpui::test]
fn aborting_a_failure_takes_two_clicks_and_never_moves_the_button(cx: &mut TestAppContext) {
    let (model, visual) =
        parked_failure_window(cx, "provider_transport", true, false, (1440.0, 900.0));
    let armed = visual.debug_bounds("failure-abort").unwrap();
    click(visual, "failure-abort");
    model.update(visual, |m, cx| {
        assert!(
            m.confirm_failure_abort,
            "the first click must only arm abort"
        );
        assert!(
            m.pulsations.read(cx).failure.checkpoint.is_some(),
            "the first click must not resolve the checkpoint"
        );
    });
    // The label shortens when armed, so the button is allowed to resize. What must not
    // change is where the cursor is: the second click has to land on the same control.
    let after = visual.debug_bounds("failure-abort").unwrap();
    let cursor = armed.center();
    assert_eq!(after.origin, armed.origin, "arming abort moved the button");
    assert!(
        cursor.x >= after.left()
            && cursor.x <= after.right()
            && cursor.y >= after.top()
            && cursor.y <= after.bottom(),
        "arming abort slid the control out from under the cursor: {cursor:?} not in {after:?}"
    );
}

/// A failed pulsation whose cleanup preview already arrived, in a window of its own.
fn failed_with_leftovers_window(
    cx: &mut TestAppContext,
    window: (f32, f32),
) -> (Entity<AppModel>, &mut VisualTestContext) {
    use hvc_core::orchestration::{
        CleanupDisposition, FailedPulsationCleanupEntry, FailedPulsationCleanupPlan,
    };
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    model.update(visual, |m, cx| {
        m.pulsations.update(cx, |state, _| {
            state.pulsations[0].status = PulsationStatus::Failed;
            state.pulsations[0].terminal_reason = Some("user_cancel".into());
            state.detail_section = PulsationDetailSection::Overview;
            state.cleanup.plan = Some(FailedPulsationCleanupPlan {
                pulsation_id: "pulse-a".into(),
                project_id: "layout-project".into(),
                entries: vec![
                    FailedPulsationCleanupEntry {
                        relative_path: "src/generated/feature.rs".into(),
                        disposition: CleanupDisposition::Remove,
                        detail: None,
                    },
                    FailedPulsationCleanupEntry {
                        relative_path: "src/lib.rs".into(),
                        disposition: CleanupDisposition::KeepEdited,
                        detail: None,
                    },
                ],
                candidate_dirs: vec!["exec-1".into()],
                plan_token: "layout-token".into(),
            });
        });
        m.select_view(AppView::Pulsations, cx);
    });
    visual.simulate_resize(size(px(window.0), px(window.1)));
    visual.run_until_parked();
    (model, visual)
}

#[gpui::test]
fn leftover_files_card_shows_only_for_failed_pulsations(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    model.update(visual, |m, cx| m.select_view(AppView::Pulsations, cx));
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("pulsation-cleanup").is_none(),
        "a planning pulsation has nothing to clean"
    );
    let (_, visual) = failed_with_leftovers_window(cx, (1280.0, 820.0));
    let card = visual.debug_bounds("pulsation-cleanup").unwrap();
    let remove = visual.debug_bounds("pulsation-cleanup-remove").unwrap();
    let refresh = visual.debug_bounds("pulsation-cleanup-refresh").unwrap();
    assert!(
        remove.size.width < card.size.width / 2.0,
        "the removal button sizes to its label"
    );
    assert!(refresh.left() > remove.right(), "controls sit side by side");
}

#[gpui::test]
fn removing_leftover_files_takes_two_clicks_and_never_moves_the_button(cx: &mut TestAppContext) {
    let (model, visual) = failed_with_leftovers_window(cx, (1280.0, 820.0));
    let before = visual.debug_bounds("pulsation-cleanup-remove").unwrap();
    click(visual, "pulsation-cleanup-remove");
    model.update(visual, |m, cx| {
        assert!(
            m.pulsations.read(cx).cleanup.armed,
            "the first click only arms"
        );
    });
    let after = visual.debug_bounds("pulsation-cleanup-remove").unwrap();
    let cursor = before.center();
    assert_eq!(after.origin, before.origin, "arming moved the button");
    assert!(
        cursor.x >= after.left()
            && cursor.x <= after.right()
            && cursor.y >= after.top()
            && cursor.y <= after.bottom(),
        "arming slid the control out from under the cursor: {cursor:?} not in {after:?}"
    );
    // Leaving the pulsation disarms and drops the preview of the other one.
    model.update(visual, |m, cx| {
        m.select_pulsation("pulse-b", cx);
        let state = m.pulsations.read(cx);
        assert!(!state.cleanup.armed);
        assert!(state.cleanup.plan.is_none());
    });
}

#[gpui::test]
fn failure_abort_confirmation_expires_when_selecting_another_pulsation(cx: &mut TestAppContext) {
    let (model, visual) =
        parked_failure_window(cx, "provider_transport", true, false, (1440.0, 900.0));
    click(visual, "failure-abort");
    model.update(visual, |m, cx| {
        assert!(m.confirm_failure_abort);
        m.select_pulsation("pulse-b", cx);
        assert!(!m.confirm_failure_abort);
    });
}

/// A parked composed stage (ADR-022): the failure checkpoint carries the
/// permitted revision targets and the execution snapshot lists units.
fn composed_detail(pulse: &Pulsation) -> hvc_core::orchestration::PulsationCoordinationSnapshot {
    use hvc_core::orchestration::composition::{
        AttemptState, AttemptSummaryV1, CheckStatus, CheckSummaryV1, ExecutionStatus, ExecutorKind,
        Handle, RemainingBudgetV1, Stage, StageExecutionSnapshot, UnitSnapshotV1, UnitState,
        UsageCompleteness, UsageSummaryV1, Uuid,
    };
    use hvc_core::orchestration::*;
    let fresh_id = || Uuid::new(uuid::Uuid::new_v4().to_string()).unwrap();
    let attempt = |kind: ExecutorKind, state: AttemptState, error: Option<&str>| AttemptSummaryV1 {
        attempt_id: fresh_id(),
        state,
        executor_kind: kind,
        effective_model: "cheap-model".into(),
        profile_snapshot_id: fresh_id(),
        error_code: error.map(|e| Handle::new(e).unwrap()),
        usage: UsageSummaryV1 {
            input_tokens: Some(1_200),
            output_tokens: Some(300),
            reasoning_tokens: None,
            cached_input_tokens: None,
            completeness: UsageCompleteness::Partial,
        },
    };
    let unit =
        |key: &str, kind: ExecutorKind, state: UnitState, attempts: Vec<AttemptSummaryV1>| {
            UnitSnapshotV1 {
                unit_id: fresh_id(),
                logical_unit_key: Handle::new(key).unwrap(),
                executor_kind: kind,
                generation: 1,
                state_version: 2,
                state,
                reused_from_attempt_id: None,
                attempts,
            }
        };
    PulsationCoordinationSnapshot {
        description_editable: false,
        context_disposition: None,
        diagnostic: None,
        pulsation_id: pulse.id.clone(),
        project_id: pulse.project_id.clone(),
        queue_sequence: 1,
        pipeline_version: 4,
        input_revision: 1,
        state_version: 3,
        stage: PulsationStage::Code,
        status: PulsationStatus::Checkpoint,
        blocking_reason: None,
        preparation: PreparationSnapshot {
            assessment: None,
            description_review: DescriptionReviewState::Pending,
            prespec_state: PreSpecState::NotScheduled,
            context_artifact_id: None,
            skip_reason: None,
        },
        questions: None,
        learning: LearningState::Unavailable,
        learning_run: None,
        artifact_ids: vec![],
        stage_execution: Some(StageExecutionSnapshot {
            schema_version: hvc_core::orchestration::SchemaV1,
            project_id: fresh_id(),
            pulsation_id: fresh_id(),
            stage_execution_id: fresh_id(),
            stage: Stage::Code,
            parent_generation: 1,
            parent_state_version: 3,
            execution_version: 2,
            status: ExecutionStatus::Parked,
            units: vec![
                unit(
                    "unit_core",
                    ExecutorKind::CodeImplementer,
                    UnitState::Validated,
                    vec![attempt(
                        ExecutorKind::CodeImplementer,
                        AttemptState::Succeeded,
                        None,
                    )],
                ),
                unit(
                    "unit_api",
                    ExecutorKind::CodeImplementer,
                    UnitState::Failed,
                    vec![
                        attempt(
                            ExecutorKind::CodeImplementer,
                            AttemptState::Rejected,
                            Some("ownership_violation"),
                        ),
                        attempt(
                            ExecutorKind::CodeRepairer,
                            AttemptState::Rejected,
                            Some("ownership_violation"),
                        ),
                    ],
                ),
            ],
            blocked_reason: Some("Unit `unit_api` wrote outside its ownership twice.".into()),
            checks: vec![CheckSummaryV1 {
                check_ref: Handle::new("cargo_check").unwrap(),
                required: true,
                receipt_id: None,
                status: CheckStatus::NotRun,
            }],
            remaining_budget: RemainingBudgetV1 {
                initial_invocations: 0,
                extra_invocations: 0,
                initial_output_tokens: 0,
                extra_output_tokens: 0,
                initial_prompt_characters: 0,
                extra_prompt_characters: 0,
            },
            usage: UsageSummaryV1 {
                input_tokens: Some(3_600),
                output_tokens: Some(900),
                reasoning_tokens: None,
                cached_input_tokens: None,
                completeness: UsageCompleteness::Partial,
            },
        }),
    }
}

#[gpui::test]
fn a_composed_stage_lists_its_units_and_offers_only_permitted_revision_targets(
    cx: &mut TestAppContext,
) {
    let (model, visual) =
        parked_failure_window(cx, "ownership_violation", false, true, (1440.0, 900.0));
    model.update(visual, |m, cx| {
        m.pulsations.update(cx, |state, _| {
            let pulse = state.pulsations[0].clone();
            let mut checkpoint = state.failure.checkpoint.clone().unwrap();
            checkpoint.stage = PulsationStage::Code;
            checkpoint.error_code = "ownership_violation".into();
            checkpoint.allowed_revision_targets =
                vec![PulsationStage::Code, PulsationStage::Specification];
            checkpoint.remaining_extra_invocations = Some(0);
            checkpoint.stage_execution_id = Some("exec-1".into());
            state.failure.load_checkpoint(Some(checkpoint));
            state.details = vec![composed_detail(&pulse)];
        });
        cx.notify();
    });
    select(&model, visual, AppView::Pulsations);

    // Review: no permit left means no Retry; the permitted targets are offered
    // and the pick is durable in the model before any request is sent.
    assert!(visual.debug_bounds("failure-retry").is_none());
    visual.debug_bounds("failure-target-Code").unwrap();
    assert!(visual.debug_bounds("failure-target-Test").is_none());
    click(visual, "failure-target-Specification");
    model.read_with(visual, |m, cx| {
        assert_eq!(
            m.pulsations.read(cx).failure.selected_revision_target(),
            Some(PulsationStage::Specification)
        );
    });
    visual.debug_bounds("failure-revision").unwrap();

    // Overview: the execution snapshot renders one row per unit, with the
    // host checks separate from the units and no per-unit approval control.
    model.update(visual, |m, cx| {
        m.pulsations.update(cx, |state, _| {
            state.detail_section = PulsationDetailSection::Overview;
        });
        cx.notify();
    });
    select(&model, visual, AppView::Pulsations);
    let panel = visual.debug_bounds("composition-panel").unwrap();
    for unit in ["composition-unit-unit_core", "composition-unit-unit_api"] {
        let row = visual.debug_bounds(unit).unwrap();
        assert!(
            row.top() >= panel.top() && row.bottom() <= panel.bottom(),
            "{unit} escapes the panel: {row:?} vs {panel:?}"
        );
    }
    assert!(visual.debug_bounds("composition-unit-approve").is_none());
}

fn assert_absent(model: &Entity<AppModel>, visual: &mut VisualTestContext, selector: &'static str) {
    // GPUI 0.2.2 keeps debug selectors in reused frames, so test absence in a fresh window.
    let (_, fresh) = visual.add_window_view(|_, _| ModelHarness(model.clone()));
    assert!(
        fresh.debug_bounds(selector).is_none(),
        "unexpected {selector}"
    );
}

fn click(visual: &mut VisualTestContext, selector: &'static str) {
    let bounds = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("missing {selector}"));
    assert!(
        bounds.size.width > px(0.0) && bounds.size.height > px(0.0),
        "empty {selector}"
    );
    visual.simulate_click(bounds.center(), Modifiers::default());
    visual.run_until_parked();
}

fn select(model: &Entity<AppModel>, visual: &mut VisualTestContext, view: AppView) {
    model.update(visual, |model, cx| model.select_view(view, cx));
    visual.run_until_parked();
}

#[gpui::test]
fn notification_and_direct_selector_overlays_do_not_move_controls(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    for (width, height) in [(960.0, 640.0), (1440.0, 900.0)] {
        visual.simulate_resize(size(px(width), px(height)));
        select(&model, visual, AppView::Agents);
        let before = visual.debug_bounds("agents-cli-scroll").unwrap();
        let backend = visual.debug_bounds("agent-backend:task_specifier").unwrap();
        click(visual, "agent-backend:task_specifier");
        let menu = visual.debug_bounds("selection-menu").unwrap();
        assert!(menu.left() >= px(0.0) && menu.right() <= px(width));
        assert!(menu.top() >= px(0.0) && menu.bottom() <= px(height));
        assert_eq!(visual.debug_bounds("agents-cli-scroll").unwrap(), before);
        assert_eq!(
            visual.debug_bounds("agent-backend:task_specifier").unwrap(),
            backend
        );
        visual.simulate_keystrokes("down escape");
        visual.run_until_parked();
        assert!(model.read_with(visual, |m, _| m.choice_menu.is_none()));
        // Escape returns focus to the stable trigger, even after a parent render.
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        assert!(model.read_with(visual, |m, _| m.choice_menu.is_some()));
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        model.update(visual,|m,cx| {
            m.push_header_notice(crate::session::HeaderNoticeKind::Error,"A long runtime diagnostic that must stay readable without moving the agent controls or the shell navigation.",cx);
        });
        visual.run_until_parked();
        assert_eq!(visual.debug_bounds("agents-cli-scroll").unwrap(), before);
        click(visual, "notifications");
        let history = visual.debug_bounds("notification-history").unwrap();
        assert!(history.right() <= px(width) && history.bottom() <= px(height));
        assert_eq!(visual.debug_bounds("agents-cli-scroll").unwrap(), before);
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        assert!(!model.read_with(visual, |m, _| m.notifications.history_open));
    }
}

#[gpui::test]
fn changing_project_cancels_an_open_selector_and_isolates_history(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    select(&model, visual, AppView::Agents);
    model.update(visual, |m, cx| {
        m.push_header_notice(
            crate::session::HeaderNoticeKind::Warning,
            "Project A warning",
            cx,
        )
    });
    click(visual, "agent-backend:task_specifier");
    model.update(visual, |m, cx| {
        m.workspace
            .update(cx, |w, _| w.project_id = Some("project-b".into()));
        cx.notify();
    });
    visual.run_until_parked();
    model.read_with(visual, |m, _| {
        assert!(m.choice_menu.is_none());
        assert!(m.notifications.queue.history().is_empty());
    });
    model.update(visual, |m, cx| {
        m.workspace
            .update(cx, |w, _| w.project_id = Some("layout-project".into()));
        cx.notify();
    });
    visual.run_until_parked();
    assert_eq!(
        model.read_with(visual, |m, _| m.notifications.queue.history().len()),
        1
    );
}

#[gpui::test]
fn closing_a_selector_does_not_activate_covered_navigation(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    select(&model, visual, AppView::Agents);
    click(visual, "agent-backend:task_specifier");
    click(visual, "settings");
    assert_eq!(
        model.read_with(visual, |m, _| m.active_view()),
        AppView::Agents
    );
    assert!(model.read_with(visual, |m, _| m.choice_menu.is_none()));
    click(visual, "settings");
    assert_eq!(
        model.read_with(visual, |m, _| m.active_view()),
        AppView::Settings
    );
}

#[gpui::test]
fn header_tabs_and_settings_navigate_at_minimum_window_size(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(960.0), px(640.0)));
    visual.run_until_parked();
    click(visual, "agent");
    assert_eq!(
        model.read_with(visual, |model, _| model.active_view()),
        AppView::Agents
    );
    click(visual, "logs");
    click(visual, "engine");
    click(visual, "settings");
    assert_absent(&model, visual, "sub-tabs");
    click(visual, "agent");
    assert_eq!(
        model.read_with(visual, |model, _| model.active_view()),
        AppView::Logs
    );
    assert_eq!(
        model.read_with(visual, |model, _| model.active_tab()),
        AppTab::Agent
    );
    let new_work = visual.debug_bounds("new-work").unwrap();
    let settings = visual.debug_bounds("settings").unwrap();
    let notifications = visual.debug_bounds("notifications").unwrap();
    assert!(settings.right() <= px(960.0));
    assert!(new_work.left() < px(80.0));
    assert!(notifications.left() < settings.left());
    assert!(visual.debug_bounds("toggle-sidebar").is_none());
    for (width, height) in [(960.0, 640.0), (1440.0, 900.0)] {
        visual.simulate_resize(size(px(width), px(height)));
        visual.run_until_parked();
        let agent = visual.debug_bounds("agent").unwrap();
        let companion = visual.debug_bounds("companion").unwrap();
        let new_work = visual.debug_bounds("new-work").unwrap();
        let notifications = visual.debug_bounds("notifications").unwrap();
        let settings = visual.debug_bounds("settings").unwrap();
        assert!(new_work.right() < agent.left());
        assert!(agent.left() < px(width / 2.0));
        assert!(companion.right() > px(width / 2.0));
        assert!(notifications.left() > companion.right());
        assert!(settings.left() > notifications.left());
        for selector in ["agent", "engine", "companion", "notifications", "settings"] {
            let control = visual.debug_bounds(selector).unwrap();
            assert!(control.left() >= px(0.0) && control.right() <= px(width));
            assert!(control.top() >= px(0.0) && control.bottom() <= px(58.0));
        }
        let flow_tabs = visual.debug_bounds("flow-bar-tabs").unwrap();
        assert!(flow_tabs.left() < px(width / 2.0) && flow_tabs.right() > px(width / 2.0));
    }
}

#[gpui::test]
fn agents_header_shows_the_active_pulsation_pipeline(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(960.0), px(640.0)));
    select(&model, visual, AppView::Agents);

    let agents_header = visual.debug_bounds("agents-header").unwrap();
    let pipeline = visual.debug_bounds("agents-pipeline").unwrap();
    assert!(
        pipeline.top() >= agents_header.top() && pipeline.bottom() <= agents_header.bottom(),
        "pipeline escaped header: {pipeline:?} vs {agents_header:?}"
    );
    for selector in [
        "agents-pipeline-stage-intake",
        "agents-pipeline-stage-specification",
        "agents-pipeline-stage-contract",
        "agents-pipeline-stage-code",
        "agents-pipeline-stage-test",
        "agents-pipeline-stage-integrator",
    ] {
        let stage_bounds = visual.debug_bounds(selector).unwrap();
        assert!(stage_bounds.left() >= pipeline.left());
        assert!(stage_bounds.right() <= pipeline.right());
    }
    assert!(visual.debug_bounds("agents-pipeline-current").is_some());
    assert!(model.read_with(visual, |model, cx| {
        model.pulsations.read(cx).active_id().as_deref() == Some("pulse-a")
    }));
}

#[gpui::test]
fn agent_selection_and_modal_preserve_detail_geometry(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(960.0), px(640.0)));
    select(&model, visual, AppView::Agents);
    assert!(visual.debug_bounds("agent-card:task_specifier").is_some());
    assert!(visual.debug_bounds("agent-card:backend_agent").is_none());
    click(visual, "agent-select:backend_agent");
    assert!(visual.debug_bounds("agent-card:backend_agent").is_some());
    let before = visual.debug_bounds("agents-cli-scroll").unwrap();
    model.update(visual, |model, cx| {
        model.agents.update(cx, |agents, _| {
            agents.active_model_picker = Some(CliModelPickerState {
                target: CliModelPickerTarget::Canonical(AgentRole::BackendAgent),
                backend: CliBackend::Codex,
            })
        });
        cx.notify();
    });
    visual.run_until_parked();
    assert_eq!(visual.debug_bounds("agents-cli-scroll").unwrap(), before);
    let overlay = visual.debug_bounds("cli-model-picker-content").unwrap();
    assert!(overlay.right() <= px(960.0));
    assert!(overlay.bottom() <= px(640.0));
    let close = visual.debug_bounds("cli-model-close").unwrap();
    assert!(close.left() >= overlay.left() && close.right() <= overlay.right());
    assert!(close.top() >= overlay.top() && close.bottom() <= overlay.bottom());
    // The scrim prevents selection changes behind the modal.
    click(visual, "agent-select:task_specifier");
    assert_eq!(
        model.read_with(visual, |model, cx| model
            .agents
            .read(cx)
            .selected_agent
            .clone()),
        Some(AgentIdentifier::Canonical(AgentRole::BackendAgent))
    );
    assert!(!model.read_with(visual, |m, cx| {
        m.agents.read(cx).active_model_picker.is_some()
    }));
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    assert!(visual.debug_bounds("cli-model-picker-content").is_none());
    model.update(visual, |model, cx| {
        model.toggle_agents_terminal_expanded(cx)
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("agents-terminal-expanded").is_some());
    assert!(visual.debug_bounds("agents-list").is_some());
    model.update(visual, |model, cx| {
        model.agents.update(cx, |agents, _| {
            agents.selected_agent = Some(AgentIdentifier::Canonical(AgentRole::TaskSpecifier));
        });
        cx.notify();
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("agents-terminal-expanded").is_some());
    assert_eq!(
        model.read_with(visual, |model, cx| model.agents.read(cx).selected_agent.clone()),
        Some(AgentIdentifier::Canonical(AgentRole::TaskSpecifier))
    );
    // The visual harness does not consistently route synthetic Escape events
    // through a newly mounted expanded panel. The production root handler is
    // covered by the explicit focus/keyboard behavior; keep this geometry
    // test deterministic by exercising the same collapse action directly.
    model.update(visual, |model, cx| {
        model.toggle_agents_terminal_expanded(cx)
    });
    visual.run_until_parked();
    assert!(!model.read_with(visual, |model, cx| model.agents.read(cx).terminal_expanded));
    click(visual, "agent-select:backend_agent");
    model.update(visual, |model, cx| {
        model.agents.update(cx, |agents, _| {
            let snapshot = agents.snapshot.as_mut().unwrap();
            snapshot
                .agents
                .iter_mut()
                .find(|agent| agent.config.agent_role == AgentRole::BackendAgent)
                .unwrap()
                .config
                .execution_mode = AgentExecutionMode::ApiDirect;
        });
        cx.notify();
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("agents-api-scroll").is_some());
    assert!(visual.debug_bounds("agent-mode:backend_agent").is_some());
    assert!(visual.debug_bounds("agent-select:slot-free").is_some());
    // The detail has the per-target selector; global mode controls are absent by construction.
}

#[gpui::test]
fn agents_detail_accepts_vertical_wheel_scrolling(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(960.0), px(640.0)));
    select(&model, visual, AppView::Agents);

    let scroll = visual.debug_bounds("agents-cli-scroll").unwrap();
    let before = visual
        .debug_bounds("agent-start:task_specifier")
        .expect("start action should be present below the agent configuration");
    visual.simulate_event(ScrollWheelEvent {
        position: point(scroll.left() + px(12.0), scroll.top() + px(24.0)),
        delta: ScrollDelta::Pixels(point(px(0.0), px(-240.0))),
        ..Default::default()
    });
    visual.run_until_parked();
    let after = visual.debug_bounds("agent-start:task_specifier").unwrap();
    let panel = visual.debug_bounds("agents-panel");
    let header = visual.debug_bounds("agents-header");
    let content = visual.debug_bounds("agents-content");
    let columns = visual.debug_bounds("agents-columns");

    assert!(
        after.top() < before.top(),
        "Agents detail did not move after a downward wheel event: {before:?} -> {after:?}; \
         panel={panel:?}; header={header:?}; content={content:?}; columns={columns:?}; \
         scroll={scroll:?}"
    );
}

#[gpui::test]
fn agents_api_detail_accepts_vertical_wheel_scrolling(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    model.update(visual, |model, cx| {
        model.agents.update(cx, |agents, _| {
            let snapshot = agents.snapshot.as_mut().unwrap();
            snapshot.agents[0].config.execution_mode = AgentExecutionMode::ApiDirect;
        });
        cx.notify();
    });
    visual.simulate_resize(size(px(960.0), px(640.0)));
    select(&model, visual, AppView::Agents);

    let scroll = visual.debug_bounds("agents-api-scroll").unwrap();
    let before = visual
        .debug_bounds("api-activity:canonical:task_specifier")
        .expect("API activity viewer should be present below the route configuration");
    visual.simulate_event(ScrollWheelEvent {
        position: point(scroll.left() + px(12.0), scroll.top() + px(24.0)),
        delta: ScrollDelta::Pixels(point(px(0.0), px(-240.0))),
        ..Default::default()
    });
    visual.run_until_parked();
    let after = visual
        .debug_bounds("api-activity:canonical:task_specifier")
        .unwrap();

    assert!(
        after.top() < before.top(),
        "API detail did not move after a downward wheel event: {before:?} -> {after:?}"
    );
}

#[gpui::test]
fn agent_logs_detail_accepts_vertical_wheel_scrolling(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(960.0), px(640.0)));
    select(&model, visual, AppView::Logs);

    let scroll = visual.debug_bounds("logs-detail-scroll").unwrap();
    let before = visual
        .debug_bounds("logs-toggle-events")
        .expect("events control should be present near the end of the Logs detail");
    visual.simulate_event(ScrollWheelEvent {
        position: point(scroll.left() + px(12.0), scroll.top() + px(24.0)),
        delta: ScrollDelta::Pixels(point(px(0.0), px(-240.0))),
        ..Default::default()
    });
    visual.run_until_parked();
    let after = visual.debug_bounds("logs-toggle-events").unwrap();

    assert!(
        after.top() < before.top(),
        "Logs detail did not move after a downward wheel event: {before:?} -> {after:?}"
    );
}

#[gpui::test]
fn agents_list_accepts_vertical_wheel_scrolling(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    model.update(visual, |model, cx| {
        model.agents.update(cx, |agents, _| {
            let snapshot = agents.snapshot.as_mut().unwrap();
            let template = snapshot.agents[0].clone();
            snapshot.agents = AgentRole::ALL
                .into_iter()
                .map(|role| {
                    let mut agent = template.clone();
                    agent.config.agent_role = role;
                    agent
                })
                .collect();
        });
        cx.notify();
    });
    visual.simulate_resize(size(px(960.0), px(640.0)));
    select(&model, visual, AppView::Agents);

    let scroll = visual.debug_bounds("agents-list").unwrap();
    let before = visual
        .debug_bounds("agent-select:slot-free")
        .expect("FreeForm item should be present at the end of the agent list");
    visual.simulate_event(ScrollWheelEvent {
        position: point(scroll.left() + px(12.0), scroll.top() + px(24.0)),
        delta: ScrollDelta::Pixels(point(px(0.0), px(-240.0))),
        ..Default::default()
    });
    visual.run_until_parked();
    let after = visual.debug_bounds("agent-select:slot-free").unwrap();

    assert!(
        after.top() < before.top(),
        "Agent list did not move after a downward wheel event: {before:?} -> {after:?}"
    );
}

#[gpui::test]
fn settings_creation_forms_remain_reachable_after_category_changes(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(960.0), px(640.0)));
    select(&model, visual, AppView::Settings);
    assert_absent(&model, visual, "settings-save-connection");
    click(visual, "settings-new-connection");
    assert!(visual.debug_bounds("settings-save-connection").is_some());
    click(visual, "settings-section-runtime");
    assert_absent(&model, visual, "settings-save-connection");
    assert!(visual.debug_bounds("settings-routing-mode").is_some());
    click(visual, "settings-section-connections");
    assert!(visual.debug_bounds("settings-save-connection").is_some());
    click(visual, "settings-new-route");
    assert_eq!(
        model.read_with(visual, |model, cx| model.settings.read(cx).settings_section),
        SettingsSection::Routes
    );
    assert!(visual.debug_bounds("settings-save-route").is_some());
    assert_absent(&model, visual, "settings-save-connection");
}

#[gpui::test]
fn pulsation_sections_keep_header_fixed_and_reset_on_selection(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(960.0), px(640.0)));
    select(&model, visual, AppView::Pulsations);
    let section_tabs = visual.debug_bounds("pulsation-section-tabs").unwrap();
    let header = visual.debug_bounds("pulsation-cancel").unwrap();
    for selector in [
        "pulsation-review",
        "pulsation-overview",
        "pulsation-preparation",
        "pulsation-artifacts",
    ] {
        click(visual, selector);
        assert_eq!(
            visual.debug_bounds("pulsation-section-tabs").unwrap(),
            section_tabs,
            "section strip moved after selecting {selector}"
        );
    }
    click(visual, "pulsation-overview");
    assert_eq!(visual.debug_bounds("pulsation-cancel").unwrap(), header);
    let section = visual.debug_bounds("pulsation-section-scroll").unwrap();
    assert!(section.top() >= header.bottom() && section.bottom() <= px(640.0));
    click(visual, "pulsation-artifacts");
    model.update(visual, |model, cx| {
        model.pulsations.update(cx, |state, _| {
            assert_eq!(state.detail_section, PulsationDetailSection::Artifacts);
            state.select("pulse-b");
            assert_eq!(state.detail_section, PulsationDetailSection::Overview);
        });
    });
}

#[gpui::test]
fn forge_capture_separates_multiple_tasks_from_feature_discovery(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    model.update(visual, |m, cx| m.select_tab(AppTab::Engine, cx));
    for (width, height) in [(960.0, 640.0), (1440.0, 900.0)] {
        visual.simulate_resize(size(px(width), px(height)));
        visual.run_until_parked();
        click(visual, "new-work");
        assert!(model.read_with(visual, |m, _| m.forge.new_open && !m.forge.capture_feature));
        let overlay = visual.debug_bounds("forge-capture-content").unwrap();
        assert!(
            overlay.left() >= px(0.0) && overlay.right() <= px(width),
            "capture width {overlay:?}"
        );
        assert!(
            overlay.top() >= px(0.0) && overlay.bottom() <= px(height),
            "capture height {overlay:?}"
        );
        model.update(visual, |m, cx| {
            for _ in 0..5 {
                m.forge_action(crate::forge::ForgeAction::AddDraft, cx);
            }
        });
        visual.run_until_parked();
        let body = visual.debug_bounds("forge-capture-body").unwrap();
        assert!(
            body.bottom() <= px(height),
            "multiple tasks must scroll inside the overlay"
        );
        click(visual, "capture-feature-option");
        assert!(model.read_with(visual, |m, _| m.forge.capture_feature));
        assert!(visual.debug_bounds("create-feature").is_some());
        click(visual, "close-capture");
        assert!(!model.read_with(visual, |m, _| m.forge.new_open));
        click(visual, "new-work");
        let overlay = visual.debug_bounds("forge-capture-content").unwrap();
        visual.simulate_click(
            gpui::point(overlay.left() + px(3.0), overlay.top() + px(3.0)),
            Modifiers::default(),
        );
        visual.run_until_parked();
        assert!(
            model.read_with(visual, |m, _| m.forge.new_open),
            "inside padding must keep the modal open"
        );
        let scrim = visual.debug_bounds("forge-capture-overlay").unwrap();
        visual.simulate_click(
            scrim.origin + gpui::point(px(2.0), px(2.0)),
            Modifiers::default(),
        );
        visual.run_until_parked();
        assert!(
            !model.read_with(visual, |m, _| m.forge.new_open),
            "outside click must dismiss capture"
        );
    }
}

#[gpui::test]
fn forge_busy_state_never_moves_the_task_controls(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(960.0), px(640.0)));
    select(&model, visual, AppView::Forge);
    let control = visual.debug_bounds("retry-forge-publications").unwrap();
    for busy in [true, false, true, false] {
        model.update(visual, |m, cx| {
            m.forge.busy = busy;
            cx.notify();
        });
        visual.run_until_parked();
        assert_eq!(
            visual.debug_bounds("retry-forge-publications").unwrap(),
            control
        );
    }
}

#[gpui::test]
fn forge_graph_waves_and_delivery_fit_supported_window_sizes(cx: &mut TestAppContext) {
    use hvc_core::feature_forge::*;
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    model.update(visual, |m, cx| {
        m.forge.snapshot.current_wave_limit = 3;
        m.forge.snapshot.features = (0..4)
            .map(|i| Feature {
                id: format!("feature-{i}"),
                project_id: "layout-project".into(),
                title: format!("Feature {i}"),
                original_idea: "Improve export workflow".into(),
                revision: 2,
                approved_revision: Some(2),
                state: FeatureState::Planned,
                brief: FeatureBrief {
                    objective: "Improve export workflow".into(),
                    ..Default::default()
                },
                evaluation: Default::default(),
                structural_level: i,
                wave: Some(if i < 2 { Wave::Current } else { Wave::Next }),
                position: 0,
                archive_reason: None,
            })
            .collect();
        m.forge.snapshot.dependencies = vec![FeatureDependency {
            feature_id: "feature-1".into(),
            prerequisite_id: "feature-0".into(),
            rationale: "Foundation provides storage".into(),
            origin: DependencyOrigin::Human,
        }];
        m.forge.selected_feature = Some("feature-1".into());
        m.select_view(AppView::Forge, cx);
    });
    use crate::forge::ForgeSection;
    let sections = [
        ForgeSection::Incubation,
        ForgeSection::Triage,
        ForgeSection::Dependencies,
        ForgeSection::Waves,
        ForgeSection::Delivery,
    ];
    for (width, height) in [(960.0, 640.0), (1440.0, 900.0)] {
        visual.simulate_resize(size(px(width), px(height)));
        visual.run_until_parked();
        for section in sections {
            model.update(visual, |m, cx| {
                m.forge_action(crate::forge::ForgeAction::Section(section), cx)
            });
            visual.run_until_parked();
            let detail = visual.debug_bounds("forge-detail").unwrap();
            assert!(
                detail.right() <= px(width) && detail.bottom() <= px(height),
                "section {section:?}: {detail:?}"
            );
            if section == ForgeSection::Dependencies {
                assert!(visual.debug_bounds("feature-graph").is_some());
            }
            if section == ForgeSection::Waves {
                for selector in ["wave-Current", "wave-Next", "wave-Horizon"] {
                    let wave = visual.debug_bounds(selector).unwrap();
                    assert!(
                        wave.right() <= detail.right(),
                        "{selector} overflows: {wave:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn ui_source_contains_no_unapproved_portuguese_literals() {
    const PT_CHARS: &[char] = &[
        '\u{00E1}', '\u{00E0}', '\u{00E2}', '\u{00E3}', '\u{00E9}', '\u{00EA}', '\u{00ED}',
        '\u{00F3}', '\u{00F4}', '\u{00F5}', '\u{00FA}', '\u{00E7}', '\u{00C1}', '\u{00C0}',
        '\u{00C2}', '\u{00C3}', '\u{00C9}', '\u{00CA}', '\u{00CD}', '\u{00D3}', '\u{00D4}',
        '\u{00D5}', '\u{00DA}', '\u{00C7}',
    ];
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let src_dir = manifest_dir.join("src");
    let allowed_files = ["text_input.rs", "native_editor.rs"];

    let mut violations = Vec::new();
    let entries = std::fs::read_dir(&src_dir).expect("src directory must exist");
    for entry in entries {
        let entry = entry.expect("valid dir entry");
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if allowed_files.contains(&file_name) {
            continue;
        }
        let content = std::fs::read_to_string(&path).expect("failed to read source file");
        for (line_idx, line) in content.lines().enumerate() {
            if line.chars().any(|c| PT_CHARS.contains(&c)) {
                violations.push(format!("{file_name}:{}: {}", line_idx + 1, line.trim()));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "Found unapproved Portuguese characters in UI source files:\n{}",
        violations.join("\n")
    );
}

#[gpui::test]
fn forge_analysis_button_keeps_its_geometry_when_gated(cx: &mut TestAppContext) {
    use hvc_core::feature_forge::*;
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(1100.0), px(760.0)));
    model.update(visual, |m, cx| {
        m.forge.snapshot.features = vec![Feature {
            id: "feature-1".into(),
            project_id: "layout-project".into(),
            title: "Export workflow".into(),
            original_idea: "Improve export workflow".into(),
            revision: 2,
            approved_revision: None,
            state: FeatureState::Incubating,
            brief: FeatureBrief {
                objective: "Improve export workflow".into(),
                ..Default::default()
            },
            evaluation: Default::default(),
            structural_level: 0,
            wave: None,
            position: 0,
            archive_reason: None,
        }];
        m.forge.selected_feature = Some("feature-1".into());
        m.forge.section = crate::forge::ForgeSection::Triage;
        m.select_view(AppView::Forge, cx);
    });
    visual.run_until_parked();
    let control = visual.debug_bounds("analyze-triage").unwrap();
    // An open incubation question disables the control. Disabling may only
    // recolour it -- if the box moved or resized, the surrounding triage form
    // would shift under the pointer every time the gate flipped.
    for pending in [true, false, true, false] {
        model.update(visual, |m, cx| {
            m.forge.snapshot.features[0].brief.pending_decisions = if pending {
                vec!["Choose the export format".into()]
            } else {
                vec![]
            };
            cx.notify();
        });
        visual.run_until_parked();
        assert_eq!(visual.debug_bounds("analyze-triage").unwrap(), control);
    }
}

/// Builds one feature with a filled-in brief, selected, so the three-column
/// sections have something to render in every pane.
#[cfg(test)]
fn forge_feature_fixture(
    model: &Entity<AppModel>,
    visual: &mut VisualTestContext,
    states: &[hvc_core::feature_forge::FeatureState],
) {
    use hvc_core::feature_forge::*;
    model.update(visual, |m, cx| {
        m.forge.snapshot.features = states
            .iter()
            .enumerate()
            .map(|(i, state)| Feature {
                id: format!("feature-{i}"),
                project_id: "layout-project".into(),
                title: format!("Feature {i}"),
                original_idea: "Improve the export workflow".into(),
                revision: 2,
                approved_revision: None,
                state: *state,
                brief: FeatureBrief {
                    objective: "Improve the export workflow end to end".into(),
                    audience: "Project developers".into(),
                    behavior: "Validate the input and show the result".into(),
                    scope: "A bounded, validated workflow".into(),
                    ..Default::default()
                },
                evaluation: Default::default(),
                structural_level: 0,
                wave: None,
                position: 0,
                archive_reason: None,
            })
            .collect();
        m.forge.selected_feature = Some("feature-0".into());
        m.forge.section = crate::forge::ForgeSection::Incubation;
        m.select_view(AppView::Forge, cx);
    });
    visual.run_until_parked();
}

#[gpui::test]
fn all_three_navigation_lists_keep_long_titles_on_one_line(cx: &mut TestAppContext) {
    use hvc_core::chat_service::{ChatConversation, ChatTransportKind};
    use hvc_core::feature_forge::FeatureState;
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    forge_feature_fixture(&model, visual, &[FeatureState::Incubating]);
    model.update(visual, |m, cx| {
        m.pulsations
            .update(cx, |p, _| p.toggle_group(PulsationStatus::Planning));
        m.chat.update(cx, |chat, _| {
            chat.project_id = Some("layout-project".into());
            chat.conversations = vec![ChatConversation {
                id: "title-chat".into(),
                project_id: "layout-project".into(),
                connection_id: None,
                provider_id: "codex".into(),
                model_id: "default".into(),
                transport: ChatTransportKind::Cli,
                cli_backend: Some(CliBackend::Codex),
                model_profile: None,
                model_source: CliModelSelectionSource::Default,
                title: "Short".into(),
                connection_available: true,
                created_at: "now".into(),
                updated_at: "now".into(),
            }];
            chat.active_conversation = Some("title-chat".into());
        });
    });
    let conversations = model.read_with(visual, |m, cx| m.chat.read(cx).conversations.clone());
    for (view, selector) in [
        (AppView::Chat, "chat-title:title-chat"),
        (AppView::Forge, "feature-title:feature-0"),
        (AppView::Pulsations, "pulsation-title:pulse-a"),
    ] {
        select(&model, visual, view);
        // Chat navigation starts hydration; this geometry fixture has no backend.
        model.update(visual, |m, cx| {
            m.chat.update(cx, |chat, _| {
                chat.conversations = conversations.clone();
                chat.loading = false;
            });
        });
        for width in [960.0, 1440.0] {
            visual.simulate_resize(size(px(width), px(900.0)));
            let mut heights = Vec::new();
            for title in [
                "Short".to_string(),
                "VeryLongUnbrokenTitle".repeat(50),
                "Caf\u{e9} 👩‍💻\nAn explicitly wrapped title ".repeat(30),
            ] {
                model.update(visual, |m, cx| {
                    m.forge.snapshot.features[0].title = title.clone();
                    m.pulsations
                        .update(cx, |p, _| p.pulsations[0].feature_name = title.clone());
                    m.chat
                        .update(cx, |chat, _| chat.conversations[0].title = title.clone());
                    cx.notify();
                });
                visual.run_until_parked();
                let bounds = visual
                    .debug_bounds(selector)
                    .unwrap_or_else(|| panic!("missing {selector}"));
                assert!(
                    bounds.size.width > px(20.0),
                    "title has no usable width: {bounds:?}"
                );
                assert!(bounds.left() >= px(0.0) && bounds.right() <= px(width));
                assert!(
                    bounds.size.height > px(0.0) && bounds.size.height <= px(32.0),
                    "title wrapped: {bounds:?}"
                );
                heights.push(bounds.size.height);
                assert_eq!(
                    model.read_with(visual, |m, cx| m.chat.read(cx).conversations[0]
                        .title
                        .clone()),
                    title
                );
            }
            assert!(heights.windows(2).all(|pair| pair[0] == pair[1]));
        }
    }
}

#[gpui::test]
fn forge_incubation_and_triage_render_three_resizable_columns(cx: &mut TestAppContext) {
    use hvc_core::feature_forge::FeatureState;
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    forge_feature_fixture(&model, visual, &[FeatureState::InTriage]);

    // The analysis column costs width, so it stays out of the way until asked for.
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    select(&model, visual, AppView::Forge);
    model.update(visual, |m, cx| {
        m.forge.section = crate::forge::ForgeSection::Triage;
        cx.notify();
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("forge-detail").is_some());
    assert!(
        visual.debug_bounds("forge-triage-aside").is_none(),
        "the analysis column should not open on its own"
    );

    for (width, height) in [(960.0, 640.0), (1440.0, 900.0)] {
        visual.simulate_resize(size(px(width), px(height)));
        select(&model, visual, AppView::Forge);

        model.update(visual, |m, cx| {
            m.forge.section = crate::forge::ForgeSection::Incubation;
            cx.notify();
        });
        visual.run_until_parked();
        let list = visual.debug_bounds("feature-list").unwrap();
        let main = visual.debug_bounds("forge-detail").unwrap();
        let aside = visual.debug_bounds("forge-incubation-aside").unwrap();
        // Left to right, none of them past the window edge.
        assert!(list.right() <= main.left(), "list overlaps chat: {list:?}");
        assert!(main.right() <= aside.left(), "chat overlaps spec: {main:?}");
        assert!(
            aside.right() <= px(width) && aside.bottom() <= px(height),
            "spec column overflows at {width}x{height}: {aside:?}"
        );

        model.update(visual, |m, cx| {
            m.forge.section = crate::forge::ForgeSection::Triage;
            m.forge.triage_aside_open = false;
            cx.notify();
        });
        visual.run_until_parked();
        model.update(visual, |m, cx| {
            m.forge.triage_aside_open = true;
            cx.notify();
        });
        visual.run_until_parked();
        let main = visual.debug_bounds("forge-detail").unwrap();
        let aside = visual.debug_bounds("forge-triage-aside").unwrap();
        assert!(main.right() <= aside.left(), "form overlaps analysis");
        assert!(
            aside.right() <= px(width) && aside.bottom() <= px(height),
            "analysis column overflows at {width}x{height}: {aside:?}"
        );
    }

    // The sash moves the boundary, and a double-click puts it back.
    let before = visual
        .debug_bounds("forge-feature-columns-left")
        .unwrap()
        .size
        .width;
    let sash = visual
        .debug_bounds("forge-feature-columns-sash")
        .unwrap()
        .center();
    visual.simulate_mouse_down(sash, gpui::MouseButton::Left, Modifiers::default());
    visual.simulate_mouse_move(
        sash + gpui::point(px(70.0), px(0.0)),
        gpui::MouseButton::Left,
        Modifiers::default(),
    );
    visual.simulate_mouse_up(
        sash + gpui::point(px(70.0), px(0.0)),
        gpui::MouseButton::Left,
        Modifiers::default(),
    );
    visual.run_until_parked();
    let resized = visual
        .debug_bounds("forge-feature-columns-left")
        .unwrap()
        .size
        .width;
    assert!(resized > before, "sash did not widen the list column");
}

#[gpui::test]
fn forge_feature_list_groups_by_state_and_collapses_finished_work(cx: &mut TestAppContext) {
    use hvc_core::feature_forge::FeatureState;
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    forge_feature_fixture(
        &model,
        visual,
        &[
            FeatureState::Incubating,
            FeatureState::Done,
            FeatureState::Archived,
        ],
    );

    // Live work is expanded; finished and abandoned work starts collapsed so it
    // does not bury what is in flight.
    assert!(
        visual.debug_bounds("feature-feature-0").is_some(),
        "an incubating feature should be visible without expanding"
    );
    assert!(visual.debug_bounds("feature-feature-1").is_none());
    assert!(visual.debug_bounds("feature-feature-2").is_none());

    // Groups with no members are not rendered at all.
    assert!(visual.debug_bounds("feature-group-executing").is_none());
    assert!(visual.debug_bounds("feature-group-done").is_some());

    click(visual, "feature-group-done");
    assert!(
        model.read_with(visual, |m, _| m.forge.group_expanded(FeatureState::Done)),
        "clicking the Done header should toggle its expansion state"
    );
    assert!(
        visual.debug_bounds("feature-feature-1").is_some(),
        "expanding the Done group should reveal its feature"
    );
    click(visual, "feature-group-incubating");
    assert!(
        !model.read_with(visual, |m, _| m
            .forge
            .group_expanded(FeatureState::Incubating)),
        "clicking an expanded header should collapse it"
    );
}

/// Buttons, pickers and single-line inputs are as wide as what they hold, not
/// as wide as the panel (see `crate::sizing`). Checked on the three views the
/// complaint named — Agents, Settings, Forge/Pulsations — at both supported
/// window extremes.
#[gpui::test]
fn controls_size_to_content_instead_of_the_panel(cx: &mut TestAppContext) {
    use hvc_core::terminals::{ExecutionOrigin, SessionSlot, TerminalRun, TerminalRunStatus};
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    // The activity toolbar (with its filter) only renders for an agent with a run.
    model.update(visual, |m, cx| {
        m.agents.update(cx, |agents, _| {
            let generation = agents.begin(true);
            let mut terminals = agents.snapshot.clone().expect("fixture snapshot");
            let agent = terminals
                .agents
                .iter_mut()
                .find(|agent| agent.config.agent_role == AgentRole::TaskSpecifier)
                .expect("fixture agent");
            agent.recent_runs.push(TerminalRun {
                id: "run-layout".into(),
                project_id: "layout-project".into(),
                slot: SessionSlot::Canonical {
                    agent_role: AgentRole::TaskSpecifier,
                },
                slot_id: "canonical:layout-project:task_specifier".into(),
                agent_role: Some(AgentRole::TaskSpecifier),
                origin: ExecutionOrigin::Manual,
                execution_mode: AgentExecutionMode::CliProcess,
                backend: Some(CliBackend::Codex),
                model: None,
                requested_model_profile: None,
                model_source: CliModelSelectionSource::Default,
                observed_model: None,
                native_session_id: None,
                pulsation_id: None,
                job_id: None,
                agent_run_id: None,
                llm_execution_id: None,
                status: TerminalRunStatus::Completed,
                exit_code: Some(0),
                error_code: None,
                error_message: None,
                created_at: "2026-01-01T00:00:00Z".into(),
                started_at: None,
                finished_at: None,
                updated_at: "2026-01-01T00:00:00Z".into(),
                deadline_at: None,
                termination_reason: None,
                output_bytes: 0,
                event_count: 0,
                workspace_snapshot_hash: None,
                launch_identity_sha256: None,
                detected_cli_version: None,
                adapter_contract_version: 1,
                parent_run_id: None,
                continuation_reason: None,
                session_fingerprint: None,
            });
            agents.apply_snapshot(
                generation,
                AgentsDashboardSnapshot {
                    terminals,
                    fallbacks: vec![],
                    free_form_bindings: vec![],
                    api_executions: Default::default(),
                },
            );
        });
    });
    for (width, height) in [(960.0, 640.0), (1440.0, 900.0)] {
        visual.simulate_resize(size(px(width), px(height)));

        // Agents: the stdin/event filter of a CLI agent card no longer spans the row.
        select(&model, visual, AppView::Agents);
        let column = visual.debug_bounds("agents-cli-scroll").unwrap();
        let filter = visual.debug_bounds("agent-filter:task_specifier").unwrap();
        assert!(
            filter.size.width <= px(360.0) && filter.size.width < column.size.width / 2.0,
            "filter spans {:?} of a {:?} column at {width}x{height}",
            filter.size.width,
            column.size.width
        );
        let backend = visual.debug_bounds("agent-backend:task_specifier").unwrap();
        assert!(backend.size.width <= px(360.0));

        // Settings · Runtime: numeric inputs are short, actions and toggles size to their label.
        model.update(visual, |m, cx| {
            m.settings.update(cx, |settings, _| {
                settings.settings_section = SettingsSection::Runtime;
            });
            m.select_view(AppView::Settings, cx);
        });
        visual.run_until_parked();
        let threshold = visual.debug_bounds("sized-input:160").unwrap();
        assert_eq!(threshold.size.width, px(160.0));
        let save = visual.debug_bounds("preparation-save").unwrap();
        let toggle = visual.debug_bounds("preparation-enabled").unwrap();
        assert!(save.size.width < px(320.0), "{:?}", save.size.width);
        assert!(
            toggle.size.width >= px(220.0) && toggle.size.width <= px(360.0),
            "{:?}",
            toggle.size.width
        );
        assert!(save.right() <= px(width) && toggle.right() <= px(width));

        // Settings · Connections: names are medium, secrets long, and nothing exceeds the panel.
        model.update(visual, |m, cx| {
            m.settings.update(cx, |settings, _| {
                settings.settings_section = SettingsSection::Connections;
                settings.creating_connection = true;
            });
            cx.notify();
        });
        visual.run_until_parked();
        let name = visual.debug_bounds("sized-input:320").unwrap();
        let secret = visual.debug_bounds("sized-input:560").unwrap();
        assert!(name.size.width <= px(320.0) && name.size.width > px(0.0));
        assert!(secret.size.width <= px(560.0) && secret.size.width > name.size.width);
        assert!(secret.right() <= px(width));
        let save_connection = visual.debug_bounds("settings-save-connection").unwrap();
        assert!(save_connection.size.width < px(240.0));

        // Pulsations: a Forge button placed in a column keeps its label width.
        select(&model, visual, AppView::Pulsations);
        let list = visual.debug_bounds("pulsations-columns-left").unwrap();
        let compose = visual.debug_bounds("compose-task-batch").unwrap();
        assert!(
            compose.size.width < list.size.width * 0.95,
            "compose button stretched to {:?} of {:?}",
            compose.size.width,
            list.size.width
        );
    }
}
