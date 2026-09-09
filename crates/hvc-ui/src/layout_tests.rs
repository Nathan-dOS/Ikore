use gpui::{
    Context, Entity, IntoElement, Modifiers, Render, TestAppContext, VisualTestContext, Window,
    div, prelude::*, px, size,
};
use hvc_core::llm::{AgentRole, LlmResilienceSnapshot};
use hvc_core::persistence_service::{Pulsation, PulsationStage, PulsationStatus};
use hvc_core::terminals::{
    AgentExecutionMode, AgentTerminalSnapshot, CliAgentConfig, CliBackend, CliModelSelectionSource,
    TerminalsSnapshot, ThinkingLevel,
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
                    free_form_slots: vec![],
                    archived_free_form_slots: vec![],
                    discovery: vec![],
                },
                fallbacks: vec![],
            },
        );
    });
    model.settings.update(cx, |settings, _| {
        settings.snapshot = Some(SettingsSnapshot {
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
    let settings = visual.debug_bounds("settings").unwrap();
    assert!(settings.right() <= px(960.0));
    assert!(visual.debug_bounds("toggle-sidebar").is_none());
    for (width, height) in [(960.0, 640.0), (1440.0, 900.0)] {
        visual.simulate_resize(size(px(width), px(height)));
        visual.run_until_parked();
        let agent = visual.debug_bounds("agent").unwrap();
        assert!(
            agent.left() < px(50.0),
            "branding must not reserve header space"
        );
        for selector in ["agent", "engine", "companion", "settings"] {
            let control = visual.debug_bounds(selector).unwrap();
            assert!(control.left() >= px(0.0) && control.right() <= px(width));
            assert!(control.top() >= px(0.0) && control.bottom() <= px(58.0));
        }
    }
}

#[gpui::test]
fn agent_selection_and_modal_preserve_detail_geometry(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(960.0), px(640.0)));
    select(&model, visual, AppView::Agents);
    assert!(visual.debug_bounds("agent-card:task_specifier").is_some());
    assert!(visual.debug_bounds("agent-card:backend_agent").is_none());
    click(visual, "agent-select:backend_agent");
    assert_absent(&model, visual, "agent-card:task_specifier");
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
    assert_absent(&model, visual, "cli-model-picker-overlay");
    model.update(visual, |model, cx| {
        model.set_agents_tab(crate::agents::AgentsTab::Api, cx)
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("api-agent:backend_agent").is_some());
    assert_absent(&model, visual, "api-agent:task_specifier");
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
    let header = visual.debug_bounds("pulsation-cancel").unwrap();
    click(visual, "pulsation-artifacts");
    assert_eq!(visual.debug_bounds("pulsation-cancel").unwrap(), header);
    let section = visual.debug_bounds("pulsation-section-scroll").unwrap();
    assert!(section.top() >= header.bottom() && section.bottom() <= px(640.0));
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
