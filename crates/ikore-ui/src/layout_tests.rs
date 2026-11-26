use gpui::{
    Context, Entity, IntoElement, Modifiers, Render, ScrollDelta, ScrollWheelEvent, TestAppContext,
    VisualTestContext, Window, div, point, prelude::*, px, size,
};
use ikore_core::llm::{AgentRole, LlmResilienceSnapshot};
use ikore_core::persistence_service::{Pulsation, PulsationStage, PulsationStatus};
use ikore_core::terminals::{
    AgentExecutionMode, AgentTerminalSnapshot, CliAgentConfig, CliBackend, CliModelSelectionSource,
    FreeFormTerminalSnapshot, SessionPolicy, TerminalSlot, TerminalsSnapshot, ThinkingLevel,
};

use crate::agents::{
    AgentIdentifier, AgentsDashboardSnapshot, CliModelPickerState, CliModelPickerTarget,
};
use crate::pulsations::PulsationDetailSection;
use crate::settings::{SettingsPage, SettingsSnapshot};
use crate::{AppModel, AppView};

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
                                cli_silence_timeout_ms: None,
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
                    automation_slots: Vec::new(),
                    discovery: vec![],
                },
                fallbacks: vec![],
                free_form_bindings: vec![],
                api_executions: Default::default(),
                quota_deltas: Default::default(),
                live_activity: Vec::new(),
                pipelines: Default::default(),
                verifiers: Default::default(),
            },
        );
    });
    model.settings.update(cx, |settings, _| {
        settings.snapshot = Some(SettingsSnapshot {
            projects: vec![],
            pending_project_deletions: 0,
            composition: None,
            preparation: Default::default(),
            config: Default::default(),
            profiles: vec![],
            connections: vec![],
            api_model_catalogs: vec![],
            routes: vec![],
            bindings: vec![],
            key_statuses: vec![],
            resilience: LlmResilienceSnapshot {
                generated_at: "now".into(),
                states: vec![],
            },
            attestation: None,
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
fn a_long_status_group_renders_its_newest_cards_until_the_rest_is_requested(
    cx: &mut TestAppContext,
) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    model.update(visual, |model, cx| {
        model.pulsations.update(cx, |state, _| {
            state.pulsations = (0..40)
                .map(|index| Pulsation {
                    id: format!("failed-{index}"),
                    project_id: "layout-project".into(),
                    feature_name: format!("Attempt {index}"),
                    stage: PulsationStage::Code,
                    status: PulsationStatus::Failed,
                    terminal_reason: None,
                    created_at: "now".into(),
                    updated_at: "now".into(),
                })
                .collect();
            state.select("failed-35");
            state.toggle_group(PulsationStatus::Failed);
        });
    });
    select(&model, visual, AppView::Pulsations);
    assert!(visual.debug_bounds("pulsation-title:failed-0").is_some());
    assert!(visual.debug_bounds("pulsation-title:failed-24").is_some());
    // The selected pulsation stays in the list even past the limit.
    assert!(visual.debug_bounds("pulsation-title:failed-35").is_some());
    assert!(visual.debug_bounds("pulsation-title:failed-30").is_none());
    // The row sits below the fold of the scrolling list, so drive its effect directly.
    assert!(visual.debug_bounds("pulsation-group-more-failed").is_some());
    model.update(visual, |model, cx| {
        model.pulsations.update(cx, |state, _| {
            state.groups_showing_all.insert("failed");
        });
        cx.notify();
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("pulsation-title:failed-30").is_some());
    // No absence check for the "more" row: `assert_absent` renders a second window, and the
    // Pulsations split re-measures forever across two widths (harness note in the UI docs).
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
fn review_clipboard_uses_a_sanitized_derivative_of_the_candidate(cx: &mut TestAppContext) {
    use ikore_core::persistence_service::{HitlCheckpoint, HitlReviewDocument};
    let secret = "sk-proj-syntheticabcdefghijklmnop";
    let original = format!("# Candidate\n\nconst KEY = \"{secret}\";\n");
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    model.update(visual, |model, cx| {
        model.pulsations.update(cx, |state, _| {
            state.pulsations[0].status = PulsationStatus::Checkpoint;
            let pulse = state.pulsations[0].clone();
            state.hitl.load_checkpoint(
                Some(&pulse),
                Some(HitlCheckpoint {
                    pulsation_id: pulse.id.clone(),
                    checkpoint_id: "secret-review".into(),
                    state_version: 1,
                }),
            );
            let (generation, checkpoint) = state.hitl.begin_documents().unwrap();
            state.hitl.finish_documents(
                generation,
                &checkpoint,
                Ok(vec![HitlReviewDocument {
                    artifact_id: "secret-artifact".into(),
                    name: "Candidate".into(),
                    kind: "stage_output".into(),
                    relative_path: Some("src/lib.rs".into()),
                    content: original.clone(),
                }]),
            );
            state.detail_section = PulsationDetailSection::Review;
        });
    });
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    select(&model, visual, AppView::Pulsations);
    click(visual, "review-copy-document");
    visual.update(|_, cx| {
        let copied = cx.read_from_clipboard().unwrap().text().unwrap();
        assert!(!copied.contains(secret));
        assert!(copied.contains("<REDACTED_SECRET:"));
    });
    assert!(original.contains(secret));
    assert_eq!(
        model.read_with(visual, |m, cx| m.pulsations.read(cx).hitl.documents[0]
            .artifact
            .content
            .clone()),
        original
    );
}

#[gpui::test]
fn complete_review_and_workspace_readers_fit_small_and_large_windows(cx: &mut TestAppContext) {
    use ikore_core::persistence_service::{HitlCheckpoint, HitlReviewDocument};
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
        // Edit and Preview share one surface; the toggle swaps them.
        model.update(visual, |model, cx| model.set_workspace_preview(false, cx));
        visual.run_until_parked();
        let editor = visual.debug_bounds("workspace-editor-viewport").unwrap();
        assert!(editor.size.height > px(100.0));
        assert!(editor.right() <= px(width) && editor.bottom() <= px(height));
        click(visual, "workspace-mode-preview");
        let preview = visual.debug_bounds("workspace-markdown-preview").unwrap();
        assert!(preview.size.height > px(100.0));
        assert!(
            preview.right() <= px(width) && preview.bottom() <= px(height),
            "window={width}x{height}; editor={editor:?}; preview={preview:?}"
        );
        // Same place, not side by side: the preview starts where the editor was, not after it.
        assert!(preview.left() < editor.right());
        model.update(visual, |model, cx| model.set_workspace_preview(false, cx));
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

fn specification_brief() -> ikore_core::orchestration::review_brief::ReviewBrief {
    use ikore_core::orchestration::composition::{InterfaceIntent, ProposalKind, VerificationMode};
    use ikore_core::orchestration::review_brief::*;
    let criterion = |id: &str, verification| BriefCriterion {
        criterion_id: id.into(),
        description: format!(
            "{id}: the export keeps one row per pulsation and model, with token totals that match the Usage view for the same filter."
        ),
        requirement: "Requirement text.".into(),
        verification,
        verification_intent: "Run the export test.".into(),
        checks: vec!["core_tests".into()],
        not_applicable_reason: None,
    };
    let unit = |key: &str, kind, depends_on: &[&str]| BriefUnit {
        key: key.into(),
        kind,
        destinations: vec![
            "crates/ikore-core/src/usage.rs".into(),
            "crates/ikore-core/src/usage/export.rs".into(),
        ],
        criteria: vec!["AC-1".into(), "AC-2".into(), "AC-3".into()],
        depends_on: depends_on.iter().map(|key| (*key).into()).collect(),
        objective: "Write the export.".into(),
        interfaces: Vec::new(),
    };
    ReviewBrief {
        stage: PulsationStage::Specification,
        next_stage: Some(PulsationStage::Contract),
        objective: Some("Let developers export token usage to CSV from the Usage view, one row per pulsation and model, with totals that match the app.".into()),
        counts: vec![
            "2 tasks".into(),
            "6 criteria · 5 executed, 1 by reading".into(),
            "4 files · 1 new".into(),
            "1 existing interface changed".into(),
            "3 work units".into(),
        ],
        revision: None,
        judgement: vec![
            JudgementItem {
                kind: JudgementKind::Decision,
                label: "D1".into(),
                text: "Export aggregates per pulsation and model, not one row per LLM call.".into(),
                detail: None,
                refs: Vec::new(),
            },
            JudgementItem {
                kind: JudgementKind::ReadingOnlyProof,
                label: "AC-4".into(),
                text: "An empty date range writes only the header row.".into(),
                detail: Some("Read the early return in write_usage_csv and confirm the header is written before iterating.".into()),
                refs: Vec::new(),
            },
            JudgementItem {
                kind: JudgementKind::InterfaceChange,
                label: "crates/ikore-ui/src/usage.rs".into(),
                text: "The Usage view gains an Export button that is disabled while a query loads.".into(),
                detail: None,
                refs: vec!["crates/ikore-ui/src/usage.rs".into(), "AC-6".into()],
            },
            JudgementItem {
                kind: JudgementKind::Exclusions,
                label: "2".into(),
                text: "Scheduled or automatic exports · Exporting one row per LLM call".into(),
                detail: None,
                refs: Vec::new(),
            },
        ],
        verified: vec![
            "Each of the 6 acceptance criteria has exactly one requirement.".into(),
            "The 3 existing files it cites were read from the project.".into(),
            "The new file it plans is a registered destination inside the project.".into(),
            "Its 3 work units form no dependency cycle, and a code unit covers every criterion.".into(),
        ],
        visual: StageVisual::Specification(SpecificationVisual {
            facts: BriefFacts {
                tasks: 2,
                criteria: 6,
                executed: 5,
                by_reading: 1,
                not_applicable: 0,
                existing_files: 3,
                planned_files: 1,
                changed_interfaces: 1,
                units: 3,
            },
            exclusions: vec![
                "Scheduled or automatic exports".into(),
                "Exporting one row per LLM call".into(),
            ],
            tasks: vec![
                BriefTask {
                    task_id: "4b8e0c62-3d1f-4a77-9b25-6c0e8f2a9d14".into(),
                    title: "Export usage summary to CSV".into(),
                    criteria: ["AC-1", "AC-2", "AC-3", "AC-5"]
                        .into_iter()
                        .map(|id| criterion(id, VerificationMode::ExecutedCheck))
                        .chain([criterion("AC-4", VerificationMode::DocumentCheck)])
                        .collect(),
                },
                BriefTask {
                    task_id: "9f31d7a0-5e2c-4c18-8d64-2b7a1e0c5f83".into(),
                    title: "Expose CSV export in the Usage view".into(),
                    criteria: vec![criterion("AC-6", VerificationMode::ExecutedCheck)],
                },
            ],
            files: vec![
                BriefFileGroup {
                    root: "crates/ikore-core".into(),
                    files: vec![
                        BriefFile {
                            path: "src/usage.rs".into(),
                            planned: false,
                            interfaces: vec![BriefInterface {
                                intent: InterfaceIntent::Preserve,
                                description: "query_usage keeps its signature.".into(),
                            }],
                        },
                        BriefFile {
                            path: "src/usage/export.rs".into(),
                            planned: true,
                            interfaces: Vec::new(),
                        },
                    ],
                },
                BriefFileGroup {
                    root: "crates/ikore-ui".into(),
                    files: vec![BriefFile {
                        path: "src/usage.rs".into(),
                        planned: false,
                        interfaces: vec![BriefInterface {
                            intent: InterfaceIntent::Change,
                            description: "The Usage view gains an Export button.".into(),
                        }],
                    }],
                },
            ],
            unattached_interfaces: vec![BriefInterface {
                intent: InterfaceIntent::New,
                description: "write_usage_csv is a new public function.".into(),
            }],
            units: vec![
                unit("core_export", ProposalKind::Code, &[]),
                unit("usage_tests", ProposalKind::Test, &["core_export"]),
                unit("ui_export", ProposalKind::Code, &["core_export"]),
            ],
        }),
    }
}

/// Parks the Specification brief fixture on the Review tab of a new window.
fn open_specification_brief(cx: &mut TestAppContext) -> (Entity<AppModel>, &mut VisualTestContext) {
    open_brief(cx, specification_brief())
}

/// Parks `brief` on the Review tab of a new window, behind two documents.
fn open_brief(
    cx: &mut TestAppContext,
    brief: ikore_core::orchestration::review_brief::ReviewBrief,
) -> (Entity<AppModel>, &mut VisualTestContext) {
    use ikore_core::persistence_service::{HitlCheckpoint, HitlReviewDocument};
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.update(|_, cx| crate::hitl::register_hitl_keybindings(cx));
    visual.update(|_, cx| crate::shell::register_navigation_keybindings(cx));
    model.update(visual, |m, cx| {
        m.pulsations.update(cx, |state, _| {
            state.pulsations[0].status = PulsationStatus::Checkpoint;
            let pulse = state.pulsations[0].clone();
            let checkpoint = HitlCheckpoint {
                pulsation_id: pulse.id.clone(),
                checkpoint_id: "brief-checkpoint".into(),
                state_version: 1,
            };
            state.hitl.load_checkpoint(Some(&pulse), Some(checkpoint));
            let (generation, checkpoint) = state.hitl.begin_documents().unwrap();
            state.hitl.finish_documents(
                generation,
                &checkpoint,
                Ok(vec![
                    HitlReviewDocument {
                        artifact_id: "spec-note".into(),
                        name: "knowledge/obsidian/features/usage-export-to-csv.md".into(),
                        kind: "specification".into(),
                        relative_path: Some(
                            "knowledge/obsidian/features/usage-export-to-csv.md".into(),
                        ),
                        content: "# Specification\n\nThe complete official document.".into(),
                    },
                    HitlReviewDocument {
                        artifact_id: "spec-payload".into(),
                        name: "specification payload".into(),
                        kind: "stage_payload_v1".into(),
                        relative_path: None,
                        content: "{}".into(),
                    },
                ]),
            );
            state
                .hitl
                .finish_brief(generation, &checkpoint, Ok(Some(brief)));
            state.detail_section = PulsationDetailSection::Review;
        });
    });
    (model, visual)
}

#[gpui::test]
fn specification_brief_opens_first_and_fits_small_and_large_windows(cx: &mut TestAppContext) {
    let (model, visual) = open_specification_brief(cx);
    for (width, height) in [(960.0, 640.0), (1440.0, 900.0)] {
        visual.simulate_resize(size(px(width), px(height)));
        select(&model, visual, AppView::Pulsations);
        let tab = visual.debug_bounds("review-brief-tab").unwrap();
        let first_document = visual.debug_bounds("review-document-tab-0").unwrap();
        let reader = visual.debug_bounds("review-document-reader").unwrap();
        let actions = visual.debug_bounds("review-decision-bar").unwrap();
        assert!(tab.right() <= first_document.left(), "the Brief tab leads");
        assert!(tab.bottom() <= reader.top());
        assert!(
            reader.size.height >= px(100.0),
            "reader too small: {reader:?}"
        );
        assert!(reader.bottom() <= actions.top() && actions.bottom() <= px(height));
        for action in ["hitl-approve", "hitl-revision", "hitl-reject"] {
            let button = visual.debug_bounds(action).unwrap();
            assert!(button.left() >= actions.left() && button.right() <= actions.right());
        }
        for section in [
            "review-brief",
            "brief-decision",
            "brief-judgement",
            "brief-verified",
            "brief-criteria",
            "brief-files",
            "brief-units",
        ] {
            let bounds = visual
                .debug_bounds(section)
                .unwrap_or_else(|| panic!("missing {section} at {width}x{height}"));
            assert!(
                bounds.left() >= reader.left() && bounds.right() <= reader.right() + px(1.0),
                "{section} overflows the reader at {width}x{height}: {bounds:?} vs {reader:?}"
            );
        }
        // The two cards of a row sit side by side only when both keep their minimum width,
        // and a shorter card keeps its own height instead of stretching to its neighbour.
        let judgement = visual.debug_bounds("brief-judgement").unwrap();
        let verified = visual.debug_bounds("brief-verified").unwrap();
        assert!(
            verified.top() >= judgement.bottom() || verified.left() >= judgement.right(),
            "cards overlap at {width}x{height}"
        );
        if verified.left() >= judgement.right() {
            assert!(verified.size.height < judgement.size.height);
        }
        // Document tabs keep a readable one-line label (they collapsed to "1" before).
        for tab in ["review-document-tab-0", "review-document-tab-1"] {
            let bounds = visual.debug_bounds(tab).unwrap();
            assert!(
                bounds.size.width >= px(120.0),
                "{tab} collapsed at {width}x{height}: {bounds:?}"
            );
        }
        // Task headers are one line: an internal id once rendered as a column of characters.
        for task in ["brief-task-0", "brief-task-1"] {
            let header = visual.debug_bounds(task).unwrap();
            assert!(
                header.size.height <= px(32.0),
                "{task} is {header:?} at {width}x{height}"
            );
        }
        model.read_with(visual, |model, cx| {
            let hitl = &model.pulsations.read(cx).hitl;
            assert!(hitl.brief_selected);
            assert!(hitl.can_approve(), "the documents still gate approval");
        });
    }
    click(visual, "review-document-tab-0");
    model.read_with(visual, |model, cx| {
        assert!(!model.pulsations.read(cx).hitl.brief_selected)
    });
    assert!(visual.debug_bounds("review-copy-document").is_some());
    click(visual, "review-brief-tab");
    model.read_with(visual, |model, cx| {
        assert!(model.pulsations.read(cx).hitl.brief_selected)
    });
}

#[gpui::test]
fn review_without_interactive_brief_keeps_shortcuts_safe_and_tabs_through_controls(
    cx: &mut TestAppContext,
) {
    let (model, visual) = open_specification_brief(cx);
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    select(&model, visual, AppView::Pulsations);
    assert!(visual.debug_bounds("review-decision-bar").is_some());
    assert!(visual.debug_bounds("review-document-reader").is_some());
    for missing_brief in [false, true] {
        model.update(visual, |model, cx| {
            model.pulsations.update(cx, |pulsations, _| {
                let hitl = &mut pulsations.hitl;
                hitl.brief_selected = false;
                if missing_brief {
                    hitl.brief = None;
                    hitl.marks.clear();
                }
            });
            cx.notify();
        });
        visual.run_until_parked();
        visual.update(|window, cx| model.read(cx).review_focus.focus(window));
        visual.simulate_keystrokes("c f");
        model.read_with(visual, |model, cx| {
            assert_eq!(model.pulsations.read(cx).hitl.mark_counts().0, 0);
            assert_eq!(model.pulsations.read(cx).hitl.flag_count(), 0);
            assert!(model.current_brief_notes(cx).is_empty());
        });
        for key in ["tab", "shift-tab"] {
            let review_focus = visual.update(|window, cx| {
                model.read(cx).review_focus.focus(window);
                window.focused(cx)
            });
            visual.simulate_keystrokes(key);
            let after = visual.update(|window, cx| window.focused(cx));
            assert!(after.is_some());
            assert_ne!(after, review_focus, "{key} must reach the Shell binding");
        }
    }
}

#[gpui::test]
fn settings_modal_keeps_brief_marks_and_decisions_untouched(cx: &mut TestAppContext) {
    let (model, visual) = open_specification_brief(cx);
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    select(&model, visual, AppView::Pulsations);
    assert!(visual.debug_bounds("review-decision-bar").is_some());
    open_settings(&model, visual, Some(SettingsPage::General));
    visual.simulate_keystrokes("c f tab shift-tab r d enter");
    model.read_with(visual, |model, cx| {
        let hitl = &model.pulsations.read(cx).hitl;
        assert_eq!(hitl.mark_counts(), (0, 0, 4));
        assert!(!hitl.busy);
        assert!(!model.confirm_hitl_reject);
        assert!(model.last_revision_feedback.is_none());
        assert!(model.settings_modal.open);
    });
    let inside = visual.update(|window, cx| model.read(cx).settings_focus_contains(window, cx));
    assert!(
        inside,
        "Tab must stay inside Settings while Review is behind it"
    );
}

#[gpui::test]
fn brief_marks_compose_the_revision_feedback_and_keep_enter_from_dropping_flags(
    cx: &mut TestAppContext,
) {
    use crate::hitl::ItemMark;
    let (model, visual) = open_specification_brief(cx);
    // Tall enough that every judgement item's buttons are on screen.
    visual.simulate_resize(size(px(1440.0), px(1600.0)));
    select(&model, visual, AppView::Pulsations);

    // Mouse: flagging opens the item's note and focuses it.
    click(visual, "brief-flag-1");
    let note = model.read_with(visual, |model, cx| {
        assert_eq!(model.pulsations.read(cx).hitl.marks[1], ItemMark::Flagged);
        model
            .brief_notes
            .get(&1)
            .cloned()
            .expect("a flagged item gets a note")
    });
    assert!(visual.debug_bounds("brief-note-1").is_some());
    visual.update(|window, cx| {
        assert!(gpui::Focusable::focus_handle(note.read(cx), cx).is_focused(window));
    });
    visual.simulate_keystrokes("c f");
    model.read_with(visual, |model, cx| {
        // Typing in the note never fires the review shortcuts.
        assert_eq!(model.pulsations.read(cx).hitl.marks[1], ItemMark::Flagged);
        assert_eq!(note.read(cx).value(), "cf");
    });
    note.update(visual, |input, cx| {
        input.set_value("Add an executed test for the empty range.", cx)
    });

    // Keyboard: from the review, Tab moves, C confirms and F flags the item under the cursor.
    visual.update(|window, cx| model.read(cx).review_focus.focus(window));
    visual.simulate_keystrokes("tab c shift-tab shift-tab f");
    model.read_with(visual, |model, cx| {
        let hitl = &model.pulsations.read(cx).hitl;
        assert_eq!(
            hitl.marks,
            [
                ItemMark::Flagged,
                ItemMark::Flagged,
                ItemMark::Confirmed,
                ItemMark::Open
            ]
        );
        assert_eq!(hitl.brief_cursor, 0);
        assert_eq!(hitl.mark_counts(), (1, 2, 1));
    });
    assert!(visual.debug_bounds("review-brief-progress").is_some());

    // Enter does not approve over the developer's flags.
    visual.simulate_keystrokes("enter");
    model.read_with(visual, |model, cx| {
        let hitl = &model.pulsations.read(cx).hitl;
        assert!(!hitl.busy);
        assert!(
            hitl.error
                .as_deref()
                .is_some_and(|error| error.starts_with("2 items are flagged")),
            "{:?}",
            hitl.error
        );
        assert!(model.last_revision_feedback.is_none());
    });

    // R sends the flags, in brief order, with their notes and the items' own text.
    visual.simulate_keystrokes("r");
    model.read_with(visual, |model, _| {
        let feedback = model.last_revision_feedback.as_deref().unwrap();
        assert!(feedback.starts_with("Flagged by the developer in the review brief:\n"));
        let decision = feedback
            .find("- [Decision taken by the agent] D1: Export aggregates per pulsation and model, not one row per LLM call. — flagged for revision without a note.")
            .expect(feedback);
        let proof = feedback
            .find("- [Proof by reading only] AC-4: An empty date range writes only the header row. — Add an executed test for the empty range.")
            .expect(feedback);
        assert!(decision < proof);
        assert!(!feedback.contains("Existing interface changes"), "confirmed items stay out");
    });
}

#[gpui::test]
fn brief_review_drafts_stay_with_their_checkpoint_and_flags_never_block_approve(
    cx: &mut TestAppContext,
) {
    use crate::hitl::ItemMark;
    use ikore_core::persistence_service::HitlCheckpoint;
    let (model, visual) = open_specification_brief(cx);
    let long_note = "Add an executed test for the empty range, ".repeat(12);
    model.update(visual, |model, cx| {
        model.sync_stage_feedback_scope(cx);
        model.toggle_brief_mark(1, ItemMark::Flagged, None, cx);
        model.toggle_brief_mark(0, ItemMark::Confirmed, None, cx);
        let note = model.brief_notes.get(&1).cloned().unwrap();
        note.update(cx, |input, cx| input.set_value(long_note.clone(), cx));
        model.stage_feedback_input.update(cx, |input, cx| {
            input.set_value("Free text for this checkpoint.", cx)
        });
    });

    // A long note and the progress line fit the reader and the footer at both sizes.
    for (width, height) in [(960.0, 640.0), (1440.0, 900.0)] {
        visual.simulate_resize(size(px(width), px(height)));
        select(&model, visual, AppView::Pulsations);
        let reader = visual.debug_bounds("review-document-reader").unwrap();
        let actions = visual.debug_bounds("review-decision-bar").unwrap();
        let note = visual.debug_bounds("brief-note-1").unwrap();
        assert!(
            note.left() >= reader.left() && note.right() <= reader.right() + px(1.0),
            "note overflows at {width}x{height}: {note:?} vs {reader:?}"
        );
        assert!(reader.bottom() <= actions.top() && actions.bottom() <= px(height));
        for element in [
            "review-brief-progress",
            "hitl-approve",
            "hitl-revision",
            "hitl-reject",
        ] {
            let bounds = visual.debug_bounds(element).unwrap();
            assert!(
                bounds.left() >= actions.left()
                    && bounds.right() <= actions.right()
                    && bounds.top() >= actions.top()
                    && bounds.bottom() <= actions.bottom(),
                "{element} leaves the decision bar at {width}x{height}: {bounds:?} vs {actions:?}"
            );
        }
    }

    // A refresh of the same checkpoint keeps the review; so does switching tabs.
    let same = model.read_with(visual, |model, cx| {
        model
            .pulsations
            .read(cx)
            .hitl
            .expected_checkpoint
            .clone()
            .unwrap()
    });
    model.update(visual, |model, cx| {
        model.pulsations.update(cx, |state, _| {
            let pulse = state.pulsations[0].clone();
            state.hitl.load_checkpoint(Some(&pulse), Some(same.clone()));
            state.hitl.brief_selected = false;
            state.hitl.brief_selected = true;
        });
        model.sync_stage_feedback_scope(cx);
    });
    model.read_with(visual, |model, cx| {
        assert_eq!(
            model.pulsations.read(cx).hitl.marks[..2],
            [ItemMark::Confirmed, ItemMark::Flagged]
        );
        assert_eq!(model.current_brief_notes(cx).len(), 1);
        assert_eq!(
            model.stage_feedback_input.read(cx).value(),
            "Free text for this checkpoint."
        );
    });

    // Flags never block an explicit Approve: the decision is dispatched.
    click(visual, "hitl-approve");
    model.read_with(visual, |model, cx| {
        assert_eq!(
            model.pulsations.read(cx).hitl.error.as_deref(),
            Some("HITL decision unavailable in test environment")
        );
    });

    // Checkpoint B of the same pulsation and input revision starts clean:
    // no marks, no notes and no free-text draft from A.
    model.update(visual, |model, cx| {
        model.pulsations.update(cx, |state, _| {
            let pulse = state.pulsations[0].clone();
            state.hitl.load_checkpoint(
                Some(&pulse),
                Some(HitlCheckpoint {
                    checkpoint_id: "contract-checkpoint".into(),
                    ..same.clone()
                }),
            );
        });
        model.sync_stage_feedback_scope(cx);
    });
    model.read_with(visual, |model, cx| {
        assert!(model.pulsations.read(cx).hitl.marks.is_empty());
        assert!(model.current_brief_notes(cx).is_empty());
        assert_eq!(model.stage_feedback_input.read(cx).value(), "");
    });

    // A newer state version of one checkpoint is a new review as well.
    model.update(visual, |model, cx| {
        model
            .stage_feedback_input
            .update(cx, |input, cx| input.set_value("Draft for version 1.", cx));
        model.pulsations.update(cx, |state, _| {
            let pulse = state.pulsations[0].clone();
            state.hitl.load_checkpoint(
                Some(&pulse),
                Some(HitlCheckpoint {
                    checkpoint_id: "contract-checkpoint".into(),
                    state_version: same.state_version + 1,
                    ..same.clone()
                }),
            );
        });
        model.sync_stage_feedback_scope(cx);
    });
    model.read_with(visual, |model, cx| {
        assert_eq!(model.stage_feedback_input.read(cx).value(), "");
    });
}

fn set_brief_revision(
    model: &Entity<AppModel>,
    visual: &mut VisualTestContext,
    revision: Option<ikore_core::orchestration::review_brief::RevisionDiff>,
) {
    model.update(visual, |model, cx| {
        model.pulsations.update(cx, |state, _| {
            state.hitl.brief.as_mut().unwrap().revision = revision;
        });
    });
    select(model, visual, AppView::Pulsations);
}

#[gpui::test]
fn a_revised_brief_lists_its_changes_under_the_feedback_before_the_judgement(
    cx: &mut TestAppContext,
) {
    use ikore_core::orchestration::review_brief::{
        BriefChange, ChangeKind, ChangeSubject, RevisionDiff,
    };
    let (model, visual) = open_specification_brief(cx);
    let change = |kind,
                  subject,
                  label: &str,
                  field: Option<&'static str>,
                  before: Option<&str>,
                  after: Option<&str>| BriefChange {
        kind,
        subject,
        label: label.into(),
        field,
        before: before.map(Into::into),
        after: after.map(Into::into),
    };
    let long =
        "Export aggregates per pulsation, model and calendar day in the developer's time zone. "
            .repeat(4);
    let changes = vec![
        change(
            ChangeKind::Removed,
            ChangeSubject::Decision,
            "D1",
            None,
            Some("Export aggregates per pulsation and model, not one row per LLM call."),
            None,
        ),
        change(
            ChangeKind::Added,
            ChangeSubject::Decision,
            "D1",
            None,
            None,
            Some(&long),
        ),
        change(
            ChangeKind::Changed,
            ChangeSubject::Criterion,
            "AC-4",
            Some("proof"),
            Some("by reading"),
            Some("executed check"),
        ),
        change(
            ChangeKind::Moved,
            ChangeSubject::Criterion,
            "AC-6",
            Some("work units"),
            Some("ui_export"),
            Some("core_export, ui_export"),
        ),
        change(
            ChangeKind::Changed,
            ChangeSubject::Interface,
            "crates/ikore-ui/src/usage.rs",
            Some("intent"),
            Some("change"),
            Some("preserve"),
        ),
        change(
            ChangeKind::Removed,
            ChangeSubject::File,
            "crates/ikore-core/src/usage/legacy_export_kept_for_one_release_only.rs",
            None,
            Some("existing file"),
            None,
        ),
        change(
            ChangeKind::Added,
            ChangeSubject::Unit,
            "usage_tests",
            None,
            None,
            Some("Author the export tests."),
        ),
    ];
    let feedback = "Flagged by the developer in the review brief:\n\
        - [Decision] D1: Export aggregates per pulsation and model, not one row per LLM call. — Aggregate per day too.\n\
        - [Proof by reading only] AC-4: An empty date range writes only the header row. — Add an executed test.\n\n\
        Additional feedback: keep the button where it is.";
    set_brief_revision(
        &model,
        visual,
        Some(RevisionDiff {
            feedback: Some(feedback.into()),
            changes: Some(changes.clone()),
        }),
    );

    // Tall windows keep every row painted; the widths are the ones under test.
    for (width, height) in [(960.0, 1800.0), (1440.0, 1800.0)] {
        visual.simulate_resize(size(px(width), px(height)));
        select(&model, visual, AppView::Pulsations);
        let reader = visual.debug_bounds("review-document-reader").unwrap();
        let decision = visual.debug_bounds("brief-decision").unwrap();
        let revision = visual.debug_bounds("brief-revision").unwrap();
        let judgement = visual.debug_bounds("brief-judgement").unwrap();
        let quoted = visual.debug_bounds("brief-revision-feedback").unwrap();
        assert!(
            decision.bottom() <= revision.top() && revision.bottom() <= judgement.top(),
            "the diff sits between the decision line and the judgement at {width}: {decision:?} {revision:?} {judgement:?}"
        );
        assert!(
            revision.left() >= reader.left() && revision.right() <= reader.right() + px(1.0),
            "the diff overflows the reader at {width}: {revision:?} vs {reader:?}"
        );
        assert!(quoted.left() >= revision.left() && quoted.right() <= revision.right());
        assert!(
            quoted.size.height >= px(60.0),
            "every feedback line shows at {width}: {quoted:?}"
        );
        let mut above = quoted.bottom();
        const ROWS: [&str; 7] = [
            "brief-change-0",
            "brief-change-1",
            "brief-change-2",
            "brief-change-3",
            "brief-change-4",
            "brief-change-5",
            "brief-change-6",
        ];
        assert_eq!(ROWS.len(), changes.len());
        for selector in ROWS {
            let row = visual
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("missing {selector} at {width}"));
            assert!(
                row.left() >= revision.left() && row.right() <= revision.right() + px(1.0),
                "{selector} overflows at {width}: {row:?} vs {revision:?}"
            );
            assert!(row.top() >= above, "{selector} is out of order at {width}");
            above = row.bottom();
        }
        const HEADS: [&str; 7] = [
            "brief-change-head-0",
            "brief-change-head-1",
            "brief-change-head-2",
            "brief-change-head-3",
            "brief-change-head-4",
            "brief-change-head-5",
            "brief-change-head-6",
        ];
        // Each header is one line; only the long path may take a second. A label
        // that shrank inside a wrapping row once stood one character per line.
        for (index, selector) in HEADS.into_iter().enumerate() {
            let head = visual.debug_bounds(selector).unwrap();
            let limit = if index == 5 { px(40.0) } else { px(24.0) };
            assert!(
                head.size.height <= limit,
                "{selector} is {head:?} at {width}"
            );
        }
        // The long decision wraps inside the card instead of widening it.
        let wrapped = visual.debug_bounds("brief-change-1").unwrap();
        assert!(
            wrapped.size.height >= px(48.0),
            "long text did not wrap at {width}: {wrapped:?}"
        );
    }

    // The small window's reader is short (the decision line alone fills it), so
    // the card sits below the fold there, still inside the reader's width.
    visual.simulate_resize(size(px(960.0), px(640.0)));
    select(&model, visual, AppView::Pulsations);
    let reader = visual.debug_bounds("review-document-reader").unwrap();
    let revision = visual.debug_bounds("brief-revision").unwrap();
    assert!(
        revision.left() >= reader.left() && revision.right() <= reader.right() + px(1.0),
        "{revision:?} vs {reader:?}"
    );
}

/// Each variant gets its own window: GPUI's test frames keep every selector
/// they ever painted, so absence is only provable where it never rendered.
#[gpui::test]
fn an_unreadable_or_unchanged_previous_revision_says_so_and_a_first_candidate_shows_no_diff(
    cx: &mut TestAppContext,
) {
    use ikore_core::orchestration::review_brief::RevisionDiff;
    visual_variant(cx, |model, visual| {
        set_brief_revision(
            model,
            visual,
            Some(RevisionDiff {
                feedback: None,
                changes: None,
            }),
        );
        // An unreadable previous candidate is said, never reconstructed.
        assert!(visual.debug_bounds("brief-revision").is_some());
        assert!(visual.debug_bounds("brief-revision-unavailable").is_some());
        assert!(visual.debug_bounds("brief-revision-feedback").is_none());
        assert!(visual.debug_bounds("brief-change-0").is_none());
    });
    visual_variant(cx, |model, visual| {
        set_brief_revision(
            model,
            visual,
            Some(RevisionDiff {
                feedback: Some("Tighten the wording.".into()),
                changes: Some(Vec::new()),
            }),
        );
        // A revision that changed nothing structural lists nothing.
        assert!(visual.debug_bounds("brief-revision-feedback").is_some());
        assert!(visual.debug_bounds("brief-revision-unavailable").is_none());
        assert!(visual.debug_bounds("brief-change-0").is_none());
    });
    visual_variant(cx, |model, visual| {
        select(model, visual, AppView::Pulsations);
        assert!(visual.debug_bounds("brief-decision").is_some());
        assert!(visual.debug_bounds("brief-revision").is_none());
        model.read_with(visual, |model, cx| {
            assert!(model.pulsations.read(cx).hitl.can_approve());
        });
    });
}

fn visual_variant(
    cx: &mut TestAppContext,
    check: impl FnOnce(&Entity<AppModel>, &mut VisualTestContext),
) {
    let (model, visual) = open_specification_brief(cx);
    visual.simulate_resize(size(px(1440.0), px(1800.0)));
    check(&model, visual);
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
    use ikore_core::persistence_service::StageFailureCheckpoint;
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
                allow_continue_anyway: false,
                revision_stage: revision.then_some(PulsationStage::Code),
                attempts: 3,
                state_version: 1,
                created_at: "2026-09-09T00:00:00Z".into(),
                allowed_revision_targets: Vec::new(),
                remaining_extra_invocations: None,
                stage_execution_id: None,
                scope_request: None,
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
fn validation_failure_evidence_keeps_recovery_actions_visible(cx: &mut TestAppContext) {
    use ikore_core::orchestration::composition::recovery::ValidationFailureContextV1;
    use ikore_core::persistence_service::StageFailureDetail;
    let (model, visual) =
        parked_failure_window(cx, "validation_failed", false, true, (960.0, 640.0));
    let context: ValidationFailureContextV1 = serde_json::from_value(serde_json::json!({
        "schemaVersion": 1, "stageExecutionId": "previous-execution", "candidateSha256": "digest",
        "checks": [{"receiptId":"receipt", "checkRef":"unit_tests", "required":true, "status":"failed", "candidateSha256":"digest", "diagnostics":["Expected expired tokens to be rejected"], "diagnosticsOmitted":0, "stdout":"", "stderr":"", "outputTruncated":false}],
        "omittedChecks":0, "requirements":[], "decisions":[], "interfaces":[],
        "tests":[{"path":"tests/auth_test.rs", "sha256":"digest", "content":"assert!(!expired)", "contentSanitized":false, "unavailableReason":null}],
        "omittedTests":0, "limitations":["Historical evidence does not establish the cause."]
    })).unwrap();
    model.update(visual, |model, cx| {
        model.pulsations.update(cx, |state, _| {
            state.failure.detail = Some(StageFailureDetail {
                checkpoint_id: "layout-failure".into(),
                raw_output: None,
                validation_error: None,
                validation_context: Some(context),
            });
        });
        cx.notify();
    });
    select(&model, visual, AppView::Pulsations);
    visual
        .debug_bounds("failure-validation-evidence")
        .expect("actual check results are visible");
    let bar = visual.debug_bounds("failure-decision-bar").unwrap();
    assert!(bar.bottom() <= px(640.0));
    visual
        .debug_bounds("failure-revision")
        .expect("the operator can still request the agent's correction");
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
    use ikore_core::orchestration::{
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

/// The active pulsation at `status` in `section`, with the report card's last
/// notice set, in a window of its own.
fn report_window(
    cx: &mut TestAppContext,
    section: PulsationDetailSection,
    status: PulsationStatus,
    notice: Option<crate::pulsations::ReportNotice>,
    window: (f32, f32),
) -> (Entity<AppModel>, &mut VisualTestContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    model.update(visual, |m, cx| {
        m.pulsations.update(cx, |state, _| {
            state.pulsations[0].stage = PulsationStage::Integrator;
            state.pulsations[0].status = status;
            state.detail_section = section;
            state.report.notice = notice;
        });
        m.select_view(AppView::Pulsations, cx);
    });
    visual.simulate_resize(size(px(window.0), px(window.1)));
    visual.run_until_parked();
    (model, visual)
}

/// ADR-027 M6: the report card sits in Overview and Artifacts, inside the
/// reading column at the minimum and the common window, with its two actions
/// side by side and a long written path wrapping inside the card.
#[gpui::test]
fn the_report_card_fits_overview_and_artifacts(cx: &mut TestAppContext) {
    use crate::pulsations::ReportNotice;
    let written = std::path::PathBuf::from("C:")
        .join("Users")
        .join("developer")
        .join("AppData")
        .join("Roaming")
        .join(ikore_core::app_names::APP_IDENTIFIER)
        .join("reports")
        .join("5d0c2a6e-8b1f-4c3a-9e27-61f0b4a9d2c8")
        .join("pulsation-report.html");
    for window in [(960.0, 640.0), (1440.0, 900.0)] {
        for section in [
            PulsationDetailSection::Overview,
            PulsationDetailSection::Artifacts,
        ] {
            let (_, visual) = report_window(
                cx,
                section,
                PulsationStatus::Done,
                Some(ReportNotice::Opened(written.clone())),
                window,
            );
            let scroll = visual.debug_bounds("pulsation-section-scroll").unwrap();
            let card = visual.debug_bounds("pulsation-report").unwrap();
            let status = visual.debug_bounds("pulsation-report-status").unwrap();
            let open = visual.debug_bounds("pulsation-report-open").unwrap();
            let save = visual.debug_bounds("pulsation-report-save").unwrap();
            let at = format!("{section:?} at {window:?}");
            assert!(
                card.left() >= scroll.left() && card.right() <= scroll.right(),
                "{at}: the card leaves the reading column: {card:?} in {scroll:?}"
            );
            assert!(
                status.right() <= card.right() && status.size.width > card.size.width / 2.0,
                "{at}: the written path overflows or collapses: {status:?} in {card:?}"
            );
            assert!(
                save.left() > open.right() && open.top() == save.top(),
                "{at}: the actions sit side by side"
            );
            assert!(
                open.size.width < card.size.width / 2.0,
                "{at}: the open action sizes to its label"
            );
            assert!(
                status.bottom() <= open.top(),
                "{at}: status above the actions"
            );
        }
    }
}

/// Before the Integrator checkpoint the card only says when the report
/// becomes available; its actions do nothing.
#[gpui::test]
fn the_report_card_waits_for_the_integrator(cx: &mut TestAppContext) {
    let (model, visual) = report_window(
        cx,
        PulsationDetailSection::Overview,
        PulsationStatus::Running,
        None,
        (1440.0, 900.0),
    );
    assert!(visual.debug_bounds("pulsation-report-status").is_some());
    click(visual, "pulsation-report-open");
    click(visual, "pulsation-report-save");
    model.update(visual, |m, cx| {
        let report = &m.pulsations.read(cx).report;
        assert!(!report.busy);
        assert_eq!(report.notice, None);
    });
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
fn composed_detail(pulse: &Pulsation) -> ikore_core::orchestration::PulsationCoordinationSnapshot {
    use ikore_core::orchestration::composition::{
        AttemptState, AttemptSummaryV1, CheckStatus, CheckSummaryV1, ExecutionStatus, ExecutorKind,
        Handle, RemainingBudgetV1, Stage, StageExecutionSnapshot, UnitSnapshotV1, UnitState,
        UsageCompleteness, UsageSummaryV1, Uuid,
    };
    use ikore_core::orchestration::*;
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
        sanitization: Default::default(),
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
            schema_version: ikore_core::orchestration::SchemaV1,
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
            validation_evidence: vec![],
            validation_receipts: vec![],
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
        layers: None,
        attestation: None,
        attested: false,
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
    // The execution sits under the Layers panel, collapsed until opened.
    click(visual, "execution-details-toggle");
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

/// A text sharing a flex row keeps a usable width. Squeezed, GPUI lays it
/// out at zero width and wraps one character per line (found by the ADR-035
/// M5 visual pass).
fn assert_readable(
    visual: &mut VisualTestContext,
    selector: &'static str,
    min_width: f32,
    max_height: f32,
) {
    let bounds = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("missing {selector}"));
    assert!(
        bounds.size.width > px(min_width) && bounds.size.height < px(max_height),
        "{selector} squeezed: {bounds:?}"
    );
}

fn select(model: &Entity<AppModel>, visual: &mut VisualTestContext, view: AppView) {
    model.update(visual, |model, cx| model.select_view(view, cx));
    visual.run_until_parked();
}

/// Feature 25, M1: the Spec agent's synthesis on the canvas, with `drawer`
/// open whatever was open before.
fn open_agent_drawer(
    model: &Entity<AppModel>,
    visual: &mut VisualTestContext,
    drawer: crate::agents::AgentDetailTab,
) {
    use crate::agents::AgentDetailTab;
    click(visual, "agents-node:role:task_specifier");
    if model.read_with(visual, |m, cx| m.agents.read(cx).detail_tab) == drawer {
        return;
    }
    click(
        visual,
        match drawer {
            AgentDetailTab::Summary => "agents-drawer-close",
            AgentDetailTab::Configuration => "agents-synthesis-configure",
            AgentDetailTab::History => "agents-synthesis-history",
            AgentDetailTab::Run => "agents-synthesis-run",
        },
    );
}

#[gpui::test]
fn notification_and_direct_selector_overlays_do_not_move_controls(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    for (width, height) in [(960.0, 640.0), (1440.0, 900.0)] {
        visual.simulate_resize(size(px(width), px(height)));
        select(&model, visual, AppView::Agents);
        // The backend selector lives in Configuration (ADR-040 M7).
        open_agent_drawer(&model, visual, crate::agents::AgentDetailTab::Configuration);
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
        // The panel drops from the bell's bottom-left corner, like a menu.
        let bell = visual.debug_bounds("notifications").unwrap();
        assert_eq!(history.right(), bell.left(), "{history:?} vs {bell:?}");
        assert!(
            history.top() >= bell.bottom() && history.top() - bell.bottom() <= px(12.0),
            "{history:?} vs {bell:?}"
        );
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
    open_agent_drawer(&model, visual, crate::agents::AgentDetailTab::Configuration);
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
    open_agent_drawer(&model, visual, crate::agents::AgentDetailTab::Configuration);
    click(visual, "agent-backend:task_specifier");
    click(visual, "rail-settings");
    assert_eq!(
        model.read_with(visual, |m, _| m.active_view()),
        AppView::Agents
    );
    assert!(model.read_with(visual, |m, _| m.choice_menu.is_none()));
    click(visual, "rail-settings");
    // Settings opens as a modal over the view instead of replacing it.
    assert_eq!(
        model.read_with(visual, |m, _| m.active_view()),
        AppView::Agents
    );
    assert!(model.read_with(visual, |m, _| m.settings_modal.open));
}

#[gpui::test]
fn nav_rail_selects_every_view_and_pins_settings_at_the_bottom(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    for (width, height) in [(960.0, 640.0), (1440.0, 900.0)] {
        visual.simulate_resize(size(px(width), px(height)));
        visual.run_until_parked();
        let rail = visual.debug_bounds("rail").unwrap();
        assert_eq!(rail.left(), px(0.0));
        assert_eq!(rail.size.width, crate::sizing::NAV_RAIL_WIDTH);
        let header = visual.debug_bounds("header").unwrap();
        let status_bar = visual.debug_bounds("status-bar").unwrap();
        assert_eq!(rail.top(), header.bottom());
        assert_eq!(rail.bottom(), status_bar.top());

        let mut lowest_nav_bottom = px(0.0);
        for view in crate::shell::RAIL_VIEWS {
            let selector: &'static str = match view {
                AppView::Home => "rail-nav-home",
                AppView::Forge => "rail-nav-forge",
                AppView::Pulsations => "rail-nav-pulsations",
                AppView::Vault => "rail-nav-vault",
                AppView::Agents => "rail-nav-agents",
                AppView::Logs => "rail-nav-logs",
                AppView::Chat => "rail-nav-chat",
                AppView::Notes => "rail-nav-notes",
                AppView::Canvas => "rail-nav-canvas",
                AppView::Workspace => "rail-nav-workspace",
                AppView::Settings => unreachable!("settings is not a rail view"),
            };
            let item = visual.debug_bounds(selector).unwrap();
            assert!(
                item.left() >= rail.left() && item.right() <= rail.right(),
                "{selector} escaped the rail at {width}: {item:?}"
            );
            assert!(item.top() >= rail.top() && item.bottom() <= rail.bottom());
            lowest_nav_bottom = lowest_nav_bottom.max(item.bottom());
            click(visual, selector);
            assert_eq!(
                model.read_with(visual, |model, _| model.active_view()),
                view,
                "{selector} did not select {view:?}"
            );
        }
        let new_work = visual.debug_bounds("new-work").unwrap();
        assert!(new_work.left() >= rail.left() && new_work.right() <= rail.right());
        let settings = visual.debug_bounds("rail-settings").unwrap();
        assert!(
            settings.top() > lowest_nav_bottom,
            "settings is not pinned below the nav items at {width}: {settings:?}"
        );
        assert!(settings.bottom() <= rail.bottom());
        click(visual, "rail-settings");
        assert!(model.read_with(visual, |model, _| model.settings_modal.open));
        assert_eq!(
            model.read_with(visual, |model, _| model.active_view()),
            AppView::Workspace
        );
        // The backdrop covers the rail; close before the next window size clicks it.
        click(visual, "settings-close");
        assert!(!model.read_with(visual, |model, _| model.settings_modal.open));
        // The former tab bar and context bar are gone for good.
        assert!(visual.debug_bounds("flow-bar").is_none());
        assert!(visual.debug_bounds("flow-bar-tabs").is_none());
        assert!(visual.debug_bounds("toggle-sidebar").is_none());
    }
}

#[gpui::test]
fn header_is_informational_and_fits_the_minimum_width(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    for (width, height) in [(960.0, 640.0), (1440.0, 900.0)] {
        visual.simulate_resize(size(px(width), px(height)));
        for view in [AppView::Home, AppView::Agents, AppView::Settings] {
            select(&model, visual, view);
            let header = visual.debug_bounds("header").unwrap();
            assert_eq!(header.top(), px(0.0));
            assert_eq!(header.size.height, crate::sizing::HEADER_HEIGHT);
            let brand = visual.debug_bounds("header-brand").unwrap();
            let context = visual.debug_bounds("header-context").unwrap();
            let health = visual.debug_bounds("header-health").unwrap();
            let notifications = visual.debug_bounds("notifications").unwrap();
            assert!(brand.left() < px(80.0));
            assert!(
                context.left() < px(width / 2.0) && context.right() > px(width / 2.0),
                "context readout off-centre in {view:?} at {width}: {context:?}"
            );
            assert!(brand.right() <= context.left());
            assert!(context.right() <= health.left());
            assert!(health.right() <= notifications.left());
            assert!(notifications.right() <= px(width));
            for bounds in [brand, context, health, notifications] {
                assert!(bounds.top() >= header.top() && bounds.bottom() <= header.bottom());
            }
        }
        // Old header controls must not linger under their previous ids.
        assert!(visual.debug_bounds("settings").is_none());
        assert!(visual.debug_bounds("pipeline-status").is_none());
    }
    // The centre readout is a readout: clicking it goes nowhere.
    select(&model, visual, AppView::Agents);
    click(visual, "header-context");
    assert_eq!(
        model.read_with(visual, |model, _| model.active_view()),
        AppView::Agents
    );
    // Without a pulsation the slot keeps a muted placeholder instead of collapsing.
    model.update(visual, |model, cx| {
        model.pulsations.update(cx, |pulsations, _| {
            pulsations.pulsations.clear();
            pulsations.active_pulsation_id = None;
        });
        cx.notify();
    });
    visual.run_until_parked();
    let context = visual.debug_bounds("header-context").unwrap();
    assert!(context.size.width > px(0.0));
}

/// Visual QA (2026-11-16): the Shell holds the keyboard from the first frame,
/// so shortcuts and Tab answer before any click. A deliberate blur stays the
/// developer's: the Shell does not take the keyboard back.
#[gpui::test]
fn shortcuts_answer_from_the_first_frame(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.update(|_, cx| crate::shell::register_navigation_keybindings(cx));
    visual.simulate_resize(size(px(1280.0), px(820.0)));
    visual.run_until_parked();
    assert!(
        model.read_with(visual, |model, _| model.active_view()) != AppView::Agents,
        "the fixture opens elsewhere"
    );
    visual.simulate_keystrokes("ctrl-5");
    visual.run_until_parked();
    assert_eq!(
        model.read_with(visual, |model, _| model.active_view()),
        AppView::Agents
    );
    let tab = model.read_with(visual, |model, cx| model.agents.read(cx).detail_tab);
    visual.simulate_keystrokes("ctrl-shift-]");
    visual.run_until_parked();
    assert_ne!(
        model.read_with(visual, |model, cx| model.agents.read(cx).detail_tab),
        tab
    );
    visual.simulate_keystrokes("tab");
    visual.run_until_parked();
    let focused = visual.update(|window, cx| window.focused(cx));
    assert!(focused.is_some_and(|focused| {
        model.read_with(visual, |model, _| focused != model.shell_focus)
    }));

    visual.update(|window, _| window.blur());
    visual.run_until_parked();
    visual.update(|window, cx| assert!(window.focused(cx).is_none()));
}

#[gpui::test]
fn keyboard_shortcuts_follow_rail_order(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.update(|_, cx| crate::shell::register_navigation_keybindings(cx));
    visual.simulate_resize(size(px(960.0), px(640.0)));
    visual.run_until_parked();
    // Own the focus first so the keystrokes reach the shell's action handlers.
    click(visual, "rail-nav-home");
    for (view, keys) in crate::shell::RAIL_SHORTCUTS {
        visual.simulate_keystrokes(keys);
        visual.run_until_parked();
        assert_eq!(
            model.read_with(visual, |model, _| model.active_view()),
            view,
            "{keys} did not select {view:?}"
        );
    }
    // Existing shortcuts keep their keys; the Notes view took the next free one.
    assert!(crate::shell::RAIL_SHORTCUTS.contains(&(AppView::Workspace, "ctrl-8")));
    assert!(crate::shell::RAIL_SHORTCUTS.contains(&(AppView::Notes, "ctrl-9")));
    assert!(crate::shell::RAIL_SHORTCUTS.contains(&(AppView::Canvas, "ctrl-0")));
    visual.simulate_keystrokes("ctrl-,");
    visual.run_until_parked();
    assert!(model.read_with(visual, |model, _| model.settings_modal.open));
    assert_eq!(
        model.read_with(visual, |model, _| model.active_view()),
        AppView::Canvas
    );
    visual.simulate_keystrokes("ctrl-shift-]");
    visual.run_until_parked();
    assert_eq!(
        model.read_with(visual, |model, cx| model.settings.read(cx).page),
        SettingsPage::Usage
    );
    let before = model.read_with(visual, |model, cx| model.context_rail_collapsed(cx));
    visual.simulate_keystrokes("ctrl-b");
    visual.run_until_parked();
    assert_ne!(
        model.read_with(visual, |model, cx| model.context_rail_collapsed(cx)),
        before
    );
    // The same shortcut closes Settings again.
    visual.simulate_keystrokes("ctrl-,");
    visual.run_until_parked();
    assert!(!model.read_with(visual, |model, _| model.settings_modal.open));
}

#[gpui::test]
fn status_bar_keeps_pending_counts_events_version_and_clock(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    for (width, height) in [(960.0, 640.0), (1440.0, 900.0)] {
        visual.simulate_resize(size(px(width), px(height)));
        select(&model, visual, AppView::Home);
        let status_bar = visual.debug_bounds("status-bar").unwrap();
        assert_eq!(status_bar.size.height, crate::sizing::STATUS_BAR_HEIGHT);
        assert_eq!(status_bar.bottom(), px(height));
        assert_eq!(status_bar.size.width, px(width));
        let events = visual.debug_bounds("status-events").unwrap();
        let version = visual.debug_bounds("status-version").unwrap();
        let clock = visual.debug_bounds("status-clock").unwrap();
        assert!(events.right() <= version.left());
        assert!(version.right() <= clock.left());
        assert!(clock.right() <= px(width));
        for bounds in [events, version, clock] {
            assert!(bounds.top() >= status_bar.top() && bounds.bottom() <= status_bar.bottom());
        }
        assert!(visual.debug_bounds("status-hitl").is_none());
    }
    model.update(visual, |model, cx| {
        model.pulsations.update(cx, |pulsations, _| {
            pulsations.awaiting_hitl = 2;
        });
        cx.notify();
    });
    visual.run_until_parked();
    click(visual, "status-hitl");
    assert_eq!(
        model.read_with(visual, |model, _| model.active_view()),
        AppView::Pulsations
    );
    click(visual, "status-events");
    assert_eq!(
        model.read_with(visual, |model, _| model.active_view()),
        AppView::Logs
    );
    assert_eq!(crate::shell::compact_count(999), "999");
    assert_eq!(crate::shell::compact_count(18_400), "18.4k");
    assert_eq!(crate::shell::compact_count(2_150_000), "2.1M");
}

#[gpui::test]
fn home_is_the_default_view_and_its_primary_step_runs_the_flow(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    assert_eq!(
        model.read_with(visual, |model, _| model.active_view()),
        AppView::Home
    );
    for (width, height) in [(960.0, 640.0), (1440.0, 900.0)] {
        visual.simulate_resize(size(px(width), px(height)));
        select(&model, visual, AppView::Home);
        let center = visual.debug_bounds("shell-center").unwrap();
        let hero = visual.debug_bounds("home-hero").unwrap();
        assert!(hero.left() >= center.left() && hero.right() <= center.right());
        assert!(hero.top() >= center.top());
        assert!(visual.debug_bounds("home-hero-title").is_some());
        let pipeline = visual.debug_bounds("home-pipeline").unwrap();
        assert!(pipeline.left() >= hero.left() && pipeline.right() <= hero.right());
        assert!(
            visual
                .debug_bounds("home-pipeline-stage-specification")
                .is_some()
        );
        let step = visual.debug_bounds("home-next-step").unwrap();
        assert!(step.top() >= hero.bottom());
        assert!(step.right() <= center.right());
        // Home is the unfolded context; the rail's collapse toggle has nothing to fold here.
        assert!(visual.debug_bounds("context-rail").is_none());
    }
    // The fixture's pulsation is Planning, so the primary step is "Follow pipeline".
    click(visual, "home-next-step");
    assert_eq!(
        model.read_with(visual, |model, _| model.active_view()),
        AppView::Pulsations
    );

    // Without a project the hero gives way to a single call to action.
    let (_, fresh) = cx.add_window_view(|_, cx| {
        let model = fixture(cx);
        model
            .workspace
            .update(cx, |workspace, _| workspace.project_id = None);
        model
    });
    assert!(fresh.debug_bounds("home-open-project").is_some());
    assert!(fresh.debug_bounds("home-hero").is_none());
}

fn leak(selector: String) -> &'static str {
    Box::leak(selector.into_boxed_str())
}

fn open_settings(
    model: &Entity<AppModel>,
    visual: &mut VisualTestContext,
    page: Option<SettingsPage>,
) {
    model.update(visual, |model, cx| model.open_settings(page, cx));
    visual.run_until_parked();
}

/// One connection, one route targeting it and a composed project, so every Settings page and
/// drill-in detail has something to render.
fn with_settings_items(model: &mut AppModel, cx: &mut Context<AppModel>) {
    use ikore_core::llm::{ProviderConnection, ProviderRoute, RouteTarget};
    model.settings.update(cx, |settings, _| {
        let snapshot = settings.snapshot.as_mut().expect("fixture snapshot");
        snapshot.connections = vec![ProviderConnection {
            id: "conn-1".into(),
            provider_id: "openai".into(),
            display_name: "OpenAI".into(),
            endpoint_url: None,
            default_model: "gpt-4.1".into(),
            enabled: true,
            state: "ready".into(),
            blocked_reason: None,
            cooldown_until: None,
            max_concurrency: 2,
            version: 1,
            has_api_key: true,
            chat_eligible: true,
            created_at: "now".into(),
            updated_at: "now".into(),
        }];
        snapshot.routes = vec![ProviderRoute {
            id: "route-1".into(),
            name: "Main route".into(),
            strategy: "priority".into(),
            max_attempts: 3,
            attempt_timeout_ms: 45_000,
            route_timeout_ms: 120_000,
            enabled: true,
            version: 1,
            targets: vec![RouteTarget {
                position: 0,
                connection_id: "conn-1".into(),
                model_id: "gpt-4.1".into(),
                enabled: true,
            }],
            created_at: "now".into(),
            updated_at: "now".into(),
        }];
        snapshot.composition = Some(crate::settings::CompositionProjectSettings {
            project_id: "layout-project".into(),
            project_name: "Layout".into(),
            settings: Default::default(),
            profiles: vec![],
            presets: vec![],
            cargo_policies: Default::default(),
            overrides: vec![],
            cli_observation_trial: None,
            cli_observation_report: None,
            sanitization_exceptions: vec![],
            sanitization_summary: Default::default(),
        });
        settings.selected_connection_id = Some("conn-1".into());
        settings.selected_route_id = Some("route-1".into());
    });
}

#[gpui::test]
fn settings_modal_fits_the_window_and_lists_every_page(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    for (width, height) in [(960.0, 640.0), (1440.0, 900.0), (1920.0, 1080.0)] {
        visual.simulate_resize(size(px(width), px(height)));
        select(&model, visual, AppView::Agents);
        click(visual, "rail-settings");
        assert!(model.read_with(visual, |m, _| m.settings_modal.open));
        let modal = visual.debug_bounds("settings-modal").unwrap();
        assert!(
            modal.left() > px(0.0)
                && modal.top() > px(0.0)
                && modal.right() < px(width)
                && modal.bottom() < px(height),
            "modal {modal:?} does not fit {width}x{height}"
        );
        let nav = visual.debug_bounds("settings-nav").unwrap();
        let content = visual.debug_bounds("settings-content-scroll").unwrap();
        let close = visual.debug_bounds("settings-close").unwrap();
        assert!(nav.left() >= modal.left() && nav.right() <= content.left());
        assert!(content.right() <= modal.right() && content.bottom() <= modal.bottom());
        assert!(close.right() <= modal.right() && close.top() >= modal.top());
        for page in SettingsPage::ALL {
            let item = visual
                .debug_bounds(leak(format!("settings-nav:{}", page.id())))
                .unwrap_or_else(|| panic!("no sidebar item for {page:?}"));
            assert!(item.left() >= nav.left() && item.right() <= nav.right());
        }
        click(visual, "settings-nav:connections");
        assert_eq!(
            model.read_with(visual, |m, cx| m.settings.read(cx).page),
            SettingsPage::Connections
        );
        // Sections keep a reading width however wide the pane is.
        click(visual, "settings-nav:general");
        let section = visual.debug_bounds("settings-section:Workflow").unwrap();
        assert!(section.size.width <= px(720.0), "{:?}", section.size.width);
        click(visual, "settings-close");
        assert!(!model.read_with(visual, |m, _| m.settings_modal.open));
        assert_eq!(
            model.read_with(visual, |m, _| m.active_view()),
            AppView::Agents
        );
    }
}

#[gpui::test]
fn settings_backdrop_closes_but_clicks_inside_the_panel_do_not(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(960.0), px(640.0)));
    select(&model, visual, AppView::Agents);
    open_settings(&model, visual, None);
    let modal = visual.debug_bounds("settings-modal").unwrap();
    // The sidebar's padding above the search box: inside the panel, on nothing clickable.
    visual.simulate_click(
        point(modal.left() + px(6.0), modal.top() + px(6.0)),
        Modifiers::default(),
    );
    visual.run_until_parked();
    assert!(model.read_with(visual, |m, _| m.settings_modal.open));
    // The dimmed backdrop beside the panel closes it; the view behind is untouched.
    visual.simulate_click(
        point(modal.left() / 2.0, modal.top() + px(40.0)),
        Modifiers::default(),
    );
    visual.run_until_parked();
    assert!(!model.read_with(visual, |m, _| m.settings_modal.open));
    assert_eq!(
        model.read_with(visual, |m, _| m.active_view()),
        AppView::Agents
    );
}

#[gpui::test]
fn project_deletion_confirmation_has_a_readable_width_and_accepts_the_project_name(
    cx: &mut TestAppContext,
) {
    use ikore_core::persistence_service::Project;
    use ikore_core::project_deletion::ProjectDeletionPreview;
    let (model, visual) = cx.add_window_view(|_, cx| {
        let model = fixture(cx);
        model.settings.update(cx, |settings, _| {
            settings.deletion_project_id = Some("layout-project".into());
            settings.deletion_preview = Some(ProjectDeletionPreview {
                project: Project {
                    id: "layout-project".into(),
                    name: "Note-App-Online".into(),
                    workspace_path: "C:/Projects/Note-App-Online".into(),
                    is_archived: false,
                    created_at: "now".into(),
                    updated_at: "now".into(),
                    last_opened_at: None,
                },
                pulsations: 3,
                conversations: 2,
                files: 12,
                bytes: 4096,
            });
        });
        model
    });
    for (width, height) in [(960.0, 640.0), (1440.0, 900.0)] {
        visual.simulate_resize(size(px(width), px(height)));
        open_settings(&model, visual, Some(SettingsPage::Projects));
        let scroll = visual.debug_bounds("settings-content-scroll").unwrap();
        visual.simulate_event(ScrollWheelEvent {
            position: scroll.center(),
            delta: ScrollDelta::Pixels(point(px(0.0), px(-1000.0))),
            ..Default::default()
        });
        visual.run_until_parked();
        let input = visual.debug_bounds("sized-input:320").unwrap();
        assert!(
            input.size.width >= px(250.0) && input.size.width <= px(320.0),
            "{input:?}"
        );
        assert!(input.left() >= scroll.left() && input.right() <= scroll.right());
        model.update(visual, |model, cx| {
            model
                .settings_project_delete_input
                .update(cx, |input, cx| input.clear(cx));
        });
        click(visual, "sized-input:320");
        visual.simulate_input("Note-App-Online");
        assert_eq!(
            model.read_with(visual, |model, cx| {
                model
                    .settings_project_delete_input
                    .read(cx)
                    .value()
                    .to_string()
            }),
            "Note-App-Online"
        );
    }
}

/// A plan-quota overview as the meter produces it: Codex fresh with a tight 5h window, Claude
/// fresh with its first-use disclosure pending, Antigravity signed out.
fn quota_overview_fixture() -> ikore_core::quota::QuotaOverview {
    use ikore_core::quota::{
        BackendQuota, CredentialDisclosure, Projection, QuotaOverview, QuotaSnapshot, QuotaSource,
        QuotaState, QuotaWindow, SourceStatus, UnavailableReason,
    };
    let now = chrono::Utc::now();
    let window = |used: f64, minutes: u32, hours_left: i64| {
        QuotaWindow::new(
            None,
            used,
            Some(minutes),
            Some(now + chrono::Duration::hours(hours_left)),
        )
        .unwrap()
    };
    let sources = |backend: CliBackend, active: QuotaSource| {
        ikore_core::quota::source_chain(backend)
            .iter()
            .map(|&source| SourceStatus {
                source,
                enabled: true,
                active: source == active,
                last_error: None,
            })
            .collect::<Vec<_>>()
    };
    QuotaOverview {
        checked_at: now,
        credential_collectors_off: None,
        backends: vec![
            BackendQuota {
                backend: CliBackend::Codex,
                state: QuotaState::Fresh,
                snapshot: Some(QuotaSnapshot::new(
                    CliBackend::Codex,
                    Some("plus"),
                    vec![window(92.0, 300, 2), window(16.0, 10_080, 100)],
                    QuotaSource::CodexAppServer,
                    now,
                )),
                projections: vec![
                    Some(Projection {
                        exhausts_at: now + chrono::Duration::minutes(40),
                        before_reset: true,
                    }),
                    None,
                ],
                sources: sources(CliBackend::Codex, QuotaSource::CodexAppServer),
                disclosure: None,
                endpoint_switch: Some(true),
            },
            BackendQuota {
                backend: CliBackend::Claude,
                state: QuotaState::Fresh,
                snapshot: Some(QuotaSnapshot::new(
                    CliBackend::Claude,
                    None,
                    vec![window(30.0, 300, 3), window(60.0, 10_080, 30)],
                    QuotaSource::ClaudeEndpoint,
                    now,
                )),
                projections: vec![None, None],
                sources: sources(CliBackend::Claude, QuotaSource::ClaudeEndpoint),
                disclosure: Some(CredentialDisclosure {
                    backend: CliBackend::Claude,
                    file: "D:/fixture/.claude/.credentials.json".into(),
                    host: "api.anthropic.com",
                }),
                endpoint_switch: Some(true),
            },
            BackendQuota {
                backend: CliBackend::Antigravity,
                state: QuotaState::Unavailable(UnavailableReason::NotSignedIn),
                snapshot: None,
                projections: vec![],
                sources: sources(CliBackend::Antigravity, QuotaSource::AntigravityUsage),
                disclosure: None,
                endpoint_switch: None,
            },
        ],
    }
}

#[gpui::test]
fn connections_show_cli_plan_windows_without_an_open_project_and_keep_bars_inside_cards(
    cx: &mut TestAppContext,
) {
    use ikore_core::terminals::{
        CliAvailability, CliAvailabilityState, CliCapabilities, CliConnectionStatus,
        CliConnectionsSnapshot, CliOutputFormat,
    };
    let overview = quota_overview_fixture();
    let (model, visual) = cx.add_window_view(|_, cx| {
        let mut model = fixture(cx);
        with_settings_items(&mut model, cx);
        model
            .workspace
            .update(cx, |workspace, _| workspace.project_id = None);
        let availability = |backend: CliBackend| CliAvailability {
            backend,
            state: CliAvailabilityState::Available,
            executable_path: Some(format!("C:/CLI/{}.exe", backend.as_str()).into()),
            version: Some("1.0.0".into()),
            diagnostic: None,
            install_url: "",
            install_command: "",
            capabilities: CliCapabilities {
                output_format: CliOutputFormat::JsonLines,
                structured_output: true,
                native_resume: true,
                thinking_levels: vec![],
                auth_probe: true,
                read_only: true,
                model_catalog: true,
                action_frames: true,
                reasoning_blocks: false,
            },
            quota: vec![],
        };
        model.settings.update(cx, |settings, _| {
            settings.cli_connections = Some(CliConnectionsSnapshot {
                checked_at: "2026-10-21T12:00:00Z".into(),
                connections: [CliBackend::Codex, CliBackend::Claude]
                    .into_iter()
                    .map(|backend| CliConnectionStatus {
                        availability: availability(backend),
                        quota: overview.backend(backend).cloned(),
                    })
                    .collect(),
                overview: overview.clone(),
            });
        });
        model
    });
    for (width, height) in [(960.0, 640.0), (1440.0, 900.0)] {
        visual.simulate_resize(size(px(width), px(height)));
        open_settings(&model, visual, Some(SettingsPage::Connections));
        assert!(visual.debug_bounds("settings-connection:conn-1").is_some());
        model.update(visual, |model, cx| {
            model.open_settings_entry(crate::settings_search::entry_by_id("cli-connections"), cx);
        });
        visual.run_until_parked();
        let card = visual.debug_bounds("settings-cli:codex").unwrap();
        let pane = visual.debug_bounds("settings-content-scroll").unwrap();
        assert!(card.size.width > px(250.0));
        assert!(card.left() >= pane.left() && card.right() <= pane.right());
        for index in 0..2 {
            let bar = visual
                .debug_bounds(leak(format!("quota-window:settings:codex:{index}:bar")))
                .unwrap();
            assert!(bar.size.width > px(200.0));
            assert!(bar.left() >= card.left() && bar.right() <= card.right());
            let pace = visual
                .debug_bounds(leak(format!(
                    "quota-window:settings:codex:{index}:bar:pace"
                )))
                .unwrap();
            assert!(pace.left() >= bar.left() && pace.right() <= bar.right() + px(2.0));
        }
        let switch = visual.debug_bounds("quota-endpoint-codex").unwrap();
        assert!(switch.right() <= card.right());
        let claude = visual.debug_bounds("settings-cli:claude").unwrap();
        let disclosure = visual.debug_bounds("quota-disclosure:claude").unwrap();
        assert!(disclosure.left() >= claude.left() && disclosure.right() <= claude.right());
        for button in [
            "quota-disclosure-keep:claude",
            "quota-disclosure-off:claude",
        ] {
            let button = visual.debug_bounds(button).unwrap();
            assert!(button.size.height >= px(24.0));
            assert!(button.right() <= disclosure.right());
        }
    }
}

#[gpui::test]
fn the_quota_pill_shows_the_worst_window_and_opens_every_window(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    model.update(visual, |model, cx| {
        model.quota.overview = Some(quota_overview_fixture());
        cx.notify();
    });
    for (width, height) in [(960.0, 640.0), (1440.0, 900.0)] {
        visual.simulate_resize(size(px(width), px(height)));
        select(&model, visual, AppView::Home);
        let header = visual.debug_bounds("header").unwrap();
        let context = visual.debug_bounds("header-context").unwrap();
        let pill = visual.debug_bounds("header-quota").unwrap();
        let health = visual.debug_bounds("header-health").unwrap();
        assert!(
            context.right() <= pill.left(),
            "pill overlaps the readout at {width}"
        );
        assert!(pill.right() <= health.left());
        assert!(pill.top() >= header.top() && pill.bottom() <= header.bottom());
        assert!(pill.size.width > px(60.0));

        click(visual, "header-quota");
        let popover = visual
            .debug_bounds("quota-popover")
            .expect("the pill opens the popover");
        assert!(popover.top() >= header.bottom());
        assert!(popover.right() <= px(width) && popover.left() >= px(0.0));
        for backend in ["codex", "claude", "antigravity"] {
            let section = visual
                .debug_bounds(leak(format!("quota:popover:{backend}")))
                .unwrap();
            assert!(section.left() >= popover.left() && section.right() <= popover.right());
        }
        for index in 0..2 {
            let bar = visual
                .debug_bounds(leak(format!("quota-window:popover:codex:{index}:bar")))
                .unwrap();
            assert!(bar.size.width > px(150.0));
        }
        // Signed out: a state line, no bar.
        assert!(
            visual
                .debug_bounds("quota-window:popover:antigravity:0")
                .is_none()
        );
        assert!(visual.debug_bounds("quota-disclosure:claude").is_some());
        model.update(visual, |model, cx| {
            assert!(model.quota.popover_open);
            model.quota.popover_open = false;
            cx.notify();
        });
        visual.run_until_parked();

        // The Agents card of a Codex agent shows the same bars, tightest first,
        // under Configuration (ADR-040 M7).
        select(&model, visual, AppView::Agents);
        open_agent_drawer(&model, visual, crate::agents::AgentDetailTab::Configuration);
        let column = visual.debug_bounds("agents-cli-scroll").unwrap();
        let rows = visual.debug_bounds("agent-quota:task_specifier").unwrap();
        assert!(rows.left() >= column.left() && rows.right() <= column.right());
        let tightest = visual
            .debug_bounds("agent-quota:task_specifier:0:bar")
            .unwrap();
        assert!(tightest.size.width > px(150.0));
    }
}

#[gpui::test]
fn the_quota_pill_is_hidden_when_no_metered_cli_is_installed(cx: &mut TestAppContext) {
    // A separate window: debug bounds are never cleared between frames.
    let (_, visual) = cx.add_window_view(|_, cx| {
        let mut model = fixture(cx);
        let mut overview = quota_overview_fixture();
        for quota in &mut overview.backends {
            quota.state = ikore_core::quota::QuotaState::Unavailable(
                ikore_core::quota::UnavailableReason::CliMissing,
            );
            quota.snapshot = None;
        }
        model.quota.overview = Some(overview);
        model
    });
    visual.simulate_resize(size(px(960.0), px(640.0)));
    visual.run_until_parked();
    assert!(visual.debug_bounds("header-health").is_some());
    assert!(visual.debug_bounds("header-quota").is_none());
}
#[gpui::test]
fn permanent_deletion_keeps_settings_open_and_prevents_page_navigation(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(960.0), px(640.0)));
    open_settings(&model, visual, Some(SettingsPage::Projects));
    assert!(
        visual
            .debug_bounds("settings-row:project-deletion")
            .is_some()
    );
    model.update(visual, |model, cx| {
        model
            .settings
            .update(cx, |settings, _| settings.deletion_busy = true);
        cx.notify();
    });
    visual.run_until_parked();
    click(visual, "settings-close");
    click(visual, "settings-nav:general");
    assert!(model.read_with(visual, |model, _| model.settings_modal.open));
    assert_eq!(
        model.read_with(visual, |model, cx| model.settings.read(cx).page),
        SettingsPage::Projects
    );
}

#[gpui::test]
fn escape_clears_the_settings_search_before_closing_settings(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    select(&model, visual, AppView::Logs);
    open_settings(&model, visual, None);
    visual.simulate_input("timeout");
    visual.run_until_parked();
    assert!(model.read_with(visual, |m, _| {
        m.settings_modal
            .results()
            .is_some_and(|hits| hits.iter().any(|hit| hit.page == SettingsPage::Timeouts))
    }));
    assert!(
        visual
            .debug_bounds("settings-result:agent-timeouts")
            .is_some()
    );

    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    assert!(model.read_with(visual, |m, _| m.settings_modal.open));
    assert!(model.read_with(visual, |m, _| m.settings_modal.results().is_none()));

    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    assert!(!model.read_with(visual, |m, _| m.settings_modal.open));
    assert_eq!(
        model.read_with(visual, |m, _| m.active_view()),
        AppView::Logs
    );
}

#[gpui::test]
fn a_settings_search_result_opens_its_page_on_the_matching_row(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    open_settings(&model, visual, None);
    visual.simulate_input("timeout");
    visual.run_until_parked();
    click(visual, "settings-result:agent-timeouts");
    model.read_with(visual, |m, cx| {
        assert_eq!(m.settings.read(cx).page, SettingsPage::Timeouts);
        assert_eq!(m.settings_modal.highlight, Some("agent-timeouts"));
    });
    assert!(visual.debug_bounds("settings-row:agent-timeouts").is_some());

    model.update(visual, |m, cx| {
        m.settings_modal
            .search
            .update(cx, |input, cx| input.set_value("zzzz", cx));
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("settings-search-empty").is_some());
    assert!(model.read_with(visual, |m, _| {
        m.settings_modal
            .results()
            .is_some_and(|hits| hits.is_empty())
    }));
}

#[gpui::test]
fn settings_pickers_open_above_the_modal(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| {
        let mut model = fixture(cx);
        with_settings_items(&mut model, cx);
        model
    });
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    open_settings(&model, visual, Some(SettingsPage::Routes));
    click(visual, "settings-new-route");
    click(visual, "settings-route-strategy");
    click(visual, "choice:lkgp");
    model.read_with(visual, |m, cx| {
        assert_eq!(m.settings.read(cx).route_strategy.as_ref(), "lkgp");
        assert!(m.settings_modal.open);
    });
}

/// Search text and screen text come from the same index; every entry must be on its page (in
/// the list or the drill-in detail it belongs to).
#[gpui::test]
fn sanitization_preview_cannot_authorize_another_project_or_path(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| {
        let mut model = fixture(cx);
        with_settings_items(&mut model, cx);
        model
            .sanitization_path_input
            .update(cx, |input, cx| input.set_value("tests/fixture.rs", cx));
        model.settings.update(cx, |settings, _| {
            settings.sanitization_preview = Some((
                "another-project".into(),
                ikore_core::sanitization::SanitizationFilePreview {
                    relative_path: "tests/fixture.rs".into(),
                    original_file_sha256: "a".repeat(64),
                    spans: vec![ikore_core::secret_redactor::RedactionSpan {
                        start_byte: 10,
                        end_byte: 30,
                        detector_id: "known_api_token".into(),
                        detector_version: 1,
                        category: "known_api_token".into(),
                        marker: "<REDACTED_SECRET:KNOWN_API_TOKEN_1>".into(),
                    }],
                },
            ));
        });
        model.open_settings(Some(SettingsPage::Composition), cx);
        model
    });
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    visual.run_until_parked();
    assert!(visual.debug_bounds("sanitization-authorize-0").is_none());
    model.update(visual, |model, cx| {
        model.settings.update(cx, |settings, _| {
            settings.sanitization_preview.as_mut().unwrap().0 = "layout-project".into();
        });
        model
            .sanitization_path_input
            .update(cx, |input, cx| input.set_value("tests/other.rs", cx));
        cx.notify();
    });
    let (_, fresh) = visual.add_window_view(|_, _| ModelHarness(model.clone()));
    fresh.simulate_resize(size(px(1440.0), px(900.0)));
    fresh.run_until_parked();
    assert!(fresh.debug_bounds("sanitization-authorize-0").is_none());
}

#[gpui::test]
fn cargo_policy_drafts_are_bound_to_the_project_and_check(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| {
        let mut model = fixture(cx);
        with_settings_items(&mut model, cx);
        let presets = ikore_core::orchestration::composition::validation::profile::detect_presets(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
        );
        model.settings.update(cx, |settings, _| {
            let project = settings.snapshot.as_mut().unwrap().composition.as_mut().unwrap();
            project.profiles = vec![(presets[0].clone(), false)];
            project.presets = presets;
            project.cargo_policies.insert("cargo_check".into(),
                ikore_core::orchestration::composition::validation::cargo_policy::CargoValidationPolicyV1 {
                    additional_inputs: vec!["assets/icon.ico".into()],
                    toolchain: Some("1.98.0".into()),
                    ..Default::default()
                });
        });
        model.configure_cargo_profile("cargo_check".into(), cx);
        model.open_settings(Some(SettingsPage::Composition), cx);
        model
    });
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    visual.run_until_parked();
    model.read_with(visual, |model, cx| {
        assert_eq!(model.cargo_toolchain_input.read(cx).value(), "1.98.0");
        assert_eq!(
            model.cargo_additional_inputs_input.read(cx).value(),
            "assets/icon.ico"
        );
        assert!(
            !model
                .settings
                .read(cx)
                .snapshot
                .as_ref()
                .unwrap()
                .composition
                .as_ref()
                .unwrap()
                .profiles[0]
                .1
        );
    });
    assert!(visual.debug_bounds("cargo-policy-form").is_some());
    model.update(visual, |model, cx| {
        model.settings.update(cx, |settings, _| {
            settings
                .snapshot
                .as_mut()
                .unwrap()
                .composition
                .as_mut()
                .unwrap()
                .project_id = "different-project".into();
        });
        cx.notify();
    });
    let (_, fresh) = visual.add_window_view(|_, _| ModelHarness(model.clone()));
    fresh.simulate_resize(size(px(1440.0), px(900.0)));
    fresh.run_until_parked();
    assert!(fresh.debug_bounds("cargo-policy-form").is_none());
}

#[gpui::test]
fn every_settings_search_entry_is_rendered_on_its_page(cx: &mut TestAppContext) {
    use crate::settings_search::{ENTRIES, EntryScope};
    let (model, visual) = cx.add_window_view(|_, cx| {
        let mut model = fixture(cx);
        with_settings_items(&mut model, cx);
        model
    });
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    for page in SettingsPage::ALL {
        for scope in [
            EntryScope::Page,
            EntryScope::ConnectionDetail,
            EntryScope::RouteDetail,
        ] {
            let entries: Vec<_> = ENTRIES
                .iter()
                .filter(|entry| entry.page == page && entry.scope == scope)
                .collect();
            if entries.is_empty() {
                continue;
            }
            model.update(visual, |m, cx| {
                m.open_settings(Some(page), cx);
                m.settings.update(cx, |s, _| {
                    s.connection_detail = scope == EntryScope::ConnectionDetail;
                    s.route_detail = scope == EntryScope::RouteDetail;
                });
                cx.notify();
            });
            visual.run_until_parked();
            // A fresh window: the test context keeps selectors from earlier frames.
            let (_, fresh) = visual.add_window_view(|_, _| ModelHarness(model.clone()));
            fresh.simulate_resize(size(px(1440.0), px(900.0)));
            fresh.run_until_parked();
            for entry in entries {
                let row = leak(format!("settings-row:{}", entry.id));
                let section = leak(format!("settings-section:{}", entry.section));
                assert!(
                    fresh.debug_bounds(row).is_some() || fresh.debug_bounds(section).is_some(),
                    "`{}` is in the search index but not on the {page:?} page ({scope:?})",
                    entry.id
                );
            }
        }
    }
}

/// Hovers `selector` long enough for its tooltip to open.
fn hover(visual: &mut VisualTestContext, selector: &'static str) {
    let bounds = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("missing {selector}"));
    visual.simulate_mouse_move(bounds.center(), None, Modifiers::default());
    visual
        .executor()
        .advance_clock(std::time::Duration::from_millis(600));
    visual.run_until_parked();
}

#[gpui::test]
fn settings_explanations_sit_in_info_tips_that_open_on_hover(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| {
        let mut model = fixture(cx);
        with_settings_items(&mut model, cx);
        model
    });
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    open_settings(&model, visual, Some(SettingsPage::General));
    // The row keeps its title; the explanation is an "i" on the title line, inside the row.
    let row = visual
        .debug_bounds("settings-row:auto-open-review")
        .unwrap();
    let tip = visual.debug_bounds("info:auto-open-review").unwrap();
    assert!(row.contains(&tip.origin) && row.contains(&tip.bottom_right()));
    assert!(tip.size.width <= px(18.0) && tip.size.height <= px(18.0));
    // The page title carries its description the same way.
    assert!(visual.debug_bounds("info:page").is_some());
    hover(visual, "info:auto-open-review");
    let tooltip = visual
        .debug_bounds("tooltip")
        .expect("hovering the i shows its explanation");
    // Long explanations wrap instead of running off the window.
    assert!(tooltip.size.width <= px(321.0), "{tooltip:?}");
    assert!(tooltip.right() <= px(1440.0));
    // Section headings with a description get one too.
    open_settings(&model, visual, Some(SettingsPage::SecondBrain));
    assert!(visual.debug_bounds("info:section:Note expansion").is_some());
}

#[gpui::test]
fn a_connection_test_result_shows_its_outcome_latency_and_advice(cx: &mut TestAppContext) {
    use crate::settings::{ConnectionTestOutcome, ConnectionTestResult};
    let (model, visual) = cx.add_window_view(|_, cx| {
        let mut model = fixture(cx);
        with_settings_items(&mut model, cx);
        model
    });
    visual.simulate_resize(size(px(1280.0), px(800.0)));
    open_settings(&model, visual, Some(SettingsPage::Connections));
    click(visual, "settings-connection:conn-1");
    let action = visual.debug_bounds("settings-test-connection").unwrap();
    model.update(visual, |m, cx| {
        m.settings.update(cx, |settings, _| {
            settings.connection_tests.insert(
                "conn-1".into(),
                ConnectionTestResult {
                    outcome: ConnectionTestOutcome::AuthenticationFailed,
                    latency_ms: Some(212),
                    model: "gpt-4.1".into(),
                    detail: "invalid api key".into(),
                    at: "08:31".into(),
                },
            );
        });
        cx.notify();
    });
    visual.run_until_parked();
    let result = visual
        .debug_bounds("connection-test-result")
        .expect("the last result renders under the action");
    let modal = visual.debug_bounds("settings-modal").unwrap();
    // Beside the action, or under it when the line wraps.
    assert!(
        result.left() >= action.right() || result.top() >= action.bottom(),
        "{result:?} vs {action:?}"
    );
    assert!(result.right() <= modal.right());
    assert!(visual.debug_bounds("info:connection-test-advice").is_some());
    // The list row reports the same test once the detail is closed.
    click(visual, "settings-back-connections");
    let summary = model.read_with(visual, |m, cx| {
        m.settings.read(cx).connection_tests["conn-1"].summary()
    });
    assert_eq!(summary, "Authentication failed · 212 ms · 08:31");
}

#[gpui::test]
fn hovering_a_vault_dot_names_its_file_above_it(cx: &mut TestAppContext) {
    use ikore_core::workspace_service::{GetWorkspaceVaultIndexResponse, VaultNoteSummary};
    let note = |path: &str, title: &str| VaultNoteSummary {
        relative_path: path.into(),
        title: title.into(),
        revision: "r1".into(),
        frontmatter: None,
        normalized_properties: None,
        links: Vec::new(),
        outgoing_paths: Vec::new(),
        backlink_paths: Vec::new(),
        schema_warnings: Vec::new(),
        source_files: Vec::new(),
        test_files: Vec::new(),
    };
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    model.update(visual, |m, cx| {
        m.vault.update(cx, |vault, _| {
            vault.workspace_root = Some("C:\\vault".into());
            vault.index = Some(GetWorkspaceVaultIndexResponse {
                notes: vec![
                    note("Features/Login.md", "Login"),
                    note("Features/Checkout flow with a long name.md", "Checkout"),
                ],
                edges: Vec::new(),
                broken_links: Vec::new(),
                broken_file_references: Vec::new(),
                indexed_at: "now".into(),
            });
        });
    });
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    select(&model, visual, AppView::Vault);
    let path = "Features/Checkout flow with a long name.md";
    let dot = visual
        .debug_bounds("vault-node:Features/Checkout flow with a long name.md")
        .expect("the note is drawn as a dot");
    visual.simulate_mouse_move(dot.center(), None, Modifiers::default());
    visual.run_until_parked();
    assert_eq!(
        model.read_with(visual, |m, cx| m.vault.read(cx).camera.hovered.clone()),
        Some(path.to_string())
    );
    let chip = visual
        .debug_bounds("vault-node-hover")
        .expect("hovering a dot shows its file name");
    // Read the dot again: the graph fits itself to the panel once it knows its size.
    let dot = visual
        .debug_bounds("vault-node:Features/Checkout flow with a long name.md")
        .unwrap();
    let graph = visual.debug_bounds("vault-graph").unwrap();
    // Above the dot, centred on it, on one line and inside the graph.
    assert!(chip.bottom() <= dot.top(), "{chip:?} vs {dot:?}");
    assert!(
        (chip.center().x - dot.center().x).abs() <= px(1.0),
        "{chip:?} vs {dot:?}"
    );
    assert!(chip.size.height < px(28.0), "{chip:?}");
    assert!(chip.top() >= graph.top());
    // Leaving the dot clears it. (The chip is drawn only from `camera.hovered`; a second window
    // over the same graph would fight this one over the camera fit, so no `assert_absent`.)
    visual.simulate_mouse_move(
        point(graph.left() + px(4.0), graph.bottom() - px(4.0)),
        None,
        Modifiers::default(),
    );
    visual.run_until_parked();
    assert_eq!(
        model.read_with(visual, |m, cx| m.vault.read(cx).camera.hovered.clone()),
        None
    );
}

#[gpui::test]
fn forge_headings_carry_their_explanation_in_an_info_tip(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(1280.0), px(800.0)));
    select(&model, visual, AppView::Forge);
    let tip = visual
        .debug_bounds("info:forge-section")
        .expect("the section description is an i beside the section tabs");
    // Right of the tab strip, on its line, whichever section is open.
    let last_tab = visual.debug_bounds("forge-section-delivery").unwrap();
    assert!(tip.left() >= last_tab.right(), "{tip:?} vs {last_tab:?}");
    assert!((tip.center().y - last_tab.center().y).abs() <= px(2.0));
    hover(visual, "info:forge-section");
    let tooltip = visual.debug_bounds("tooltip").unwrap();
    assert!(tooltip.size.width <= px(321.0));
    assert!(tip.size.width <= px(18.0));
}

#[gpui::test]
fn context_rail_is_absent_on_home_and_present_on_workbench_views(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    for view in [AppView::Pulsations, AppView::Agents, AppView::Chat] {
        select(&model, visual, view);
        let rail = visual.debug_bounds("context-rail").unwrap();
        let center = visual.debug_bounds("shell-center").unwrap();
        let status_bar = visual.debug_bounds("status-bar").unwrap();
        assert_eq!(rail.right(), px(1440.0), "{view:?}");
        assert_eq!(
            rail.size.width,
            crate::sizing::CONTEXT_RAIL_WIDTH,
            "{view:?}"
        );
        assert_eq!(center.right(), rail.left(), "{view:?}");
        assert_eq!(rail.bottom(), status_bar.top(), "{view:?}");
        for selector in [
            "context-rail-toggle",
            "rail-card-pipeline",
            "rail-card-waiting",
            "rail-card-actions",
            "rail-card-health",
        ] {
            let card = visual.debug_bounds(selector).unwrap();
            assert!(
                card.left() >= rail.left() && card.right() <= rail.right(),
                "{selector} escaped the rail in {view:?}: {card:?}"
            );
        }
        // The fixture's pulsation is at Specification: the vertical stepper stacks the stages.
        let spec = visual
            .debug_bounds("rail-pipeline-stage-specification")
            .unwrap();
        let code = visual.debug_bounds("rail-pipeline-stage-code").unwrap();
        assert!(code.top() > spec.bottom(), "{view:?}");
    }
    // Last, because `assert_absent` opens a second window: a split-pane view rendered in two
    // windows of different widths re-measures itself forever (see `split_pane`'s canvas).
    select(&model, visual, AppView::Home);
    assert_absent(&model, visual, "context-rail");
}

#[gpui::test]
fn context_rail_auto_collapses_at_the_minimum_width(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    select(&model, visual, AppView::Pulsations);

    visual.simulate_resize(size(px(960.0), px(640.0)));
    visual.run_until_parked();
    let rail = visual.debug_bounds("context-rail").unwrap();
    assert_eq!(rail.size.width, crate::sizing::CONTEXT_RAIL_COLLAPSED_WIDTH);
    for selector in [
        "rail-strip-pipeline",
        "rail-strip-waiting",
        "rail-strip-health",
    ] {
        let item = visual.debug_bounds(selector).unwrap();
        assert!(
            item.left() >= rail.left() && item.right() <= rail.right(),
            "{selector}"
        );
    }
    // Forced by the window width: the toggle is inert.
    click(visual, "context-rail-toggle");
    assert!(!model.read_with(visual, |m, cx| m.context_rail_collapsed(cx)));
    // The centre column keeps every split above its minimum width.
    let columns = visual.debug_bounds("pulsations-columns").unwrap();
    assert!(columns.size.width >= px(640.0), "{columns:?}");

    visual.simulate_resize(size(px(1440.0), px(900.0)));
    visual.run_until_parked();
    assert_eq!(
        visual.debug_bounds("context-rail").unwrap().size.width,
        crate::sizing::CONTEXT_RAIL_WIDTH
    );
    click(visual, "context-rail-toggle");
    assert!(model.read_with(visual, |m, cx| m.context_rail_collapsed(cx)));
    assert_eq!(
        visual.debug_bounds("context-rail").unwrap().size.width,
        crate::sizing::CONTEXT_RAIL_COLLAPSED_WIDTH
    );
    // (The fold is persisted through the session's `sidebar_collapsed` column; see
    // `persists_layout_and_domain_selections_with_debounce`.)
    click(visual, "context-rail-toggle");
    assert!(!model.read_with(visual, |m, cx| m.context_rail_collapsed(cx)));
    assert_eq!(
        visual.debug_bounds("context-rail").unwrap().size.width,
        crate::sizing::CONTEXT_RAIL_WIDTH
    );
    // Folded again on a view without split panes, the cards are gone (second window: see the
    // note in `context_rail_is_absent_on_home_and_present_on_workbench_views`).
    select(&model, visual, AppView::Vault);
    model.update(visual, |m, cx| m.set_context_rail_collapsed(true, cx));
    visual.run_until_parked();
    assert_absent(&model, visual, "rail-card-pipeline");
}

#[gpui::test]
fn context_rail_quick_action_runs_the_primary_step(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    select(&model, visual, AppView::Vault);
    // Planning pulsation: primary = Follow pipeline.
    click(visual, "rail-step-flow-follow-pipeline");
    assert_eq!(
        model.read_with(visual, |m, _| m.active_view()),
        AppView::Pulsations
    );
    // The waiting card lists a pending review and jumps to it.
    select(&model, visual, AppView::Vault);
    assert!(visual.debug_bounds("rail-waiting-hitl").is_none());
    model.update(visual, |m, cx| {
        m.pulsations.update(cx, |pulsations, _| {
            pulsations.awaiting_hitl = 1;
        });
        cx.notify();
    });
    visual.run_until_parked();
    click(visual, "rail-waiting-hitl");
    assert_eq!(
        model.read_with(visual, |m, _| m.active_view()),
        AppView::Pulsations
    );
    assert_eq!(
        model.read_with(visual, |m, cx| m.pulsations.read(cx).detail_section),
        PulsationDetailSection::Review
    );
    // Folded, the strip's action icon runs the same primary step.
    select(&model, visual, AppView::Vault);
    model.update(visual, |m, cx| m.set_context_rail_collapsed(true, cx));
    visual.run_until_parked();
    click(visual, "rail-strip-actions");
    assert_eq!(
        model.read_with(visual, |m, _| m.active_view()),
        AppView::Pulsations
    );
}

#[gpui::test]
fn home_waiting_list_jumps_to_review_for_a_checkpoint(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    select(&model, visual, AppView::Home);
    let waiting = visual.debug_bounds("home-waiting").unwrap();
    assert!(waiting.size.width > px(0.0));
    assert!(visual.debug_bounds("home-waiting-hitl").is_none());
    model.update(visual, |m, cx| {
        m.pulsations.update(cx, |pulsations, _| {
            pulsations.awaiting_hitl = 1;
            pulsations.awaiting_failure = 2;
        });
        cx.notify();
    });
    visual.run_until_parked();
    let hitl = visual.debug_bounds("home-waiting-hitl").unwrap();
    let failures = visual.debug_bounds("home-waiting-failures").unwrap();
    assert!(hitl.top() >= waiting.top());
    assert!(failures.top() >= hitl.bottom());
    click(visual, "home-waiting-hitl");
    assert_eq!(
        model.read_with(visual, |m, _| m.active_view()),
        AppView::Pulsations
    );
    assert_eq!(
        model.read_with(visual, |m, cx| m.pulsations.read(cx).detail_section),
        PulsationDetailSection::Review
    );
    assert_eq!(
        model.read_with(visual, |m, cx| m.pulsations.read(cx).active_id()),
        Some("pulse-a".to_string())
    );
}

#[gpui::test]
fn home_activity_lists_recent_events_newest_first(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    select(&model, visual, AppView::Home);
    let activity = visual.debug_bounds("home-activity").unwrap();
    assert!(activity.size.width > px(0.0));
    assert!(visual.debug_bounds("home-activity-0").is_none());
    model.update(visual, |m, cx| {
        m.record_activity(
            crate::activity::ActivityKind::Workspace,
            "src/a.rs changed",
            Some(crate::flow::FlowTarget::View(AppView::Workspace)),
            false,
        );
        m.record_activity(
            crate::activity::ActivityKind::Agent,
            "BackendAgent · Failed",
            Some(crate::flow::FlowTarget::View(AppView::Agents)),
            true,
        );
        m.record_activity(
            crate::activity::ActivityKind::Notice,
            "Provider cooldown",
            None,
            true,
        );
        cx.notify();
    });
    visual.run_until_parked();
    let first = visual.debug_bounds("home-activity-0").unwrap();
    let second = visual.debug_bounds("home-activity-1").unwrap();
    let third = visual.debug_bounds("home-activity-2").unwrap();
    assert!(first.top() >= activity.top());
    assert!(second.top() >= first.bottom());
    assert!(third.top() >= second.bottom());
    // Entry 1 is the agent failure (newest first): clicking it opens Agents.
    click(visual, "home-activity-1");
    assert_eq!(
        model.read_with(visual, |m, _| m.active_view()),
        AppView::Agents
    );
    select(&model, visual, AppView::Home);
    click(visual, "home-open-logs");
    assert_eq!(
        model.read_with(visual, |m, _| m.active_view()),
        AppView::Logs
    );
}

#[gpui::test]
fn home_stacks_its_cards_at_the_minimum_width(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(960.0), px(640.0)));
    select(&model, visual, AppView::Home);
    let hero = visual.debug_bounds("home-hero").unwrap();
    let waiting = visual.debug_bounds("home-waiting").unwrap();
    let health = visual.debug_bounds("home-health").unwrap();
    let activity = visual.debug_bounds("home-activity").unwrap();
    assert!(waiting.top() >= hero.bottom());
    assert!(
        health.top() >= waiting.bottom(),
        "{health:?} vs {waiting:?}"
    );
    assert!(
        activity.top() >= health.bottom(),
        "{activity:?} vs {health:?}"
    );
    assert!(activity.right() <= px(960.0));

    visual.simulate_resize(size(px(1440.0), px(900.0)));
    visual.run_until_parked();
    let waiting = visual.debug_bounds("home-waiting").unwrap();
    let health = visual.debug_bounds("home-health").unwrap();
    let activity = visual.debug_bounds("home-activity").unwrap();
    assert_eq!(waiting.top(), health.top());
    assert_eq!(health.top(), activity.top());
    assert!(health.left() >= waiting.right());
    assert!(activity.left() >= health.right());
}

#[gpui::test]
fn pulsation_status_transitions_feed_the_activity_list(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    select(&model, visual, AppView::Home);
    assert!(model.read_with(visual, |m, _| m.activity.is_empty()));
    // pulse-a moves from Planning to Checkpoint: one attention line, jumping to its Review.
    model.update(visual, |m, cx| {
        let mut pulsations = m.pulsations.read(cx).pulsations.clone();
        pulsations[0].status = PulsationStatus::Checkpoint;
        pulsations[0].stage = PulsationStage::Code;
        m.apply_pulsation_snapshot(
            "layout-project".into(),
            ikore_core::persistence_service::PulsationRuntimeSnapshot {
                details: Vec::new(),
                coordination: None,
                main_agents_count: 0,
                hitl_checkpoints: Vec::new(),
                pulsations,
                hitl_waiting_count: 0,
                failure_checkpoints: Vec::new(),
                verification_error_checkpoints: Vec::new(),
                failure_waiting_count: 0,
                background_agents_count: 0,
                active_jobs: Vec::new(),
                brownfield_coverage: None,
            },
            None,
            cx,
        );
    });
    visual.run_until_parked();
    let lines: Vec<String> = model.read_with(visual, |m, _| {
        m.activity
            .recent(5)
            .map(|entry| entry.summary.to_string())
            .collect()
    });
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("Layout regression"), "{lines:?}");
    assert!(lines[0].contains("Code"), "{lines:?}");
    assert!(visual.debug_bounds("home-activity-0").is_some());
}

#[gpui::test]
fn vault_columns_resize_and_expanding_the_graph_drops_them(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    model.update(visual, |m, cx| {
        m.vault.update(cx, |vault, _| {
            vault.workspace_root = Some("C:\\vault".into());
        });
    });
    for (width, height) in [(960.0, 640.0), (1440.0, 900.0)] {
        visual.simulate_resize(size(px(width), px(height)));
        select(&model, visual, AppView::Vault);
        let center = visual.debug_bounds("shell-center").unwrap();
        let files = visual.debug_bounds("vault-columns-left").unwrap();
        let files_sash = visual.debug_bounds("vault-columns-sash").unwrap();
        let graph = visual.debug_bounds("vault-detail-columns-left").unwrap();
        let detail_sash = visual.debug_bounds("vault-detail-columns-sash").unwrap();
        let inspector = visual.debug_bounds("vault-detail-columns-right").unwrap();
        assert!(files.left() >= center.left(), "{width}");
        assert!(files.right() <= files_sash.left(), "{width}");
        assert!(files_sash.right() <= graph.left(), "{width}");
        assert!(graph.right() <= detail_sash.left(), "{width}");
        assert!(detail_sash.right() <= inspector.left(), "{width}");
        assert!(
            inspector.right() <= center.right(),
            "{width}: {inspector:?} vs {center:?}"
        );
        assert!(
            graph.size.width >= px(200.0),
            "{width}: graph squeezed to {graph:?}"
        );
        assert!(
            inspector.size.width >= px(180.0),
            "{width}: inspector squeezed to {inspector:?}"
        );
    }
    // Dragging the files sash widens the files column and the detail split follows.
    let sash = visual.debug_bounds("vault-columns-sash").unwrap().center();
    let before = visual
        .debug_bounds("vault-columns-left")
        .unwrap()
        .size
        .width;
    visual.simulate_mouse_down(sash, gpui::MouseButton::Left, Modifiers::default());
    visual.simulate_mouse_move(
        point(sash.x + px(80.0), sash.y),
        gpui::MouseButton::Left,
        Modifiers::default(),
    );
    visual.simulate_mouse_up(
        point(sash.x + px(80.0), sash.y),
        gpui::MouseButton::Left,
        Modifiers::default(),
    );
    visual.run_until_parked();
    let after = visual
        .debug_bounds("vault-columns-left")
        .unwrap()
        .size
        .width;
    assert_eq!(after, before + px(80.0));
    assert!(
        visual
            .debug_bounds("vault-detail-columns-right")
            .unwrap()
            .right()
            <= px(1440.0)
    );
    // Expanding the graph removes both side columns and their sashes (absence last: it opens a
    // second window).
    click(visual, "vault-expand-graph");
    assert!(model.read_with(visual, |m, cx| m.vault.read(cx).graph_expanded));
    let graph = visual.debug_bounds("vault-graph-scroll").unwrap();
    let center = visual.debug_bounds("shell-center").unwrap();
    assert!(
        graph.size.width >= center.size.width - px(48.0),
        "{graph:?} vs {center:?}"
    );
    assert_absent(&model, visual, "vault-columns-sash");
}

#[gpui::test]
fn an_unvalidated_cli_fallback_offers_validate_switch_or_abort_and_a_flow_step(
    cx: &mut TestAppContext,
) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    let fallback = |code: &str| ikore_core::persistence_service::CliFallbackCheckpoint {
        checkpoint_id: "cp-gemini".into(),
        pulsation_id: "pulse-a".into(),
        project_id: "layout-project".into(),
        feature_name: "Layout regression".into(),
        agent_role: AgentRole::ContractAgent,
        terminal_run_id: None,
        error_code: code.into(),
        diagnostic: "Gemini v99.0.0 is newer than the last validated version.".into(),
        state_version: 3,
        stage: PulsationStage::Contract,
        created_at: "now".into(),
    };
    model.update(visual, |m, cx| {
        m.agents.update(cx, |agents, _| {
            agents.fallbacks = vec![fallback(
                ikore_core::persistence_service::CLI_VERSION_UNVALIDATED,
            )];
        });
        cx.notify();
    });
    visual.run_until_parked();

    // Home and the context rail surface the decision as the primary step and a waiting row.
    select(&model, visual, AppView::Home);
    assert!(visual.debug_bounds("home-next-step").is_some());
    let steps = model.read_with(visual, crate::flow::next_steps);
    assert_eq!(steps[0].id, "flow-cli-fallback");
    assert_eq!(steps[0].label.as_ref(), "Validate updated CLI");
    assert!(visual.debug_bounds("home-waiting-fallbacks").is_some());
    click(visual, "home-next-step");
    assert_eq!(
        model.read_with(visual, |m, _| m.active_view()),
        AppView::Agents
    );
    assert_eq!(
        model.read_with(visual, |m, cx| m.agents.read(cx).selected_agent.clone()),
        Some(AgentIdentifier::Canonical(AgentRole::ContractAgent))
    );

    // Three decisions for an updated CLI awaiting validation; no plain "Retry CLI".
    let card = visual.debug_bounds("cli-fallback:cp-gemini").unwrap();
    for selector in [
        "fallback-validate:cp-gemini",
        "fallback-api:cp-gemini",
        "fallback-cancel:cp-gemini",
    ] {
        let button = visual.debug_bounds(selector).unwrap();
        assert!(
            button.left() >= card.left() && button.right() <= card.right(),
            "{selector} outside the card"
        );
    }
    assert!(visual.debug_bounds("fallback-retry:cp-gemini").is_none());

    // After a failed validation only switching provider or aborting remain.
    model.update(visual, |m, cx| {
        m.agents.update(cx, |agents, _| {
            agents.fallbacks = vec![fallback(
                ikore_core::persistence_service::CLI_VALIDATION_FAILED,
            )];
        });
        cx.notify();
    });
    visual.run_until_parked();
    let steps = model.read_with(visual, crate::flow::next_steps);
    assert_eq!(steps[0].label.as_ref(), "Switch provider or abort");
    assert!(visual.debug_bounds("fallback-api:cp-gemini").is_some());
    assert!(visual.debug_bounds("fallback-cancel:cp-gemini").is_some());
    // (No `assert_absent` here: Agents renders split panes, and a second window of another
    // width would re-measure them forever — see `context_rail_is_absent_on_home_...`.)
}

/// Feature 25, M1 (criterion 1): the Agents page is a canvas. The default box
/// carries the pipeline's state in its title; its stages sit on one row, left
/// to right, a human checkpoint marked on the stages that have one; the other
/// agents and the `New freeform agent` ghost sit below, without links.
/// Nothing is chosen for the developer: the synthesis waits for a click.
#[gpui::test]
fn agents_canvas_draws_the_pipeline_box_and_its_nodes(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    select(&model, visual, AppView::Agents);
    for (width, height) in [(960.0, 640.0), (1440.0, 900.0)] {
        visual.simulate_resize(size(px(width), px(height)));
        visual.run_until_parked();
        let agents_header = visual.debug_bounds("agents-header").unwrap();
        let fit = visual.debug_bounds("agents-fit").unwrap();
        let refresh = visual.debug_bounds("agents-refresh").unwrap();
        assert!(fit.top() >= agents_header.top() && fit.bottom() <= agents_header.bottom());
        assert!(fit.right() <= refresh.left() && refresh.right() <= px(width));
        // The stepper left the header: the box's title says where the pulsation is.
        assert!(visual.debug_bounds("agents-pipeline").is_none());
        let canvas = visual.debug_bounds("agents-canvas").unwrap();
        let frame = visual.debug_bounds("agents-box:default").unwrap();
        let title = visual.debug_bounds("agents-box-title:default").unwrap();
        assert!(frame.left() >= canvas.left() && frame.top() >= canvas.top());
        assert!(title.top() >= frame.top() && title.bottom() <= frame.bottom());
        let spec = visual
            .debug_bounds("agents-node:role:task_specifier")
            .unwrap();
        let code = visual
            .debug_bounds("agents-node:role:backend_agent")
            .unwrap();
        assert!(spec.right() < code.left(), "{spec:?} {code:?}");
        assert_eq!(spec.top(), code.top());
        let free = visual.debug_bounds("agents-node:slot:slot-free").unwrap();
        let ghost = visual
            .debug_bounds("agents-node-ghost:new-free-form")
            .unwrap();
        assert!(free.top() > code.bottom() && ghost.top() == free.top());
        assert!(free.right() <= ghost.left());
        for node in [spec, code, free, ghost] {
            assert!(
                node.left() >= frame.left()
                    && node.right() <= frame.right()
                    && node.bottom() <= frame.bottom(),
                "{node:?} outside {frame:?}"
            );
        }
        assert!(
            visual
                .debug_bounds("agents-node-gate:role:task_specifier")
                .is_some()
        );
        assert!(
            visual
                .debug_bounds("agents-node-gate:role:backend_agent")
                .is_none()
        );
        assert!(visual.debug_bounds("agents-synthesis").is_none());
    }
    assert!(model.read_with(visual, |model, cx| {
        model.pulsations.read(cx).active_id().as_deref() == Some("pulse-a")
            && model.agents.read(cx).selected_agent.is_none()
    }));
}

/// Feature 25, M1 (criterion 2): a node clicked opens its synthesis on the
/// right and pins the dock on it; the model picker's scrim keeps the
/// selection; the background closes the synthesis; an agent in API Mode shows
/// its API panel.
#[gpui::test]
fn agent_selection_and_modal_preserve_detail_geometry(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(960.0), px(640.0)));
    select(&model, visual, AppView::Agents);
    assert!(visual.debug_bounds("agent-card:task_specifier").is_none());
    click(visual, "agents-node:role:task_specifier");
    assert!(visual.debug_bounds("agent-card:task_specifier").is_some());
    click(visual, "agents-node:role:backend_agent");
    assert!(visual.debug_bounds("agent-card:backend_agent").is_some());
    let synthesis = visual.debug_bounds("agents-synthesis").unwrap();
    let canvas = visual.debug_bounds("agents-canvas").unwrap();
    assert!(
        canvas.right() <= synthesis.left(),
        "{canvas:?} {synthesis:?}"
    );
    assert!(synthesis.size.width >= px(crate::agents_canvas::SYNTHESIS_MIN_WIDTH));
    let backend = crate::agents::AgentIdentifier::Canonical(AgentRole::BackendAgent);
    model.read_with(visual, |m, _| {
        assert!(m.agent_dock.follow.pinned());
        assert_eq!(m.agent_dock.shown(), Some(backend.clone()));
    });
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
    click(visual, "agents-node:role:task_specifier");
    assert_eq!(
        model.read_with(visual, |model, cx| model
            .agents
            .read(cx)
            .selected_agent
            .clone()),
        Some(backend.clone())
    );
    assert!(!model.read_with(visual, |m, cx| {
        m.agents.read(cx).active_model_picker.is_some()
    }));
    // The scrim's click closes the picker; the next frame draws the page
    // without it.
    model.update(visual, |_, cx| cx.notify());
    visual.run_until_parked();
    assert!(visual.debug_bounds("cli-model-picker-content").is_none());
    // A click on the canvas's background closes the synthesis.
    let frame = visual.debug_bounds("agents-canvas").unwrap();
    visual.simulate_click(
        point(frame.left() + px(4.0), frame.bottom() - px(4.0)),
        Modifiers::default(),
    );
    visual.run_until_parked();
    assert_eq!(
        model.read_with(visual, |m, cx| m.agents.read(cx).selected_agent.clone()),
        None
    );
    click(visual, "agents-node:role:backend_agent");
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
    assert!(visual.debug_bounds("agents-node:slot:slot-free").is_some());
}

#[gpui::test]
fn agents_detail_accepts_vertical_wheel_scrolling(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(960.0), px(640.0)));
    select(&model, visual, AppView::Agents);
    // The Configure drawer is taller than the column (feature 25, M1).
    open_agent_drawer(&model, visual, crate::agents::AgentDetailTab::Configuration);

    let scroll = visual.debug_bounds("agents-cli-scroll").unwrap();
    let before = visual
        .debug_bounds("agent-start:task_specifier")
        .expect("start action should be present in the synthesis header");
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
        "Agents synthesis did not move after a downward wheel event: {before:?} -> {after:?}; \
         panel={panel:?}; header={header:?}; content={content:?}; columns={columns:?}; \
         scroll={scroll:?}"
    );
}

/// ADR-040 M2: the console shows the selected run as blocks, newest at the
/// bottom; scrolling up pauses it and counts what arrives, and the pill
/// brings the newest block back. On the Agents page it is the dock's
/// (feature 25, M1).
#[gpui::test]
fn agent_console_follows_pauses_and_resumes(cx: &mut TestAppContext) {
    use ikore_core::terminals::{
        EventDetailV1, ExecutionOrigin, SessionSlot, TerminalEvent, TerminalEventEnvelope,
        TerminalEventKind, TerminalRun, TerminalRunStatus, ToolKind,
    };

    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(1280.0), px(820.0)));
    let run = TerminalRun {
        id: "console-run".into(),
        project_id: "layout-project".into(),
        slot: SessionSlot::Canonical {
            agent_role: AgentRole::TaskSpecifier,
        },
        slot_id: "canonical:layout-project:task_specifier".into(),
        agent_role: Some(AgentRole::TaskSpecifier),
        origin: ExecutionOrigin::Manual,
        execution_mode: AgentExecutionMode::CliProcess,
        backend: Some(CliBackend::Claude),
        model: Some("claude-haiku-5-5".into()),
        requested_model_profile: None,
        model_source: CliModelSelectionSource::Default,
        observed_model: None,
        native_session_id: None,
        pulsation_id: None,
        job_id: None,
        agent_run_id: None,
        llm_execution_id: None,
        status: TerminalRunStatus::Running,
        exit_code: None,
        error_code: None,
        error_message: None,
        created_at: "2026-11-15T10:00:00Z".into(),
        started_at: Some("2026-11-15T10:00:00Z".into()),
        finished_at: None,
        updated_at: "2026-11-15T10:00:00Z".into(),
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
        observation_trial_id: None,
    };
    let envelope = |sequence: u64| TerminalEventEnvelope {
        protocol_version: 2,
        project_id: "layout-project".into(),
        agent_role: Some(AgentRole::TaskSpecifier),
        run_id: "console-run".into(),
        sequence,
        run: run.clone(),
        event: TerminalEvent {
            id: format!("console-event-{sequence}"),
            run_id: "console-run".into(),
            sequence,
            kind: TerminalEventKind::ToolUse,
            stream: None,
            content: format!("Read · src/file_{sequence}.rs"),
            redaction_count: 0,
            redaction_categories: vec![],
            created_at: "2026-11-15T10:00:00Z".into(),
            detail: Some(EventDetailV1::ActionCall {
                v: 1,
                ordinal: u32::try_from(sequence).unwrap(),
                kind: ToolKind::Read,
                name: "Read".into(),
            }),
            observed_at: None,
        },
    };
    let deliver = |visual: &mut VisualTestContext, sequences: std::ops::RangeInclusive<u64>| {
        model.update(visual, |model, cx| {
            model.agents.update(cx, |agents, _| {
                for sequence in sequences {
                    agents.record_event(envelope(sequence));
                }
            });
            cx.notify();
        });
        visual.run_until_parked();
    };
    // The page first: its own refresh settles before the run starts. A click
    // on the node pins the dock, whose console is the page's (feature 25, M1).
    select(&model, visual, AppView::Agents);
    click(visual, "agents-node:role:task_specifier");
    deliver(visual, 1..=60);
    let console = visual
        .debug_bounds("agent-console:dock")
        .expect("the dock shows the clicked agent's run");
    // The newest block is in view; the first is far above.
    assert!(visual.debug_bounds("agent-console-block:59").is_some());
    assert!(visual.debug_bounds("agent-console-block:0").is_none());
    assert!(visual.debug_bounds("agent-console-new-pill").is_none());

    // Reading earlier blocks pauses the console, and what arrives is counted.
    visual.simulate_event(ScrollWheelEvent {
        position: console.center(),
        delta: ScrollDelta::Pixels(point(px(0.0), px(2000.0))),
        ..Default::default()
    });
    visual.run_until_parked();
    deliver(visual, 61..=63);
    assert!(visual.debug_bounds("agent-console-new-pill").is_some());
    assert!(visual.debug_bounds("agent-console-block:62").is_none());

    // The pill brings the newest block back and resumes following. GPUI keeps
    // the bounds of an element that was drawn once, so the state says the
    // pill is gone.
    click(visual, "agent-console-new-pill");
    visual.run_until_parked();
    let (following, unseen) = model.update(visual, |model, cx| {
        model.agents.update(cx, |agents, _| {
            let surface = agents.consoles.surface_mut(crate::agent_dock::DOCK_SURFACE);
            (surface.following(), surface.unseen)
        })
    });
    assert!(following);
    assert_eq!(unseen, 0);
    assert!(visual.debug_bounds("agent-console-block:62").is_some());
}

/// A run of `role` in the layout project, for the dock tests.
fn dock_run(
    role: AgentRole,
    id: &str,
    origin: ikore_core::terminals::ExecutionOrigin,
    status: ikore_core::terminals::TerminalRunStatus,
) -> ikore_core::terminals::TerminalRun {
    ikore_core::terminals::TerminalRun {
        id: id.into(),
        project_id: "layout-project".into(),
        slot: ikore_core::terminals::SessionSlot::Canonical { agent_role: role },
        slot_id: format!("canonical:layout-project:{}", role.as_str()),
        agent_role: Some(role),
        origin,
        execution_mode: AgentExecutionMode::CliProcess,
        backend: Some(CliBackend::Claude),
        model: Some("claude-haiku-5-5".into()),
        requested_model_profile: None,
        model_source: CliModelSelectionSource::Default,
        observed_model: None,
        native_session_id: None,
        pulsation_id: None,
        job_id: None,
        agent_run_id: None,
        llm_execution_id: None,
        status,
        exit_code: None,
        error_code: None,
        error_message: None,
        created_at: "2026-11-15T10:00:00Z".into(),
        started_at: Some("2026-11-15T10:00:00Z".into()),
        finished_at: None,
        updated_at: "2026-11-15T10:00:00Z".into(),
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
        observation_trial_id: None,
    }
}

/// One event of `run`, delivered as `handle_event` does: the dock learns of
/// the run, then the agents' buffer takes the event.
fn deliver_run_event(
    model: &Entity<AppModel>,
    visual: &mut VisualTestContext,
    run: &ikore_core::terminals::TerminalRun,
    sequence: u64,
) {
    use ikore_core::terminals::{TerminalEvent, TerminalEventEnvelope, TerminalEventKind};
    let envelope = TerminalEventEnvelope {
        protocol_version: 2,
        project_id: "layout-project".into(),
        agent_role: run.agent_role,
        run_id: run.id.clone(),
        sequence,
        run: run.clone(),
        event: TerminalEvent {
            id: format!("{}-{sequence}", run.id),
            run_id: run.id.clone(),
            sequence,
            kind: TerminalEventKind::System,
            stream: None,
            content: "Run in progress.".into(),
            redaction_count: 0,
            redaction_categories: vec![],
            created_at: "2026-11-15T10:00:00Z".into(),
            detail: None,
            observed_at: None,
        },
    };
    model.update(visual, |model, cx| {
        model.note_agent_dock_run(&envelope, cx);
        model.agents.update(cx, |agents, _| {
            agents.record_event(envelope);
        });
        cx.notify();
    });
    visual.run_until_parked();
}

/// ADR-040 M3: the dock starts by the window's height, keeps the strip's
/// height until a run appears and leaves the view above 240 px. Feature 25,
/// M1: it shows on Agents too.
#[gpui::test]
fn agent_dock_starts_by_window_height_and_shows_on_agents(cx: &mut TestAppContext) {
    use ikore_core::terminals::{ExecutionOrigin, TerminalRunStatus};

    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(960.0), px(640.0)));
    select(&model, visual, AppView::Workspace);
    let strip = visual
        .debug_bounds("agent-dock-strip")
        .expect("a short window starts with the strip");
    assert_eq!(strip.size.height, px(32.0));
    assert_eq!(model.read_with(visual, |m, _| m.agent_dock.mode), None);

    // A tall window starts expanded, which keeps the strip's height until a run.
    visual.simulate_resize(size(px(1280.0), px(820.0)));
    visual.run_until_parked();
    assert_eq!(
        visual.debug_bounds("agent-dock").unwrap().size.height,
        px(32.0)
    );
    let run = dock_run(
        AgentRole::TaskSpecifier,
        "dock-run",
        ExecutionOrigin::Manual,
        TerminalRunStatus::Running,
    );
    deliver_run_event(&model, visual, &run, 1);
    let dock = visual.debug_bounds("agent-dock").unwrap();
    assert_eq!(dock.size.height, px(crate::agent_dock::DEFAULT_HEIGHT));
    assert!(visual.debug_bounds("agent-dock-console").is_some());
    assert!(visual.debug_bounds("agent-dock-now").is_some());
    assert!(
        visual
            .debug_bounds("agent-dock-tab:role:task_specifier")
            .is_some()
    );
    let frame = visual.debug_bounds("shell-frame").unwrap();
    assert!(frame.size.height >= px(240.0), "{frame:?}");
    assert!(frame.bottom() <= dock.top(), "{frame:?} {dock:?}");

    // Agents shows it as well, over the canvas, and its state stays.
    select(&model, visual, AppView::Agents);
    assert!(model.read_with(visual, |m, _| m.agent_dock_inset()) > px(0.0));
    let dock = visual.debug_bounds("agent-dock").unwrap();
    let canvas = visual.debug_bounds("agents-canvas").unwrap();
    assert!(canvas.bottom() <= dock.top(), "{canvas:?} {dock:?}");
    select(&model, visual, AppView::Workspace);
    assert!(model.read_with(visual, |m, _| m.agent_dock_inset()) > px(0.0));
}

/// ADR-040 M3: `Ctrl+J` hides and brings the dock back while an input keeps
/// the focus; the strip's own controls and the status bar item do the same.
#[gpui::test]
fn ctrl_j_and_the_status_item_toggle_the_dock_without_moving_the_focus(cx: &mut TestAppContext) {
    use crate::agent_dock::DockMode;

    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.update(|_, cx| crate::shell::register_navigation_keybindings(cx));
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    select(&model, visual, AppView::Workspace);
    click(visual, "work-tab:search");
    let input = model.read_with(visual, |m, _| m.work_panel.search_input().unwrap());
    let focused = |visual: &mut VisualTestContext| {
        visual.update(|window, cx| {
            gpui::Focusable::focus_handle(input.read(cx), cx).is_focused(window)
        })
    };
    assert!(focused(visual));
    visual.simulate_keystrokes("ctrl-j");
    visual.run_until_parked();
    assert_eq!(
        model.read_with(visual, |m, _| m.agent_dock.mode),
        Some(DockMode::Hidden)
    );
    assert!(focused(visual), "hiding keeps the focus");
    visual.simulate_keystrokes("ctrl-j");
    visual.run_until_parked();
    assert_eq!(
        model.read_with(visual, |m, _| m.agent_dock.mode),
        Some(DockMode::Expanded)
    );
    assert!(focused(visual), "opening never moves the focus");

    // The status bar item says nothing runs, and toggles the same way.
    click(visual, "status-agent-console");
    assert_eq!(
        model.read_with(visual, |m, _| m.agent_dock.mode),
        Some(DockMode::Hidden)
    );
    click(visual, "status-agent-console");
    assert_eq!(
        model.read_with(visual, |m, _| m.agent_dock.mode),
        Some(DockMode::Expanded)
    );
    // The strip folds and hides from its own buttons.
    model.update(visual, |m, cx| m.set_agent_dock_mode(DockMode::Strip, cx));
    visual.run_until_parked();
    click(visual, "agent-dock-expand");
    assert_eq!(
        model.read_with(visual, |m, _| m.agent_dock.mode),
        Some(DockMode::Expanded)
    );
}

/// ADR-040 M3 (9.6): the Companion keeps clear of the dock, folded or
/// expanded, in the three sizes the specification names.
#[gpui::test]
fn the_companion_stays_above_the_agent_dock(cx: &mut TestAppContext) {
    use crate::agent_dock::DockMode;
    use ikore_core::terminals::{ExecutionOrigin, TerminalRunStatus};

    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    select(&model, visual, AppView::Workspace);
    let run = dock_run(
        AgentRole::BackendAgent,
        "companion-run",
        ExecutionOrigin::Pipeline,
        TerminalRunStatus::Running,
    );
    deliver_run_event(&model, visual, &run, 1);
    for (width, height) in [(960.0, 640.0), (1280.0, 820.0), (2560.0, 1440.0)] {
        visual.simulate_resize(size(px(width), px(height)));
        for mode in [DockMode::Strip, DockMode::Expanded] {
            model.update(visual, |m, cx| m.set_agent_dock_mode(mode, cx));
            visual.run_until_parked();
            let dock = visual.debug_bounds("agent-dock").unwrap();
            let companion = visual.debug_bounds("companion-dock").unwrap();
            // The sash, or the strip's gap, sits 6 px above the dock's box.
            assert!(
                companion.bottom() <= dock.top() - px(6.0),
                "{width}x{height} {mode:?}: companion {companion:?} over dock {dock:?}"
            );
            let frame = visual.debug_bounds("shell-frame").unwrap();
            assert!(frame.size.height >= px(240.0), "{width}x{height} {mode:?}");
        }
    }
}

/// Visual QA (2026-11-16): on the narrowest window, seven running agents
/// give six tabs and the overflow control. The tabs give way instead of
/// pushing the last one and the control under the header's buttons.
#[gpui::test]
fn agent_dock_tabs_give_way_on_a_narrow_window(cx: &mut TestAppContext) {
    use crate::agent_dock::{DockMode, VISIBLE_TABS};
    use ikore_core::terminals::{ExecutionOrigin, TerminalRunStatus};

    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(960.0), px(640.0)));
    select(&model, visual, AppView::Workspace);
    let roles = [
        AgentRole::IntakeRouter,
        AgentRole::TaskSpecifier,
        AgentRole::ContractAgent,
        AgentRole::BackendAgent,
        AgentRole::TestAgent,
        AgentRole::Integrator,
        AgentRole::FeatureAgent,
    ];
    for (index, role) in roles.into_iter().enumerate() {
        let run = dock_run(
            role,
            &format!("narrow-run-{index}"),
            ExecutionOrigin::Pipeline,
            TerminalRunStatus::Running,
        );
        deliver_run_event(&model, visual, &run, 1);
    }
    model.update(visual, |m, cx| {
        m.set_agent_dock_mode(DockMode::Expanded, cx)
    });
    visual.run_until_parked();

    let more = visual
        .debug_bounds("agent-dock-more")
        .expect("the seventh agent goes to More");
    let follow = visual.debug_bounds("agent-dock-follow").unwrap();
    assert!(more.right() <= follow.left(), "{more:?} under {follow:?}");
    // Support comes after the stages, so the six stages keep the tabs.
    let closes: Vec<_> = roles
        .iter()
        .filter_map(|role| {
            visual.debug_bounds(leak(format!("agent-dock-tab-close:role:{}", role.as_str())))
        })
        .collect();
    assert_eq!(closes.len(), VISIBLE_TABS);
    for close in closes {
        assert!(close.right() <= more.left(), "{close:?} under {more:?}");
    }
}

/// ADR-040 M3/M4: tabs in the pipeline's order; a run the dock does not
/// show gets a dot; a held-back switch shows the chip, which switches; a
/// closed tab comes back with the agent's next run; Esc folds the dock.
#[gpui::test]
fn agent_dock_tabs_follow_and_hold_back_a_switch(cx: &mut TestAppContext) {
    use crate::agent_console::follow::FollowInput;
    use crate::agent_dock::DockMode;
    use crate::agents::AgentIdentifier;
    use ikore_core::terminals::{ExecutionOrigin, TerminalRunStatus};

    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(1280.0), px(820.0)));
    select(&model, visual, AppView::Workspace);
    let code = dock_run(
        AgentRole::BackendAgent,
        "code-run",
        ExecutionOrigin::Pipeline,
        TerminalRunStatus::Running,
    );
    let spec = dock_run(
        AgentRole::TaskSpecifier,
        "spec-run",
        ExecutionOrigin::Manual,
        TerminalRunStatus::Running,
    );
    deliver_run_event(&model, visual, &code, 1);
    deliver_run_event(&model, visual, &spec, 1);
    let backend = AgentIdentifier::Canonical(AgentRole::BackendAgent);
    let task_specifier = AgentIdentifier::Canonical(AgentRole::TaskSpecifier);
    // Spec comes before Code, whatever started first.
    let spec_tab = visual
        .debug_bounds("agent-dock-tab:role:task_specifier")
        .unwrap();
    let code_tab = visual
        .debug_bounds("agent-dock-tab:role:backend_agent")
        .unwrap();
    assert!(spec_tab.right() <= code_tab.left());
    // The developer reads Code: a manual run never takes the dock from it, and
    // its tab gets a dot.
    click(visual, "agent-dock-tab:role:backend_agent");
    assert_eq!(
        model.read_with(visual, |m, _| m.agent_dock.shown()),
        Some(backend.clone())
    );
    model.update(visual, |m, cx| m.resume_agent_dock_following(cx));
    visual.run_until_parked();
    assert!(
        visual
            .debug_bounds("agent-dock-tab-new:role:task_specifier")
            .is_some()
    );

    // A switch waits while the developer touches the dock: the chip offers it.
    model.update(visual, |m, cx| {
        let now = std::time::Instant::now();
        let follow = &mut m.agent_dock.follow;
        follow.current = Some(task_specifier.clone());
        follow.pending = Some((backend.clone(), now - std::time::Duration::from_secs(3)));
        follow.apply(FollowInput::Interaction { at: now });
        cx.notify();
    });
    visual.run_until_parked();
    click(visual, "agent-dock-switch-chip");
    assert_eq!(
        model.read_with(visual, |m, _| m.agent_dock.shown()),
        Some(backend.clone())
    );

    // A closed tab stays closed for its run, and comes back with the next.
    click(visual, "agent-dock-tab-close:role:task_specifier");
    assert!(!model.read_with(visual, |m, _| m.agent_dock.tabs().contains(&task_specifier)));
    deliver_run_event(&model, visual, &spec, 2);
    assert!(!model.read_with(visual, |m, _| m.agent_dock.tabs().contains(&task_specifier)));
    let next = dock_run(
        AgentRole::TaskSpecifier,
        "spec-run-2",
        ExecutionOrigin::Manual,
        TerminalRunStatus::Running,
    );
    deliver_run_event(&model, visual, &next, 1);
    assert!(model.read_with(visual, |m, _| m.agent_dock.tabs().contains(&task_specifier)));

    // Esc in the dock folds it to the strip.
    visual.update(|window, cx| {
        let focus = model
            .read(cx)
            .agents
            .read(cx)
            .consoles
            .focus(crate::agent_dock::DOCK_SURFACE, cx);
        window.focus(&focus);
    });
    visual.run_until_parked();
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    assert_eq!(
        model.read_with(visual, |m, _| m.agent_dock.mode),
        Some(DockMode::Strip)
    );
}

/// ADR-040 M3/M4: the sash keeps the dock between its limits, and a double
/// click goes between the default and the tallest height; the attention
/// count reaches the status bar, and nothing opens the dock on its own.
#[gpui::test]
fn agent_dock_sash_limits_and_attention(cx: &mut TestAppContext) {
    use crate::agent_dock::{AgentDock, DEFAULT_HEIGHT, DockMode, MIN_HEIGHT};
    use ikore_core::terminals::{ExecutionOrigin, TerminalRunStatus};

    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(1280.0), px(820.0)));
    select(&model, visual, AppView::Workspace);
    let body = px(820.0) - crate::sizing::HEADER_HEIGHT - crate::sizing::STATUS_BAR_HEIGHT;
    let max = AgentDock::max_height(body);
    model.update(visual, |m, cx| {
        m.agent_dock.drag = Some((px(500.0), DEFAULT_HEIGHT));
        m.drag_agent_dock(px(-2000.0), body, cx);
        assert_eq!(m.agent_dock.height, Some(max));
        m.drag_agent_dock(px(2000.0), body, cx);
        assert_eq!(m.agent_dock.height, Some(MIN_HEIGHT));
        m.end_agent_dock_drag(cx);
        assert!(m.agent_dock.drag.is_none());
        m.toggle_agent_dock_height(body, cx);
        assert_eq!(m.agent_dock.height, Some(max));
        m.toggle_agent_dock_height(body, cx);
        assert_eq!(m.agent_dock.height, Some(DEFAULT_HEIGHT));
    });

    // A failed run counts on the status bar and never opens a hidden dock.
    model.update(visual, |m, cx| m.set_agent_dock_mode(DockMode::Hidden, cx));
    let failed = dock_run(
        AgentRole::BackendAgent,
        "failed-run",
        ExecutionOrigin::Pipeline,
        TerminalRunStatus::Failed,
    );
    deliver_run_event(&model, visual, &failed, 1);
    let (_, label, attention) = model.read_with(visual, |m, cx| m.agent_console_status(cx));
    assert_eq!(label, "No agent running");
    assert_eq!(attention, 1);
    assert_eq!(
        model.read_with(visual, |m, _| m.agent_dock.mode),
        Some(DockMode::Hidden)
    );
}

/// ADR-040 M5: a pulse reaches the status bar item and the dock's "now"
/// line, its actions show in the console while the run goes on, and only a
/// run a surface shows repaints the window.
#[gpui::test]
fn an_agent_pulse_reaches_the_status_bar_and_the_console(cx: &mut TestAppContext) {
    use ikore_core::terminals::{
        ActivityCounters, ActivityPhase, AgentActivityPulseV1, ExecutionOrigin, PulseActionDelta,
        PulseTarget, TerminalRunStatus, ToolKind,
    };

    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(1280.0), px(820.0)));
    select(&model, visual, AppView::Workspace);
    let run = dock_run(
        AgentRole::TaskSpecifier,
        "pulse-run",
        ExecutionOrigin::Manual,
        TerminalRunStatus::Running,
    );
    deliver_run_event(&model, visual, &run, 1);
    let pulse = AgentActivityPulseV1 {
        protocol_version: 1,
        project_id: "layout-project".into(),
        run_id: "pulse-run".into(),
        agent_role: Some(AgentRole::TaskSpecifier),
        slot_id: None,
        pulse_sequence: 1,
        emitted_at: "2026-11-16T10:00:00.000Z".into(),
        phase: ActivityPhase::RunningTool,
        phase_since_ms: 0,
        last_activity_ms_ago: 0,
        actions: vec![PulseActionDelta::Started {
            ordinal: 1,
            kind: ToolKind::Read,
            name: "Read".into(),
            target: Some(PulseTarget::WorkspacePath("src/main.rs".into())),
        }],
        actions_dropped: 0,
        counters: ActivityCounters {
            tool_calls: 1,
            files_read: 1,
            elapsed_ms: 5_000,
            ..ActivityCounters::default()
        },
    };
    model.update(visual, |m, cx| m.handle_activity_pulse(pulse.clone(), cx));
    visual.run_until_parked();
    let (_, label, _) = model.read_with(visual, |m, cx| m.agent_console_status(cx));
    assert!(
        label.starts_with("Spec · TaskSpecifier · Running tool · Read src/main.rs · 1 read"),
        "{label}"
    );
    assert!(model.read_with(visual, |m, cx| m.shows_run("pulse-run", cx)));
    assert!(!model.read_with(visual, |m, cx| m.shows_run("another-run", cx)));
    // A repeated pulse changes nothing.
    assert!(!model.update(visual, |m, cx| {
        m.agents.update(cx, |agents, _| {
            agents.live.record(&pulse, std::time::Instant::now())
        })
    }));

    // On the Agents page the dock's console shows the provisional action
    // after the persisted line, and the synthesis says what the run does now
    // (feature 25, M1).
    select(&model, visual, AppView::Agents);
    click(visual, "agents-node:role:task_specifier");
    assert!(visual.debug_bounds("agent-console-block:1").is_some());
    assert!(
        visual
            .debug_bounds("agents-synthesis-now:task_specifier")
            .is_some()
    );
}

/// ADR-040 vector 10: an API Mode route is followed like a run, and the
/// dock shows the agent's API lines.
#[gpui::test]
fn an_api_route_is_followed_like_a_run(cx: &mut TestAppContext) {
    use ikore_core::llm::{
        AgentExecutionContext, AgentExecutionTarget, LlmEventEnvelope, LlmStreamEvent,
    };

    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(1280.0), px(820.0)));
    select(&model, visual, AppView::Workspace);
    let envelope = |event| LlmEventEnvelope {
        protocol_version: 2,
        execution_id: "execution-1".into(),
        generation_id: "generation-1".into(),
        sequence: 1,
        context: Some(AgentExecutionContext {
            project_id: "layout-project".into(),
            pulsation_id: Some("pulse-1".into()),
            job_id: Some("job-1".into()),
            target: AgentExecutionTarget::canonical(AgentRole::BackendAgent),
            stage: "backend".into(),
        }),
        event,
    };
    model.update(visual, |m, cx| {
        m.note_agent_dock_route(
            &envelope(LlmStreamEvent::RouteStarted {
                route_id: "route-1".into(),
            }),
            cx,
        );
    });
    visual.run_until_parked();
    let backend = crate::agents::AgentIdentifier::Canonical(AgentRole::BackendAgent);
    assert!(model.read_with(visual, |m, _| m.agent_dock.tabs().contains(&backend)));
    assert!(model.read_with(visual, |m, _| m.agent_dock.follow.needs_tick()));
    // Another project's route never reaches the dock.
    model.update(visual, |m, cx| {
        let mut other = envelope(LlmStreamEvent::RouteStarted {
            route_id: "route-2".into(),
        });
        other.context.as_mut().unwrap().project_id = "another-project".into();
        other.context.as_mut().unwrap().target =
            AgentExecutionTarget::canonical(AgentRole::TaskSpecifier);
        m.note_agent_dock_route(&other, cx);
    });
    assert_eq!(model.read_with(visual, |m, _| m.agent_dock.tabs().len()), 1);
}

/// Feature 25, M1 (layout and R3): the stages on their row in the pipeline's
/// order; the support agents, then the free-form ones, below; archived
/// free-form agents stay off the canvas, counted in the box's footer, which
/// lists them with `Restore` and `Delete`. A node's state by its run is
/// `canvas_node`'s unit test.
#[gpui::test]
fn agents_canvas_orders_the_agents_and_counts_the_archived(cx: &mut TestAppContext) {
    use ikore_core::terminals::{ExecutionOrigin, TerminalRunStatus};

    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(1280.0), px(820.0)));
    select(&model, visual, AppView::Agents);
    model.update(visual, |model, cx| {
        model.agents.update(cx, |agents, _| {
            let snapshot = agents.snapshot.as_mut().unwrap();
            // A support agent ahead of the stages in the snapshot still sits after them.
            let mut feature = snapshot.agents[0].clone();
            feature.config.agent_role = AgentRole::FeatureAgent;
            snapshot.agents.insert(0, feature);
            snapshot.agents[1].recent_runs = vec![dock_run(
                AgentRole::TaskSpecifier,
                "failed-run",
                ExecutionOrigin::Manual,
                TerminalRunStatus::Failed,
            )];
            let mut archived = snapshot.free_form_slots[0].clone();
            archived.slot.id = "slot-archived".into();
            archived.slot.label = "Old slot".into();
            archived.slot.archived = true;
            snapshot.archived_free_form_slots.push(archived);
        });
        cx.notify();
    });
    visual.run_until_parked();
    let spec = visual
        .debug_bounds("agents-node:role:task_specifier")
        .unwrap();
    let code = visual
        .debug_bounds("agents-node:role:backend_agent")
        .unwrap();
    let feature = visual
        .debug_bounds("agents-node:role:feature_agent")
        .unwrap();
    let free = visual.debug_bounds("agents-node:slot:slot-free").unwrap();
    assert!(spec.right() < code.left() && spec.top() == code.top());
    assert!(feature.top() > code.bottom());
    assert!(feature.right() <= free.left() && feature.top() == free.top());
    model.read_with(visual, |m, cx| {
        let agents = m.agents.read(cx);
        let snapshot = agents.snapshot.as_ref().unwrap();
        assert_eq!(snapshot.archived_free_form_slots.len(), 1);
    });
    // The archived agent is off the canvas; the footer counts it.
    assert!(
        visual
            .debug_bounds("agents-node:slot:slot-archived")
            .is_none()
    );
    let footer = visual.debug_bounds("agents-box-archived").unwrap();
    let frame = visual.debug_bounds("agents-box:default").unwrap();
    assert!(footer.top() >= free.bottom() && footer.bottom() <= frame.bottom());
    click(visual, "agents-box-archived");
    assert!(model.read_with(visual, |m, cx| m.agents.read(cx).show_archived));
    assert!(visual.debug_bounds("agents-drawer:archived").is_some());
    assert!(
        visual
            .debug_bounds("free-form-restore:slot-archived")
            .is_some()
    );
    assert!(
        visual
            .debug_bounds("free-form-archived-delete:slot-archived")
            .is_some()
    );
    click(visual, "agents-drawer-close");
    assert!(!model.read_with(visual, |m, cx| m.agents.read(cx).show_archived));
}

/// Feature 25, M1 (R12, R13): the synthesis's header keeps the agent, its
/// model and Start whatever the drawer; `Configure` holds the transport and
/// the backend; a History row pins the dock on its run, with the chip that
/// brings the latest back.
#[gpui::test]
fn agent_drawers_keep_the_header_and_history_pins_the_dock_on_a_run(cx: &mut TestAppContext) {
    use crate::agents::AgentDetailTab;
    use ikore_core::terminals::{ExecutionOrigin, TerminalRunStatus};

    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    // Tall enough for the History drawer to show its rows above the dock.
    visual.simulate_resize(size(px(1440.0), px(1400.0)));
    select(&model, visual, AppView::Agents);
    model.update(visual, |model, cx| {
        model.agents.update(cx, |agents, _| {
            let snapshot = agents.snapshot.as_mut().unwrap();
            snapshot.agents[0].recent_runs = vec![
                dock_run(
                    AgentRole::TaskSpecifier,
                    "run-new",
                    ExecutionOrigin::Manual,
                    TerminalRunStatus::Completed,
                ),
                dock_run(
                    AgentRole::TaskSpecifier,
                    "run-old",
                    ExecutionOrigin::Pipeline,
                    TerminalRunStatus::Failed,
                ),
            ];
        });
        cx.notify();
    });
    visual.run_until_parked();
    click(visual, "agents-node:role:task_specifier");
    let tab = |model: &Entity<AppModel>, visual: &mut VisualTestContext| {
        model.read_with(visual, |m, cx| m.agents.read(cx).detail_tab)
    };
    // The summary first: no console on the page, the drawers under the header.
    assert_eq!(tab(&model, visual), AgentDetailTab::Summary);
    let header = visual.debug_bounds("agent-header:task_specifier").unwrap();
    let actions = visual
        .debug_bounds("agents-synthesis-actions:task_specifier")
        .unwrap();
    assert!(header.bottom() <= actions.top());
    assert!(visual.debug_bounds("agent-start:task_specifier").is_some());
    assert!(
        visual
            .debug_bounds("agent-console-filter:task_specifier")
            .is_none()
    );

    click(visual, "agents-synthesis-configure");
    assert_eq!(tab(&model, visual), AgentDetailTab::Configuration);
    let transport = visual.debug_bounds("agent-mode:task_specifier").unwrap();
    let backend = visual.debug_bounds("agent-backend:task_specifier").unwrap();
    assert!(header.bottom() <= transport.top() && header.bottom() <= backend.top());
    assert_eq!(
        visual.debug_bounds("agent-header:task_specifier").unwrap(),
        header
    );

    click(visual, "agents-synthesis-history");
    assert_eq!(tab(&model, visual), AgentDetailTab::History);
    let table = visual.debug_bounds("agent-history").unwrap();
    assert!(header.bottom() <= table.top());
    // Newest first; the older run opens in the dock, which stays pinned on it.
    let newer = visual.debug_bounds("agent-history-row:0").unwrap();
    let older = visual.debug_bounds("agent-history-row:1").unwrap();
    assert!(newer.top() < older.top());
    // The table scrolls across in the drawer: the row's start is in view.
    visual.simulate_click(
        point(older.left() + px(24.0), older.center().y),
        Modifiers::default(),
    );
    visual.run_until_parked();
    assert_eq!(tab(&model, visual), AgentDetailTab::History);
    let spec = crate::agents::AgentIdentifier::Canonical(AgentRole::TaskSpecifier);
    model.read_with(visual, |m, cx| {
        assert_eq!(
            m.agents
                .read(cx)
                .selected_run_by_role
                .get(&AgentRole::TaskSpecifier)
                .cloned(),
            Some("run-old".to_owned())
        );
        assert_eq!(m.agent_dock.chosen_run_of(&spec), Some("run-old"));
        assert_eq!(m.agent_dock.shown(), Some(spec.clone()));
    });
    assert!(visual.debug_bounds("agent-dock-run-chip").is_some());
    click(visual, "agent-dock-run-chip");
    assert_eq!(
        model.read_with(visual, |m, _| m
            .agent_dock
            .chosen_run_of(&spec)
            .map(str::to_owned)),
        None
    );
    // `Esc` closes the drawer, then the synthesis.
    model.update(visual, |m, cx| assert!(m.dismiss_agents_synthesis(cx)));
    assert_eq!(tab(&model, visual), AgentDetailTab::Summary);
    model.update(visual, |m, cx| assert!(m.dismiss_agents_synthesis(cx)));
    assert_eq!(
        model.read_with(visual, |m, cx| m.agents.read(cx).selected_agent.clone()),
        None
    );
}

/// ADR-040 M7, then feature 25 M1: `Ctrl+Shift+]` and `Ctrl+Shift+[`, the
/// other views' section keys, walk the synthesis's drawers both ways round;
/// the canvas takes the focus with the click on its node.
#[gpui::test]
fn agent_drawers_follow_the_section_keys(cx: &mut TestAppContext) {
    use crate::agents::AgentDetailTab;

    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.update(|_, cx| crate::shell::register_navigation_keybindings(cx));
    visual.simulate_resize(size(px(1280.0), px(820.0)));
    select(&model, visual, AppView::Agents);
    click(visual, "agents-node:role:task_specifier");
    assert!(
        visual.update(|window, cx| window.focused(cx)).is_some(),
        "the canvas is focusable"
    );
    let tab = |model: &Entity<AppModel>, visual: &mut VisualTestContext| {
        model.read_with(visual, |m, cx| m.agents.read(cx).detail_tab)
    };
    assert_eq!(tab(&model, visual), AgentDetailTab::Summary);
    for (keys, expected) in [
        ("ctrl-shift-]", AgentDetailTab::Configuration),
        ("ctrl-shift-]", AgentDetailTab::History),
        ("ctrl-shift-]", AgentDetailTab::Run),
        ("ctrl-shift-]", AgentDetailTab::Summary),
        ("ctrl-shift-[", AgentDetailTab::Run),
        ("ctrl-shift-[", AgentDetailTab::History),
        ("ctrl-shift-[", AgentDetailTab::Configuration),
    ] {
        visual.simulate_keystrokes(keys);
        visual.run_until_parked();
        assert_eq!(tab(&model, visual), expected, "after {keys}");
    }
    assert!(
        visual
            .debug_bounds("agent-backend:task_specifier")
            .is_some()
    );
}

/// Feature 25, M1: arrows walk the nodes, `Enter` opens the synthesis of the
/// node they reached, and `Esc` closes it.
#[gpui::test]
fn agents_canvas_answers_the_arrows_enter_and_escape(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(1280.0), px(820.0)));
    select(&model, visual, AppView::Agents);
    click(visual, "agents-node:role:task_specifier");
    let selected = |model: &Entity<AppModel>, visual: &mut VisualTestContext| {
        model.read_with(visual, |m, cx| m.agents.read(cx).selected_agent.clone())
    };
    visual.simulate_keystrokes("right");
    visual.run_until_parked();
    // The arrows move the keyboard's mark; the synthesis stays until Enter.
    assert_eq!(
        selected(&model, visual),
        Some(crate::agents::AgentIdentifier::Canonical(
            AgentRole::TaskSpecifier
        ))
    );
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();
    assert_eq!(
        selected(&model, visual),
        Some(crate::agents::AgentIdentifier::Canonical(
            AgentRole::BackendAgent
        ))
    );
    visual.simulate_keystrokes("down enter");
    visual.run_until_parked();
    assert_eq!(
        selected(&model, visual),
        Some(crate::agents::AgentIdentifier::FreeForm("slot-free".into()))
    );
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    assert_eq!(selected(&model, visual), None);
}

/// ADR-040 M7, then feature 25 M1 (criterion 3, R12): every action of the old
/// page is still reachable from the synthesis and its drawers, with the same
/// confirmations: Start and Resume in the header; the runs' history, copy,
/// export and clear in History; the prompt in `Run…`; transport, backend,
/// profile and model in Configure; and the free-form agent's own controls,
/// with `Move earlier` and `Move later`, archive and delete.
#[gpui::test]
fn agents_actions_stay_reachable_with_their_confirmations(cx: &mut TestAppContext) {
    use crate::agents::AgentDetailTab;
    use ikore_core::terminals::{ExecutionOrigin, TerminalRunStatus};

    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    // Tall enough for the History drawer to show its rows above the dock.
    visual.simulate_resize(size(px(1440.0), px(1400.0)));
    select(&model, visual, AppView::Agents);
    model.update(visual, |model, cx| {
        model.agents.update(cx, |agents, _| {
            let snapshot = agents.snapshot.as_mut().unwrap();
            snapshot.agents[0].recent_runs = vec![dock_run(
                AgentRole::TaskSpecifier,
                "done-run",
                ExecutionOrigin::Manual,
                TerminalRunStatus::Completed,
            )];
        });
        cx.notify();
    });
    visual.run_until_parked();
    let present = |visual: &mut VisualTestContext, selectors: &[&'static str]| {
        for selector in selectors {
            assert!(
                visual.debug_bounds(selector).is_some(),
                "missing {selector}"
            );
        }
    };
    click(visual, "agents-node:role:task_specifier");
    // The header: Start and Resume; the drawers under it.
    present(
        visual,
        &[
            "agent-start:task_specifier",
            "agent-resume:task_specifier",
            "agents-synthesis-configure",
            "agents-synthesis-history",
            "agents-synthesis-run",
        ],
    );
    // `Run…`: the prompt for a manual run.
    open_agent_drawer(&model, visual, AgentDetailTab::Run);
    present(
        visual,
        &["agents-drawer:run", "info:agent-stdin:task_specifier"],
    );
    // History: the runs, their navigation, copy, export and clear.
    open_agent_drawer(&model, visual, AgentDetailTab::History);
    present(
        visual,
        &[
            "agent-history",
            "agent-history-older:task_specifier",
            "agent-history-newer:task_specifier",
            "agent-history-clear:task_specifier",
            "agent-copy:task_specifier",
            "agent-export:task_specifier",
        ],
    );
    // Clearing still asks twice.
    click(visual, "agent-history-clear:task_specifier");
    model.read_with(visual, |m, cx| {
        let agents = m.agents.read(cx);
        assert_eq!(agents.pending_history_clear, Some(AgentRole::TaskSpecifier));
        assert_eq!(
            agents.snapshot.as_ref().unwrap().agents[0]
                .recent_runs
                .len(),
            1
        );
    });
    // Configure: transport, backend, profile and model.
    open_agent_drawer(&model, visual, crate::agents::AgentDetailTab::Configuration);
    present(
        visual,
        &[
            "agent-mode:task_specifier",
            "agent-backend:task_specifier",
            "agent-thinking:task_specifier",
            "agent-model:task_specifier",
        ],
    );
    // The free-form agent: New session in the header, its controls in Configure.
    click(visual, "agents-node:slot:slot-free");
    if model.read_with(visual, |m, cx| m.agents.read(cx).detail_tab)
        != AgentDetailTab::Configuration
    {
        click(visual, "agents-synthesis-configure");
    }
    present(
        visual,
        &[
            "free-form-start:slot-free",
            "free-form-mode:slot-free",
            "free-form-backend:slot-free",
            "free-form-thinking:slot-free",
            "free-form-session:slot-free",
            "free-form-model:slot-free",
            "free-form-up:slot-free",
            "free-form-down:slot-free",
            "free-form-archive:slot-free",
            "free-form-delete:slot-free",
            "free-form-save:slot-free",
        ],
    );
    // Deleting still asks twice.
    click(visual, "free-form-delete:slot-free");
    model.read_with(visual, |m, cx| {
        let agents = m.agents.read(cx);
        assert_eq!(
            agents.pending_free_form_delete.as_deref(),
            Some("slot-free")
        );
        assert_eq!(agents.snapshot.as_ref().unwrap().free_form_slots.len(), 1);
    });
    // The ghost creates a free-form agent while there is room for one.
    assert!(
        visual
            .debug_bounds("agents-node-ghost:new-free-form")
            .is_some()
    );
    // A pipeline run's Stop asks first: it cancels the whole pulsation.
    model.update(visual, |model, cx| {
        model.agents.update(cx, |agents, _| {
            let snapshot = agents.snapshot.as_mut().unwrap();
            let mut run = dock_run(
                AgentRole::TaskSpecifier,
                "pipeline-run",
                ExecutionOrigin::Pipeline,
                TerminalRunStatus::Running,
            );
            run.pulsation_id = Some("pulse-a".into());
            snapshot.agents[0].recent_runs = vec![run];
        });
        cx.notify();
    });
    visual.run_until_parked();
    click(visual, "agents-node:role:task_specifier");
    click(visual, "agent-stop:task_specifier");
    assert_eq!(
        model.read_with(visual, |m, cx| m
            .agents
            .read(cx)
            .pending_pipeline_cancel
            .clone()),
        Some("pulse-a".to_owned())
    );
}

/// Feature 25, M1 (ADR-012, item 10): in the three sizes the canvas and the
/// synthesis sit side by side, the synthesis at least 300 px wide, with the
/// header's actions inside it and the drawers under the header.
#[gpui::test]
fn agents_page_fits_the_three_sizes(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    select(&model, visual, AppView::Agents);
    click(visual, "agents-node:role:task_specifier");
    for (width, height) in [(960.0, 640.0), (1280.0, 820.0), (2560.0, 1440.0)] {
        visual.simulate_resize(size(px(width), px(height)));
        visual.run_until_parked();
        // The split measures on the frame before: one more frame settles it.
        model.update(visual, |_, cx| cx.notify());
        visual.run_until_parked();
        let canvas = visual.debug_bounds("agents-canvas").unwrap();
        let detail = visual.debug_bounds("agents-cli-scroll").unwrap();
        let synthesis = visual.debug_bounds("agents-synthesis").unwrap();
        assert!(
            canvas.right() <= synthesis.left(),
            "{width}: {canvas:?} {synthesis:?}"
        );
        assert!(
            synthesis.size.width >= px(crate::agents_canvas::SYNTHESIS_MIN_WIDTH),
            "{width}: {synthesis:?}"
        );
        assert!(detail.right() <= px(width), "{width}: {detail:?}");
        let header = visual.debug_bounds("agent-header:task_specifier").unwrap();
        assert!(
            header.left() >= detail.left() && header.right() <= detail.right(),
            "{width}: {header:?} {detail:?}"
        );
        let start = visual.debug_bounds("agent-start:task_specifier").unwrap();
        assert!(
            start.left() >= header.left()
                && start.right() <= header.right()
                && start.top() >= header.top()
                && start.bottom() <= header.bottom(),
            "{width}: {start:?} {header:?}"
        );
        for id in ["configure", "history", "run"] {
            let button = visual
                .debug_bounds(leak(format!("agents-synthesis-{id}")))
                .unwrap();
            assert!(
                button.top() >= header.bottom() && button.right() <= detail.right(),
                "{width}: {id} {button:?} {header:?}"
            );
        }
    }
}

/// PI-Desktop PD6: a finished stage run's console ends with the files its job
/// published; a file opens in the work panel's Diff tab, which lists the
/// run's files and compares the chosen one.
#[gpui::test]
fn a_stage_run_lists_its_changes_and_opens_them_in_the_diff_tab(cx: &mut TestAppContext) {
    use crate::run_changes::{DiffLine, DiffView, FileChangeDiff, RunChanges, RunFileChange};
    use crate::work_panel::WorkPanelTab;
    use ikore_core::terminals::{ExecutionOrigin, TerminalRunStatus};
    use std::sync::Arc;

    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    select(&model, visual, AppView::Agents);
    let mut run = dock_run(
        AgentRole::TaskSpecifier,
        "stage-run",
        ExecutionOrigin::Pipeline,
        TerminalRunStatus::Completed,
    );
    run.job_id = Some("job-1".into());
    let files: Vec<RunFileChange> = (0..4)
        .map(|index| RunFileChange {
            artifact_id: format!("artifact-{index}"),
            relative_path: format!("src/file_{index}.rs"),
            added: index + 1,
            removed: index,
            note: (index == 3).then_some("Published before replaced texts were kept"),
            pending: false,
        })
        .collect();
    model.update(visual, |model, cx| {
        model.agents.update(cx, |agents, _| {
            agents.snapshot.as_mut().unwrap().agents[0].recent_runs = vec![run.clone()];
            agents
                .run_changes
                .insert("stage-run".into(), RunChanges::Loaded(Arc::new(files)));
        });
        cx.notify();
    });
    visual.run_until_parked();
    click(visual, "agents-node:role:task_specifier");
    // A dock tall enough for the run's last blocks and its changes.
    model.update(visual, |model, cx| {
        model.agent_dock.height = Some(480.0);
        cx.notify();
    });
    visual.run_until_parked();
    let console = visual.debug_bounds("agent-console:dock").unwrap();
    let card = visual.debug_bounds("agent-console-changes:dock").unwrap();
    assert!(
        card.top() >= console.top() && card.bottom() <= console.bottom(),
        "{card:?} {console:?}"
    );
    for index in 0..3 {
        assert!(
            visual
                .debug_bounds(leak(format!("agent-console-change:dock:{index}")))
                .is_some()
        );
    }
    assert!(
        visual
            .debug_bounds("agent-console-changes-more:dock")
            .is_some()
    );

    click(visual, "agent-console-change:dock:1");
    model.read_with(visual, |m, _| {
        assert_eq!(m.work_panel.tab, WorkPanelTab::Diff);
        let diff = m.work_panel.diff.as_ref().unwrap();
        assert_eq!(diff.selected, "artifact-1");
        assert_eq!(diff.files.len(), 4);
    });
    // Without a backend the change stays loading: give it its lines.
    model.update(visual, |m, cx| {
        m.work_panel.diff.as_mut().unwrap().view = DiffView::Loaded(Arc::new(FileChangeDiff {
            relative_path: "src/file_1.rs".into(),
            lines: vec![
                DiffLine::Skipped(4),
                DiffLine::Context("fn kept() {}".into()),
                DiffLine::Removed("let old = 1;".into()),
                DiffLine::Added("let new = 2;".into()),
            ],
            added: 1,
            removed: 1,
            note: None,
        }));
        cx.notify();
    });
    visual.run_until_parked();
    let panel = visual.debug_bounds("context-rail").unwrap();
    // Five tabs with Run (ADR-042, item 6): what the row cannot hold goes under it,
    // and the fold button stays in the panel, clear of every tab.
    let toggle = visual.debug_bounds("context-rail-toggle").unwrap();
    assert!(toggle.right() <= panel.right(), "{toggle:?} {panel:?}");
    for tab in [
        "work-tab:general",
        "work-tab:files",
        "work-tab:search",
        "work-tab:run",
        "work-tab:diff",
    ] {
        let bounds = visual.debug_bounds(tab).unwrap();
        assert!(
            bounds.left() >= panel.left()
                && bounds.right() <= panel.right()
                && !bounds.intersects(&toggle),
            "{tab} {bounds:?} {toggle:?} {panel:?}"
        );
    }
    let diff = visual.debug_bounds("work-diff").unwrap();
    assert!(
        diff.left() >= panel.left() && diff.right() <= panel.right(),
        "{diff:?} {panel:?}"
    );
    for index in 0..4 {
        assert!(
            visual
                .debug_bounds(leak(format!("work-diff-file:{index}")))
                .is_some()
        );
    }
    // Another file of the run.
    click(visual, "work-diff-file:2");
    model.read_with(visual, |m, _| {
        let diff = m.work_panel.diff.as_ref().unwrap();
        assert_eq!(diff.selected, "artifact-2");
        assert!(matches!(diff.view, DiffView::Loading));
    });
}

/// ADR-042, item 6: the work panel's Run tab shows the run the dock follows,
/// in the panel's column. It shows the dock's agent and resumes following
/// with it, keeps its own place in the console, holds a switch back while
/// the developer reads it with the dock hidden, and opens from the folded
/// strip; a run that starts never opens it (item 7).
#[gpui::test]
fn the_run_tab_shows_the_run_the_dock_follows(cx: &mut TestAppContext) {
    use crate::agent_console::follow::FollowInput;
    use crate::agent_dock::{DOCK_SURFACE, DockMode};
    use crate::run_tab::RUN_TAB_SURFACE;
    use crate::work_panel::WorkPanelTab;
    use ikore_core::terminals::{
        EventDetailV1, ExecutionOrigin, TerminalEvent, TerminalEventEnvelope, TerminalEventKind,
        TerminalRun, TerminalRunStatus, ToolKind,
    };
    use std::time::{Duration, Instant};

    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    select(&model, visual, AppView::Workspace);
    // Actions of `run`, as `handle_event` delivers them: rows enough to scroll.
    let deliver = |visual: &mut VisualTestContext,
                   run: &TerminalRun,
                   sequences: std::ops::RangeInclusive<u64>| {
        model.update(visual, |model, cx| {
            for sequence in sequences {
                let envelope = TerminalEventEnvelope {
                    protocol_version: 2,
                    project_id: "layout-project".into(),
                    agent_role: run.agent_role,
                    run_id: run.id.clone(),
                    sequence,
                    run: run.clone(),
                    event: TerminalEvent {
                        id: format!("{}-{sequence}", run.id),
                        run_id: run.id.clone(),
                        sequence,
                        kind: TerminalEventKind::ToolUse,
                        stream: None,
                        content: format!("Read · src/file_{sequence}.rs"),
                        redaction_count: 0,
                        redaction_categories: vec![],
                        created_at: "2026-11-15T10:00:00Z".into(),
                        detail: Some(EventDetailV1::ActionCall {
                            v: 1,
                            ordinal: u32::try_from(sequence).unwrap(),
                            kind: ToolKind::Read,
                            name: "Read".into(),
                        }),
                        observed_at: None,
                    },
                };
                model.note_agent_dock_run(&envelope, cx);
                model
                    .agents
                    .update(cx, |agents, _| agents.record_event(envelope));
            }
            cx.notify();
        });
        visual.run_until_parked();
    };
    let spec = AgentIdentifier::Canonical(AgentRole::TaskSpecifier);
    let backend = AgentIdentifier::Canonical(AgentRole::BackendAgent);

    // Before any run, the tab says what it will show; a run that starts leaves the
    // panel on the tab the developer chose.
    click(visual, "work-tab:run");
    assert!(visual.debug_bounds("work-run").is_some());
    assert!(visual.debug_bounds("run-tab-agent").is_none());
    click(visual, "work-tab:general");
    let spec_run = dock_run(
        AgentRole::TaskSpecifier,
        "spec-run",
        ExecutionOrigin::Manual,
        TerminalRunStatus::Running,
    );
    deliver(visual, &spec_run, 1..=60);
    assert_eq!(
        model.read_with(visual, |m, _| m.work_panel.tab),
        WorkPanelTab::General
    );

    // The dock's agent and its console, inside the panel: the console's controls go
    // under its summary, and the tabs keep clear of the fold button.
    click(visual, "work-tab:run");
    assert_eq!(
        model.read_with(visual, |m, _| m.agent_dock.shown()),
        Some(spec.clone())
    );
    let panel = visual.debug_bounds("context-rail").unwrap();
    let toggle = visual.debug_bounds("context-rail-toggle").unwrap();
    // Four tabs keep one row, clear of the fold button.
    let row = visual.debug_bounds("work-tab:general").unwrap().top();
    for tab in [
        "work-tab:general",
        "work-tab:files",
        "work-tab:search",
        "work-tab:run",
    ] {
        let bounds = visual.debug_bounds(tab).unwrap();
        assert!(
            bounds.left() >= panel.left() && bounds.right() <= toggle.left() && bounds.top() == row,
            "{tab} {bounds:?} leaves the row or overlaps the toggle {toggle:?}"
        );
    }
    for element in [
        "work-run",
        "run-tab-agent",
        "run-tab-follow",
        "run-tab-now",
        "agent-console:run-tab",
        "agent-console-filter:run-tab",
        "agent-console-follow:run-tab",
    ] {
        let bounds = visual
            .debug_bounds(element)
            .unwrap_or_else(|| panic!("missing {element}"));
        assert!(
            bounds.left() >= panel.left() && bounds.right() <= panel.right(),
            "{element} {bounds:?} leaves {panel:?}"
        );
    }
    assert_readable(visual, "run-tab-agent", 60.0, 24.0);

    // Its own place: reading earlier rows here leaves the dock's console at the end.
    assert_eq!(
        model.read_with(visual, |m, _| m.agent_dock.mode),
        None,
        "the dock is expanded by the window's height"
    );
    let console = visual.debug_bounds("agent-console:run-tab").unwrap();
    let scroll_up = |visual: &mut VisualTestContext| {
        visual.simulate_event(ScrollWheelEvent {
            position: console.center(),
            delta: ScrollDelta::Pixels(point(px(0.0), px(2000.0))),
            ..Default::default()
        });
        visual.run_until_parked();
    };
    scroll_up(visual);
    let following = |visual: &mut VisualTestContext, key: &'static str| {
        model.read_with(visual, |m, cx| m.agents.read(cx).consoles.following(key))
    };
    assert!(!following(visual, RUN_TAB_SURFACE));
    assert!(following(visual, DOCK_SURFACE));
    click(visual, "agent-console-follow:run-tab");
    assert!(following(visual, RUN_TAB_SURFACE));

    // Reading the tab holds a switch back with the dock hidden: the tab's chip offers it.
    model.update(visual, |m, cx| m.set_agent_dock_mode(DockMode::Hidden, cx));
    visual.run_until_parked();
    assert!(!model.read_with(visual, |m, _| m.agent_dock.follow.scrolled_up));
    scroll_up(visual);
    assert!(model.read_with(visual, |m, _| m.agent_dock.follow.scrolled_up));
    // Only the reading holds it: the clicks above set the touch guard.
    model.update(visual, |m, _| m.agent_dock.follow.guard_until = None);
    let code_run = dock_run(
        AgentRole::BackendAgent,
        "code-run",
        ExecutionOrigin::Pipeline,
        TerminalRunStatus::Running,
    );
    deliver(visual, &code_run, 1..=1);
    // A pipeline stage outranks the manual run once its wait is over.
    let switched = model.update(visual, |m, cx| {
        let follow = &mut m.agent_dock.follow;
        let (candidate, _) = follow.pending.clone().expect("Code waits to take the dock");
        follow.pending = Some((candidate, Instant::now() - Duration::from_secs(3)));
        let switched = follow.apply(FollowInput::Tick(Instant::now()));
        cx.notify();
        switched
    });
    visual.run_until_parked();
    assert!(!switched, "no switch while the developer reads the tab");
    assert_eq!(
        model.read_with(visual, |m, _| m.agent_dock.shown()),
        Some(spec.clone())
    );
    click(visual, "run-tab-switch-chip");
    assert_eq!(
        model.read_with(visual, |m, _| m.agent_dock.shown()),
        Some(backend.clone())
    );

    // Pinned with the dock, and back to following from the tab.
    model.update(visual, |m, cx| m.pin_agent_dock(spec.clone(), cx));
    visual.run_until_parked();
    click(visual, "run-tab-follow");
    assert_eq!(
        model.read_with(visual, |m, _| (
            m.agent_dock.follow.pinned(),
            m.agent_dock.shown()
        )),
        (false, Some(backend.clone()))
    );
    // Touching the tab holds a switch back for a moment, as touching the dock does.
    model.update(visual, |m, _| m.agent_dock.follow.guard_until = None);
    click(visual, "run-tab-now");
    assert!(model.read_with(visual, |m, _| m.agent_dock.follow.guard_until.is_some()));

    // Folded, the strip opens the panel on the tab.
    model.update(visual, |m, cx| {
        m.work_panel.tab = WorkPanelTab::General;
        m.set_context_rail_collapsed(true, cx);
    });
    visual.run_until_parked();
    click(visual, "rail-strip-run");
    assert_eq!(
        model.read_with(visual, |m, cx| (
            m.context_rail_collapsed(cx),
            m.work_panel.tab
        )),
        (false, WorkPanelTab::Run)
    );
}

/// ADR-043: Settings › MCP servers lists each server with its tools; only a
/// read-only tool can be allowed for the Companion; the form keeps variable
/// values out of sight; deleting asks twice.
#[gpui::test]
fn the_mcp_page_lists_servers_tools_and_the_form(cx: &mut TestAppContext) {
    use ikore_core::mcp::{McpServerView, McpToolView};

    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    model.update(visual, |model, cx| {
        model.settings.update(cx, |settings, _| {
            settings.mcp.project_id = Some("layout-project".into());
            settings.mcp.servers = vec![McpServerView {
                id: "server-1".into(),
                project_id: Some("layout-project".into()),
                name: "docs".into(),
                command: "npx".into(),
                args: vec!["-y".into(), "docs-server".into()],
                env_names: vec!["DOCS_TOKEN".into()],
                enabled: true,
                connected_at: Some("2026-11-16T10:00:00Z".into()),
                last_error: None,
                version: 1,
                tools: vec![
                    McpToolView {
                        name: "search".into(),
                        description: "Search the docs".into(),
                        read_only: true,
                        granted_to_companion: true,
                    },
                    McpToolView {
                        name: "delete".into(),
                        description: "Delete a page".into(),
                        read_only: false,
                        granted_to_companion: false,
                    },
                ],
                overridden: false,
            }];
        });
    });
    open_settings(&model, visual, Some(SettingsPage::Mcp));
    for selector in [
        "mcp-server:server-1",
        "mcp-tool:server-1:0",
        "mcp-tool:server-1:1",
        "mcp-grant:server-1:0",
        "mcp-grant:server-1:1",
        "mcp-connect:server-1",
        "mcp-add",
    ] {
        assert!(
            visual.debug_bounds(selector).is_some(),
            "missing {selector}"
        );
    }
    // Deleting asks twice.
    click(visual, "mcp-delete:server-1");
    assert_eq!(
        model.read_with(visual, |m, cx| m.settings.read(cx).mcp.delete_armed.clone()),
        Some("server-1".to_owned())
    );
    // The form: a variable's value goes in hidden and is listed by name only.
    click(visual, "mcp-add");
    assert!(visual.debug_bounds("mcp-form").is_some());
    model.update(visual, |model, cx| {
        let (variable, value) = {
            let form = model.settings.read(cx).mcp.form.as_ref().unwrap();
            (form.variable.clone(), form.value.clone())
        };
        variable.update(cx, |input, cx| input.set_value("API_TOKEN", cx));
        value.update(cx, |input, cx| input.set_value("secret-value", cx));
    });
    // The form sits below the fold of the modal: drive its buttons directly.
    assert!(visual.debug_bounds("mcp-add-variable").is_some());
    model.update(visual, |model, cx| model.add_mcp_variable(cx));
    visual.run_until_parked();
    // Keep command/argument sentinels in the actual native form, without saving
    // or launching them. They must not escape through the form or parent Debug.
    const C004_UI_COMMAND: &str = "C004UiCommandOpaque-7nR8vTq2";
    const C004_UI_ARGUMENT: &str = "sk-proj-syntheticc004uiabcdefghijklmnopqrstuv";
    model.update(visual, |model, cx| {
        let (command, args) = {
            let form = model.settings.read(cx).mcp.form.as_ref().unwrap();
            (form.command.clone(), form.args.clone())
        };
        command.update(cx, |input, cx| input.set_value(C004_UI_COMMAND, cx));
        args.update(cx, |input, cx| {
            input.set_value(format!("--credential={C004_UI_ARGUMENT}"), cx)
        });
    });
    visual.run_until_parked();
    model.read_with(visual, |m, cx| {
        let form = m.settings.read(cx).mcp.form.as_ref().unwrap();
        assert_eq!(
            form.env,
            [("API_TOKEN".to_owned(), Some("secret-value".to_owned()))]
        );
        assert!(form.value.read(cx).value().is_empty());
        // REV-0002 C004: the hidden value must not reappear through Debug,
        // including the parent Settings state that contains this form.
        assert_eq!(form.command.read(cx).value(), C004_UI_COMMAND);
        assert!(form.args.read(cx).value().contains(C004_UI_ARGUMENT));
        let form_debug = format!("{form:?}");
        assert!(form_debug.contains("API_TOKEN"));
        let settings_debug = format!("{:?}", m.settings.read(cx).mcp);
        for debug in [form_debug, settings_debug] {
            for secret in ["secret-value", C004_UI_COMMAND, C004_UI_ARGUMENT] {
                assert!(
                    !debug.contains(secret),
                    "a fixture value reached native MCP Debug"
                );
            }
        }
    });
    assert!(visual.debug_bounds("mcp-cancel").is_some());
    model.update(visual, |model, cx| model.cancel_mcp_form(cx));
    assert!(model.read_with(visual, |m, cx| m.settings.read(cx).mcp.form.is_none()));
}

/// ADR-040 M3/M4 (11.2): Settings › General has the agent console's rows,
/// and the Shortcuts page names `Ctrl+J`.
#[gpui::test]
fn settings_agent_console_section(cx: &mut TestAppContext) {
    use crate::agent_dock::DockMode;

    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(1280.0), px(820.0)));
    model.update(visual, |m, cx| {
        m.open_settings(Some(SettingsPage::General), cx)
    });
    visual.run_until_parked();
    click(visual, "settings-agent-console");
    assert_eq!(
        model.read_with(visual, |m, _| m.agent_dock.mode),
        Some(DockMode::Hidden)
    );
    click(visual, "settings-agent-console-follow");
    assert!(!model.read_with(visual, |m, _| m.agent_dock.follow.enabled));
    for id in ["agent-console", "agent-console-follow"] {
        assert_eq!(
            crate::settings_search::entry_by_id(id).page,
            SettingsPage::General
        );
    }
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
    click(visual, "agents-node:role:task_specifier");

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

/// Feature 25, M1: the wheel pans the canvas; `Ctrl` with the wheel zooms it,
/// between 0.5× and 2×; `Fit` frames every box again.
#[gpui::test]
fn agents_canvas_pans_with_the_wheel_and_zooms_with_ctrl(cx: &mut TestAppContext) {
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
    click(visual, "agents-fit");

    let canvas = visual.debug_bounds("agents-canvas").unwrap();
    let fitted = visual.debug_bounds("agents-node:slot:slot-free").unwrap();
    visual.simulate_event(ScrollWheelEvent {
        position: point(canvas.left() + px(12.0), canvas.top() + px(24.0)),
        delta: ScrollDelta::Pixels(point(px(0.0), px(-120.0))),
        ..Default::default()
    });
    visual.run_until_parked();
    let panned = visual.debug_bounds("agents-node:slot:slot-free").unwrap();
    assert!(
        panned.top() < fitted.top(),
        "the canvas did not pan: {fitted:?} -> {panned:?}"
    );
    assert_eq!(panned.size, fitted.size);
    visual.simulate_event(ScrollWheelEvent {
        position: canvas.center(),
        delta: ScrollDelta::Pixels(point(px(0.0), px(-600.0))),
        modifiers: Modifiers::control(),
        ..Default::default()
    });
    visual.run_until_parked();
    let zoomed = visual.debug_bounds("agents-node:slot:slot-free").unwrap();
    assert!(
        zoomed.size.width > panned.size.width,
        "{panned:?} -> {zoomed:?}"
    );
    assert!(zoomed.size.width <= px(148.0 * 2.0 + 0.5));
    click(visual, "agents-fit");
    assert_eq!(
        visual.debug_bounds("agents-node:slot:slot-free").unwrap(),
        fitted
    );
}

#[gpui::test]
fn settings_creation_forms_remain_reachable_after_page_changes(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(960.0), px(640.0)));
    open_settings(&model, visual, Some(SettingsPage::Connections));
    click(visual, "settings-new-connection");
    let creating = |visual: &mut VisualTestContext| {
        model.read_with(visual, |m, cx| {
            let s = m.settings.read(cx);
            (s.creating_connection, s.connection_detail)
        })
    };
    assert_eq!(creating(visual), (true, true));
    assert!(visual.debug_bounds("settings-save-connection").is_some());
    // Another page and back: the unsaved form is still there.
    click(visual, "settings-nav:general");
    assert_eq!(
        model.read_with(visual, |m, cx| m.settings.read(cx).page),
        SettingsPage::General
    );
    click(visual, "settings-nav:connections");
    assert_eq!(creating(visual), (true, true));
    // The back link drops it and shows the list.
    click(visual, "settings-back-connections");
    assert_eq!(creating(visual), (false, false));
    click(visual, "settings-nav:routes");
    click(visual, "settings-new-route");
    model.read_with(visual, |m, cx| {
        let s = m.settings.read(cx);
        assert_eq!(s.page, SettingsPage::Routes);
        assert!(s.creating_route && s.route_detail);
    });
    assert!(visual.debug_bounds("settings-save-route").is_some());
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
    assert!(section.top() >= section_tabs.bottom() && section.bottom() <= px(640.0));
    assert!(header.top() >= section.top() && header.bottom() <= section.bottom());
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
fn pulsations_detail_scrolls_unified_header_on_compact_height(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    // 853x480 corresponds to a 1080p laptop display with 225% Windows DPI scaling.
    visual.simulate_resize(size(px(853.0), px(480.0)));
    select(&model, visual, AppView::Pulsations);

    let section_tabs = visual.debug_bounds("pulsation-section-tabs").unwrap();
    let scroll = visual.debug_bounds("pulsation-section-scroll").unwrap();
    assert!(scroll.top() >= section_tabs.bottom());
    assert!(scroll.bottom() <= px(480.0));

    // In Overview, header and cancel button are rendered inside the scroll area.
    click(visual, "pulsation-overview");
    let cancel = visual.debug_bounds("pulsation-cancel").unwrap();
    assert!(cancel.top() >= scroll.top());
    assert!(cancel.bottom() <= scroll.bottom());
}

#[gpui::test]
fn forge_capture_separates_multiple_tasks_from_feature_discovery(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    select(&model, visual, AppView::Forge);
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
fn forge_failed_reply_is_explained_once_with_its_actions(cx: &mut TestAppContext) {
    use crate::forge::{ForgeAction, ForgeSection};
    use ikore_core::feature_forge::*;
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    let turn = |id: &str, kind: TurnFailureKind| FeatureTurn {
        id: id.into(),
        feature_id: "feature-1".into(),
        feature_revision: 1,
        status: "failed".into(),
        user_message: "Answers".into(),
        assistant_message: None,
        error: Some("raw".into()),
        suggested_dependencies: vec![],
        kind: TurnKind::Discovery,
        failure: Some(TurnFailure::new(kind, "raw technical detail")),
        notices: vec![],
        questions: vec![],
        question_details: vec![],
    };
    model.update(visual, |m, cx| {
        m.forge.snapshot.features = vec![Feature {
            id: "feature-1".into(),
            project_id: "layout-project".into(),
            title: "UI update".into(),
            original_idea: "Improve the interface".into(),
            revision: 1,
            approved_revision: None,
            state: FeatureState::Incubating,
            brief: Default::default(),
            evaluation: Default::default(),
            structural_level: 0,
            wave: None,
            position: 0,
            archive_reason: None,
        }];
        m.forge.selected_feature = Some("feature-1".into());
        m.forge.history = vec![
            turn("older", TurnFailureKind::ReplyLanguage),
            turn("newest", TurnFailureKind::ReplyUnreadable),
        ];
        m.select_view(AppView::Forge, cx);
        m.forge_action(ForgeAction::Section(ForgeSection::Incubation), cx);
    });
    for (width, height) in [(960.0, 640.0), (1440.0, 900.0)] {
        visual.simulate_resize(size(px(width), px(height)));
        visual.run_until_parked();
        let detail = visual.debug_bounds("forge-detail").unwrap();
        let card = visual.debug_bounds("forge-failure-newest").unwrap();
        assert!(
            card.right() <= detail.right(),
            "{card:?} overflows {detail:?}"
        );
        assert!(
            visual.debug_bounds("forge-failure-older").is_none(),
            "an older failure collapses to its summary line"
        );
        for control in [
            "retry-feature-turn-newest",
            "ask-agent-about-failure-newest",
            "toggle-failure-detail-newest",
        ] {
            let bounds = visual.debug_bounds(control).unwrap();
            assert!(
                bounds.right() <= card.right(),
                "{control}: {bounds:?} leaves {card:?}"
            );
        }
        assert!(visual.debug_bounds("retry-feature-turn-older").is_none());
    }
    click(visual, "forge-user:older:copy");
    visual.update(|_, cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().as_deref(),
            Some("Answers")
        );
    });
    // The failure can be copied even while technical detail is collapsed.
    // The newest card may be below the fold after the formatted conversation.
    let detail = visual.debug_bounds("forge-detail").unwrap();
    visual.simulate_event(ScrollWheelEvent {
        position: detail.center(),
        delta: ScrollDelta::Pixels(point(px(0.0), px(-10_000.0))),
        ..Default::default()
    });
    visual.run_until_parked();
    click(visual, "forge-failure:newest:copy");
    visual.update(|_, cx| {
        let copied = cx.read_from_clipboard().unwrap().text().unwrap();
        assert!(copied.contains("raw technical detail"));
        assert!(copied.contains("reply"));
    });
    // A repeated failure of the same kind stops offering the paid follow-up.
    model.update(visual, |m, cx| {
        m.forge.history = vec![
            turn("first", TurnFailureKind::ReplyUnreadable),
            turn("second", TurnFailureKind::ReplyUnreadable),
        ];
        cx.notify();
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("retry-feature-turn-second").is_some());
    assert!(
        visual
            .debug_bounds("ask-agent-about-failure-second")
            .is_none()
    );
}

#[gpui::test]
fn forge_graph_waves_and_delivery_fit_supported_window_sizes(cx: &mut TestAppContext) {
    use ikore_core::feature_forge::*;
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

    let mut files = Vec::new();
    let entries = std::fs::read_dir(&src_dir).expect("src directory must exist");
    for entry in entries {
        let path = entry.expect("valid dir entry").path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            files.push(path);
        }
    }
    let forge_view_dir = src_dir.join("forge_view");
    if forge_view_dir.is_dir() {
        for entry in std::fs::read_dir(&forge_view_dir).expect("forge_view dir must be readable") {
            let path = entry.expect("valid dir entry").path();
            if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                files.push(path);
            }
        }
    }

    let mut violations = Vec::new();
    for path in files {
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
                violations.push(format!(
                    "{}: {}: {}",
                    path.display(),
                    line_idx + 1,
                    line.trim()
                ));
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
    use ikore_core::feature_forge::*;
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
    states: &[ikore_core::feature_forge::FeatureState],
) {
    use ikore_core::feature_forge::*;
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
    use ikore_core::chat_service::{ChatConversation, ChatTransportKind};
    use ikore_core::feature_forge::FeatureState;
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
fn forge_multiline_composer_keeps_its_hint_below_the_input(cx: &mut TestAppContext) {
    use gpui::EntityInputHandler;
    use ikore_core::feature_forge::FeatureState;
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    forge_feature_fixture(&model, visual, &[FeatureState::Incubating]);
    select(&model, visual, AppView::Forge);
    model.update(visual, |m, cx| {
        m.forge.section = crate::forge::ForgeSection::Incubation;
        cx.notify();
    });

    for width in [960.0, 1440.0, 2400.0] {
        visual.simulate_resize(size(px(width), px(900.0)));
        let drafts = [1, 3, 6, 20]
            .into_iter()
            .map(|rows| (rows, vec!["A reply about studying and planning."; rows].join("\n")))
            .chain([
                (2, "- The main audience would be university students who want to organize their study notes and planning activities.\n- Responsive web and mobile.".into()),
                (3, "- The main audience would be university students who want to organize their study notes and planning activities.\n- Responsive web and mobile.\n".into()),
                (4, "- The main audience would be university students who want to organize their study notes and planning activities.\n- Responsive web and mobile.\n\n".into()),
                (6, "A reply about studying and planning. ".repeat(30)),
            ]);
        for (rows, draft) in drafts {
            model.update(visual, |m, cx| {
                m.forge
                    .message
                    .update(cx, |input, cx| input.set_value(draft.clone(), cx));
            });
            visual.run_until_parked();
            let input = visual.debug_bounds("forge-message-input").unwrap();
            let field = visual
                .debug_bounds("native-input:Answer questions or refine your idea")
                .unwrap();
            let hint = visual.debug_bounds("forge-message-hint").unwrap();
            let send = visual.debug_bounds("send-feature-message").unwrap();
            let composer = visual.debug_bounds("forge-message-composer").unwrap();
            let message = model.read_with(visual, |m, _| m.forge.message.clone());
            let caret = visual.update(|window, cx| {
                message.update(cx, |message, cx| {
                    let end = draft.encode_utf16().count();
                    message
                        .bounds_for_range(end..end, input, window, cx)
                        .unwrap()
                })
            });
            let minimum_input_height = px(rows.min(6) as f32 * 24.0 + 18.0);
            assert!(
                input.size.height >= minimum_input_height,
                "{width}px, {rows} rows: the input wrapper collapsed: {input:?}"
            );
            assert!(
                hint.top() + px(0.5) >= field.bottom() + px(8.0),
                "{width}px, {rows} rows: hint overlaps the field border: {hint:?}, field {field:?}, wrapper {input:?}, draft {draft:?}"
            );
            assert!(
                hint.top() >= input.bottom() + px(8.0),
                "{width}px, {rows} rows: hint overlaps the input: {hint:?}, {input:?}"
            );
            assert!(
                hint.top() + px(0.5) >= caret.bottom() + px(16.0),
                "{width}px, {rows} rows: hint overlaps the rendered input: {hint:?}, caret {caret:?}, wrapper {input:?}"
            );
            assert!(
                send.top() + px(0.5) >= hint.bottom() + px(8.0),
                "{width}px, {rows} rows: send overlaps the hint: {send:?}, {hint:?}"
            );
            assert!(send.bottom() <= composer.bottom());
            assert_eq!(
                model.read_with(visual, |m, cx| m.forge.message.read(cx).value().to_owned()),
                draft
            );
        }
    }
}

#[gpui::test]
fn forge_incubation_and_triage_render_three_resizable_columns(cx: &mut TestAppContext) {
    use ikore_core::feature_forge::FeatureState;
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
    use ikore_core::feature_forge::FeatureState;
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
    use ikore_core::terminals::{ExecutionOrigin, SessionSlot, TerminalRun, TerminalRunStatus};
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
                observation_trial_id: None,
            });
            agents.apply_snapshot(
                generation,
                AgentsDashboardSnapshot {
                    terminals,
                    fallbacks: vec![],
                    free_form_bindings: vec![],
                    api_executions: Default::default(),
                    quota_deltas: Default::default(),
                    live_activity: Vec::new(),
                    pipelines: Default::default(),
                    verifiers: Default::default(),
                },
            );
        });
    });
    for (width, height) in [(960.0, 640.0), (1440.0, 900.0)] {
        visual.simulate_resize(size(px(width), px(height)));

        // Agents: the console's event filter (ADR-040 M2) no longer spans the
        // row. The console is the dock's (feature 25, M1), expanded on a tall
        // window.
        select(&model, visual, AppView::Agents);
        click(visual, "agents-node:role:task_specifier");
        if height >= 900.0 {
            let column = visual.debug_bounds("agent-dock-console").unwrap();
            let filter = visual.debug_bounds("agent-console-filter:dock").unwrap();
            assert!(
                filter.size.width <= px(360.0) && filter.size.width < column.size.width / 2.0,
                "filter spans {:?} of a {:?} column at {width}x{height}",
                filter.size.width,
                column.size.width
            );
        }
        open_agent_drawer(&model, visual, crate::agents::AgentDetailTab::Configuration);
        let backend = visual.debug_bounds("agent-backend:task_specifier").unwrap();
        assert!(backend.size.width <= px(360.0));

        // Settings · Preparation: numeric inputs are short, actions size to their label and a
        // setting's switch sits inside its row.
        model.update(visual, |m, cx| {
            m.open_settings(Some(SettingsPage::Preparation), cx)
        });
        visual.run_until_parked();
        let modal = visual.debug_bounds("settings-modal").unwrap();
        let threshold = visual.debug_bounds("sized-input:160").unwrap();
        assert_eq!(threshold.size.width, px(160.0));
        let save = visual.debug_bounds("preparation-save").unwrap();
        let toggle = visual.debug_bounds("preparation-enabled").unwrap();
        let toggle_row = visual
            .debug_bounds("settings-row:preparation-enabled")
            .unwrap();
        assert!(save.size.width < px(320.0), "{:?}", save.size.width);
        assert!(toggle.size.width <= px(60.0), "{:?}", toggle.size.width);
        assert!(toggle.left() >= toggle_row.left() && toggle.right() <= toggle_row.right());
        assert!(save.right() <= modal.right() && toggle.right() <= modal.right());

        // Settings · New connection: names are medium, secrets long, all inside the modal.
        model.update(visual, |m, cx| {
            m.settings.update(cx, |settings, _| {
                settings.creating_connection = true;
                settings.connection_detail = true;
            });
            m.set_settings_page(SettingsPage::Connections, cx);
        });
        visual.run_until_parked();
        let name = visual.debug_bounds("sized-input:320").unwrap();
        let secret = visual.debug_bounds("sized-input:560").unwrap();
        assert!(name.size.width <= px(320.0) && name.size.width > px(0.0));
        assert!(secret.size.width <= px(560.0) && secret.size.width > name.size.width);
        assert!(secret.right() <= modal.right());
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

// ---------------------------------------------------------------------------
// ADR-027 M4: Contract, Test and Integrator briefs and the failure choices
// ---------------------------------------------------------------------------

/// The shared anatomy of a later-stage fixture: no revision, host words only.
fn later_brief(
    stage: PulsationStage,
    next_stage: Option<PulsationStage>,
    counts: &[&str],
    judgement: Vec<ikore_core::orchestration::review_brief::JudgementItem>,
    visual: ikore_core::orchestration::review_brief::StageVisual,
) -> ikore_core::orchestration::review_brief::ReviewBrief {
    ikore_core::orchestration::review_brief::ReviewBrief {
        stage,
        next_stage,
        objective: Some("Let developers export token usage to CSV from the Usage view, one row per pulsation and model, with totals that match the app.".into()),
        counts: counts.iter().map(|count| (*count).to_owned()).collect(),
        revision: None,
        judgement,
        verified: vec!["Each criterion in scope has exactly one coverage entry.".into()],
        visual,
    }
}

fn item(
    kind: ikore_core::orchestration::review_brief::JudgementKind,
    label: &str,
    text: &str,
) -> ikore_core::orchestration::review_brief::JudgementItem {
    ikore_core::orchestration::review_brief::JudgementItem {
        kind,
        label: label.into(),
        text: text.into(),
        detail: Some("An agent-written explanation that is long enough to wrap inside the item at the small window size.".into()),
        refs: Vec::new(),
    }
}

fn contract_brief() -> ikore_core::orchestration::review_brief::ReviewBrief {
    use ikore_core::orchestration::composition::GroupIntent;
    use ikore_core::orchestration::review_brief::*;
    let group = |key: &str, intent, signature: &str| {
        BriefInterfaceGroup {
        key: key.into(),
        intent,
        signature: signature.into(),
        invariants: vec![
            "Returns rows in the order the Usage view shows them, so a spreadsheet matches the screen.".into(),
            "Never panics on an empty range.".into(),
        ],
        sources: vec!["crates/ikore-core/src/usage.rs".into()],
        criteria: vec!["AC-1".into(), "AC-2".into()],
        units: vec!["core_export".into()],
    }
    };
    let mut units = match specification_brief().visual {
        StageVisual::Specification(visual) => visual.units,
        _ => unreachable!(),
    };
    units[0].interfaces = vec!["csv_writer".into(), "query_usage".into()];
    later_brief(
        PulsationStage::Contract,
        Some(PulsationStage::Code),
        &[
            "3 interface groups · 1 changed, 1 new, 1 preserved",
            "3 work units · 1 bound to an interface",
        ],
        vec![
            item(
                JudgementKind::NewInterface,
                "csv_writer",
                "pub fn write_usage_csv(rows: &[UsageRow], out: &mut impl std::io::Write) -> std::io::Result<()>",
            ),
            item(
                JudgementKind::InterfaceChange,
                "usage_view",
                "fn render_export_button(&self, loading: bool, cx: &mut Context<Self>) -> impl IntoElement",
            ),
        ],
        StageVisual::Contract(ContractVisual {
            interfaces: vec![
                group(
                    "query_usage",
                    GroupIntent::Preserve,
                    "pub fn query_usage(range: DateRange, filter: &UsageFilter) -> Result<Vec<UsageRow>, UsageError>",
                ),
                group(
                    "csv_writer",
                    GroupIntent::New,
                    "pub fn write_usage_csv(rows: &[UsageRow], out: &mut impl std::io::Write) -> std::io::Result<()>",
                ),
                group(
                    "usage_view",
                    GroupIntent::Change,
                    "fn render_export_button(&self, loading: bool, cx: &mut Context<Self>) -> impl IntoElement",
                ),
            ],
            units,
        }),
    )
}

fn test_brief() -> ikore_core::orchestration::review_brief::ReviewBrief {
    use ikore_core::orchestration::composition::{CheckStatus, CoverageKind};
    use ikore_core::orchestration::review_brief::*;
    let row = |id: &str, coverage, tests: &[&str]| {
        BriefCoverage {
        task_id: "4b8e0c62-3d1f-4a77-9b25-6c0e8f2a9d14".into(),
        criterion_id: id.into(),
        description: format!("{id}: an empty date range writes only the header row, and the totals match the Usage view."),
        task_title: "Export usage summary to CSV".into(),
        coverage,
        tests: tests.iter().map(|test| (*test).to_owned()).collect(),
        declared_checks: vec!["core_tests".into()],
        explanation: "The authored test calls write_usage_csv with an empty range and compares the output with the header.".into(),
    }
    };
    later_brief(
        PulsationStage::Test,
        Some(PulsationStage::Integrator),
        &[
            "3 criteria · 1 authored, 1 existing, 1 not applicable",
            "2 host checks · 1 passed, 1 not run",
            "1 test file · 1 new",
        ],
        vec![
            item(
                JudgementKind::ExistingTestsOnly,
                "AC-2",
                "Totals match the Usage view for the same filter.",
            ),
            item(
                JudgementKind::UnexecutedCheck,
                "ui_tests",
                "The host has no passing receipt for this required check: not run.",
            ),
        ],
        StageVisual::Test(TestVisual {
            coverage: vec![
                row(
                    "AC-1",
                    CoverageKind::Authored,
                    &["crates/ikore-core/tests/usage_export_header_only_test.rs"],
                ),
                row(
                    "AC-2",
                    CoverageKind::Existing,
                    &["crates/ikore-core/tests/usage_test.rs"],
                ),
                row("AC-3", CoverageKind::NotApplicable, &[]),
            ],
            checks: vec![
                BriefCheck {
                    check_ref: "core_tests".into(),
                    required: true,
                    status: CheckStatus::Passed,
                    receipt: Some("dc3424cc-21d5-44aa-8b00-ee1c998a66e4".into()),
                },
                BriefCheck {
                    check_ref: "ui_tests".into(),
                    required: true,
                    status: CheckStatus::NotRun,
                    receipt: None,
                },
            ],
            files: vec![BriefFileGroup {
                root: "crates/ikore-core".into(),
                files: vec![BriefFile {
                    path: "tests/usage_export_header_only_test.rs".into(),
                    planned: true,
                    interfaces: Vec::new(),
                }],
            }],
        }),
    )
}

fn integrator_brief() -> ikore_core::orchestration::review_brief::ReviewBrief {
    use ikore_core::orchestration::PublicationState;
    use ikore_core::orchestration::composition::Verdict;
    use ikore_core::orchestration::review_brief::*;
    let verdict = |id: &str, verdict| {
        BriefVerdict {
        task_id: "4b8e0c62-3d1f-4a77-9b25-6c0e8f2a9d14".into(),
        criterion_id: id.into(),
        description: format!("{id}: the export keeps one row per pulsation and model."),
        task_title: "Export usage summary to CSV".into(),
        verdict,
        checks: Vec::new(),
        explanation: "The implementation groups by pulsation and model; the authored test asserts one row per pair.".into(),
    }
    };
    let file = |path: &str, stage, state| BriefPublication {
        path: path.into(),
        stage,
        state,
    };
    later_brief(
        PulsationStage::Integrator,
        None,
        &[
            "3 criteria · 1 supported, 1 contradicted, 1 unverified",
            "1 blocker",
            "No host check for this stage",
            "3 files · 2 published, 1 to publish",
        ],
        vec![
            item(
                JudgementKind::Contradicted,
                "AC-2",
                "Totals match the Usage view for the same filter.",
            ),
            item(
                JudgementKind::Blocker,
                "B1",
                "The CSV header order differs from the Usage view columns, so a spreadsheet does not line up with the screen.",
            ),
        ],
        StageVisual::Integrator(IntegratorVisual {
            verdicts: vec![
                verdict("AC-1", Verdict::Supported),
                verdict("AC-2", Verdict::Contradicted),
                verdict("AC-3", Verdict::Unverified),
            ],
            blockers: vec!["The CSV header order differs from the Usage view columns.".into()],
            checks: Vec::new(),
            publications: vec![
                file(
                    "crates/ikore-core/src/usage/export.rs",
                    PulsationStage::Code,
                    PublicationState::Applied,
                ),
                file(
                    "crates/ikore-core/tests/usage_export_header_only_test_with_a_long_name.rs",
                    PulsationStage::Test,
                    PublicationState::Applied,
                ),
                file(
                    "knowledge/obsidian/features/usage-export-to-csv.md",
                    PulsationStage::Integrator,
                    PublicationState::Pending,
                ),
            ],
        }),
    )
}

/// Asserts each section fits the reader and each row head is one line;
/// `long` heads (a long path in a narrow card) may take three, far from the
/// one-character-per-line collapse the M3 diff once had.
fn assert_brief_fits(
    visual: &mut VisualTestContext,
    sections: &[&'static str],
    heads: &[&'static str],
    long: &[&'static str],
    width: f32,
) {
    let reader = visual.debug_bounds("review-document-reader").unwrap();
    for section in sections {
        let bounds = visual
            .debug_bounds(section)
            .unwrap_or_else(|| panic!("missing {section} at {width}"));
        assert!(
            bounds.left() >= reader.left() && bounds.right() <= reader.right() + px(1.0),
            "{section} overflows the reader at {width}: {bounds:?} vs {reader:?}"
        );
    }
    for head in heads {
        let bounds = visual
            .debug_bounds(head)
            .unwrap_or_else(|| panic!("missing {head} at {width}"));
        let limit = if long.contains(head) {
            px(64.0)
        } else {
            px(24.0)
        };
        assert!(
            bounds.size.height <= limit,
            "{head} is {bounds:?} at {width}"
        );
    }
}

/// One later-stage brief and the selectors its window must draw.
struct StageCase {
    brief: ikore_core::orchestration::review_brief::ReviewBrief,
    sections: &'static [&'static str],
    heads: &'static [&'static str],
    long: &'static [&'static str],
}

#[gpui::test]
fn later_stage_briefs_draw_their_own_visuals_and_fit_both_window_widths(cx: &mut TestAppContext) {
    let stages = [
        StageCase {
            brief: contract_brief(),
            sections: &[
                "brief-decision",
                "brief-judgement",
                "brief-verified",
                "brief-interfaces",
                "brief-units",
            ],
            heads: &[
                "brief-interface-0",
                "brief-interface-1",
                "brief-interface-2",
            ],
            long: &[],
        },
        StageCase {
            brief: test_brief(),
            sections: &[
                "brief-decision",
                "brief-judgement",
                "brief-coverage",
                "brief-checks",
                "brief-test-files",
            ],
            heads: &[
                "brief-coverage-0",
                "brief-coverage-1",
                "brief-coverage-2",
                "brief-check-0",
                "brief-check-1",
            ],
            long: &[],
        },
        StageCase {
            brief: integrator_brief(),
            sections: &[
                "brief-decision",
                "brief-judgement",
                "brief-verdicts",
                "brief-checks",
                "brief-publications",
            ],
            heads: &[
                "brief-verdict-0",
                "brief-verdict-1",
                "brief-verdict-2",
                "brief-publication-0",
                "brief-publication-1",
                "brief-publication-2",
            ],
            long: &[
                "brief-publication-0",
                "brief-publication-1",
                "brief-publication-2",
            ],
        },
    ];
    for StageCase {
        brief,
        sections,
        heads,
        long,
    } in stages
    {
        let (model, visual) = open_brief(cx, brief);
        // Tall windows keep every row painted; the widths are the ones under test.
        for width in [960.0, 1440.0] {
            visual.simulate_resize(size(px(width), px(2400.0)));
            select(&model, visual, AppView::Pulsations);
            assert_brief_fits(visual, sections, heads, long, width);
        }
        // Only the stage's own visual is drawn in its window.
        assert!(visual.debug_bounds("brief-criteria").is_none());
        model.read_with(visual, |model, cx| {
            let hitl = &model.pulsations.read(cx).hitl;
            assert!(hitl.brief_selected);
            assert_eq!(hitl.marks.len(), 2, "both judgement items can be marked");
            assert!(hitl.can_approve(), "the documents still gate approval");
        });
    }
}

#[gpui::test]
fn a_parked_failure_explains_each_permitted_decision_above_its_facts(cx: &mut TestAppContext) {
    for (width, height) in [(960.0, 640.0), (1440.0, 900.0)] {
        let (_model, visual) =
            parked_failure_window(cx, "retry_budget_exhausted", true, true, (width, height));
        let detail = visual.debug_bounds("failure-detail").unwrap();
        let choices = visual.debug_bounds("failure-choices").unwrap();
        assert!(
            choices.left() >= detail.left() && choices.right() <= detail.right() + px(1.0),
            "choices overflow at {width}: {choices:?} vs {detail:?}"
        );
        assert!(choices.top() >= detail.top(), "choices lead the detail");
        // Retry, Revise from Code, Abort: as many rows as the bar has buttons.
        for row in ["failure-choice-0", "failure-choice-1", "failure-choice-2"] {
            let bounds = visual.debug_bounds(row).unwrap();
            assert!(
                bounds.left() >= choices.left() && bounds.right() <= choices.right() + px(1.0),
                "{row} overflows at {width}: {bounds:?}"
            );
        }
        let bar = visual.debug_bounds("failure-decision-bar").unwrap();
        assert!(bar.bottom() <= px(height), "the bar stays on screen");
    }
    // A failure without retry lists only revision and abort.
    let (_model, visual) =
        parked_failure_window(cx, "model_reported_failure", false, true, (1440.0, 900.0));
    visual.debug_bounds("failure-choice-1").unwrap();
    assert!(visual.debug_bounds("failure-choice-2").is_none());
}

/// Rows with short text used to take the pane's width instead of their section's, so their
/// controls stuck out past the section while longer rows lined up.
#[gpui::test]
fn settings_rows_keep_to_their_section_and_align_their_controls(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(1280.0), px(800.0)));
    open_settings(&model, visual, Some(SettingsPage::SecondBrain));
    let section = visual
        .debug_bounds("settings-section:Note expansion")
        .unwrap();
    let mut control_right = None;
    for (row, control) in [
        (
            "settings-row:adaptive-initial-k",
            "settings-adaptive-initial-k",
        ),
        ("settings-row:adaptive-max-k", "settings-adaptive-max-k"),
        (
            "settings-row:adaptive-similarity",
            "settings-adaptive-similarity",
        ),
    ] {
        let row_bounds = visual.debug_bounds(row).unwrap();
        let control_bounds = visual.debug_bounds(control).unwrap();
        assert_eq!(
            row_bounds.right(),
            section.right(),
            "{row} overflows its section"
        );
        assert!(control_bounds.right() <= row_bounds.right());
        let right = control_right.get_or_insert(control_bounds.right());
        assert_eq!(*right, control_bounds.right(), "{control} is out of line");
    }
}

/// An RFC 3339 timestamp `minutes` before now (the dashboard reads the last seven days).
fn minutes_ago(minutes: i64) -> String {
    (chrono::Utc::now() - chrono::Duration::minutes(minutes)).to_rfc3339()
}

fn layout_run(
    role: AgentRole,
    id: &str,
    started_minutes_ago: i64,
    took_minutes: i64,
    pulsation: Option<&str>,
) -> ikore_core::terminals::TerminalRun {
    use ikore_core::terminals::{ExecutionOrigin, SessionSlot, TerminalRun, TerminalRunStatus};
    TerminalRun {
        id: id.into(),
        project_id: "layout-project".into(),
        slot: SessionSlot::Canonical { agent_role: role },
        slot_id: format!("canonical:layout-project:{}", role.as_str()),
        agent_role: Some(role),
        origin: ExecutionOrigin::Pipeline,
        execution_mode: AgentExecutionMode::CliProcess,
        backend: Some(CliBackend::Codex),
        model: None,
        requested_model_profile: None,
        model_source: CliModelSelectionSource::Default,
        observed_model: None,
        native_session_id: None,
        pulsation_id: pulsation.map(Into::into),
        job_id: None,
        agent_run_id: None,
        llm_execution_id: None,
        status: TerminalRunStatus::Completed,
        exit_code: Some(0),
        error_code: None,
        error_message: None,
        created_at: minutes_ago(started_minutes_ago),
        started_at: Some(minutes_ago(started_minutes_ago)),
        finished_at: Some(minutes_ago(started_minutes_ago - took_minutes)),
        updated_at: minutes_ago(started_minutes_ago - took_minutes),
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
        observation_trial_id: None,
    }
}

#[gpui::test]
fn home_dashboard_shows_the_week_in_numbers_and_charts(cx: &mut TestAppContext) {
    use ikore_core::usage::{ModelUsage, UsageBucket, UsageSummary, UsageTotals, UsageTransport};
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    let today = chrono::Utc::now().date_naive();
    model.update(visual, |m, cx| {
        let totals = |input: i64, output: i64| UsageTotals {
            input_tokens: Some(input),
            output_tokens: Some(output),
            total_tokens: Some(input + output),
            records: 1,
            ..Default::default()
        };
        m.home_stats.usage = Some(UsageSummary {
            series: vec![
                UsageBucket {
                    period: (today - chrono::Duration::days(2)).to_string(),
                    totals: totals(4_000, 1_000),
                },
                UsageBucket {
                    period: today.to_string(),
                    totals: totals(12_000, 3_000),
                },
            ],
            models: vec![ModelUsage {
                provider_id: "openai".into(),
                model_id: "gpt-5".into(),
                model_source: "configured".into(),
                transport: UsageTransport::Api,
                totals: totals(16_000, 4_000),
            }],
            ..Default::default()
        });
        m.agents.update(cx, |agents, _| {
            let snapshot = agents.snapshot.as_mut().expect("fixture snapshot");
            for agent in &mut snapshot.agents {
                let role = agent.config.agent_role;
                agent.recent_runs = vec![
                    layout_run(
                        role,
                        &format!("{}-1", role.as_str()),
                        50,
                        4,
                        Some("pulse-live"),
                    ),
                    layout_run(role, &format!("{}-2", role.as_str()), 300, 9, None),
                ];
            }
        });
        m.pulsations.update(cx, |state, _| {
            for (id, status, created, updated) in [
                ("pulse-done", PulsationStatus::Done, 200, 150),
                ("pulse-live", PulsationStatus::Running, 60, 1),
            ] {
                state.pulsations.push(Pulsation {
                    id: id.into(),
                    project_id: "layout-project".into(),
                    feature_name: format!("Dashboard {id}"),
                    stage: PulsationStage::Code,
                    status,
                    terminal_reason: None,
                    created_at: minutes_ago(created),
                    updated_at: minutes_ago(updated),
                });
            }
            state.select("pulse-live");
        });
        cx.notify();
    });
    visual.simulate_resize(size(px(1440.0), px(1600.0)));
    select(&model, visual, AppView::Home);
    for id in [
        "home-kpi-tokens",
        "home-kpi-runs",
        "home-kpi-pulsation",
        "home-kpi-active",
        "home-chart-tokens",
        "home-chart-models",
        "home-chart-agents",
        "home-chart-pulsations",
        "home-active-timeline",
        "home-agent-0",
        "home-agent-1",
        "home-pulsation-0",
        "home-pulsation-1",
    ] {
        assert!(visual.debug_bounds(id).is_some(), "missing {id}");
    }
    // Headline numbers first, then what is waiting on you, then the charts two by two.
    let kpis = visual.debug_bounds("home-kpis").unwrap();
    let waiting = visual.debug_bounds("home-waiting").unwrap();
    let tokens = visual.debug_bounds("home-chart-tokens").unwrap();
    let models = visual.debug_bounds("home-chart-models").unwrap();
    let agents = visual.debug_bounds("home-chart-agents").unwrap();
    assert!(kpis.bottom() <= waiting.top(), "{kpis:?} {waiting:?}");
    assert!(waiting.bottom() <= tokens.top(), "{waiting:?} {tokens:?}");
    assert!(models.left() >= tokens.right(), "{tokens:?} {models:?}");
    assert_eq!(models.top(), tokens.top());
    assert!(agents.top() >= tokens.bottom());
    // Seven day columns, oldest on the left; every card stays inside the window.
    let first = visual.debug_bounds("home-day-0").unwrap();
    let last = visual.debug_bounds("home-day-6").unwrap();
    assert!(last.left() > first.right());
    assert!(models.right() <= px(1440.0));
    // A pulsation bar opens that pulsation (oldest first: the finished one).
    click(visual, "home-pulsation-0");
    assert_eq!(
        model.read_with(visual, |m, _| m.active_view()),
        AppView::Pulsations
    );
    assert_eq!(
        model.read_with(visual, |m, cx| m.pulsations.read(cx).active_id()),
        Some("pulse-done".to_string())
    );
    // Narrow windows stack the charts one per row.
    visual.simulate_resize(size(px(960.0), px(1800.0)));
    select(&model, visual, AppView::Home);
    let tokens = visual.debug_bounds("home-chart-tokens").unwrap();
    let models = visual.debug_bounds("home-chart-models").unwrap();
    assert!(models.top() >= tokens.bottom(), "{tokens:?} {models:?}");
    assert!(models.right() <= px(960.0));
}

#[gpui::test]
fn tab_moves_focus_through_controls_and_stays_inside_settings(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| {
        let mut model = fixture(cx);
        with_settings_items(&mut model, cx);
        model
    });
    visual.update(|_, cx| crate::shell::register_navigation_keybindings(cx));
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    select(&model, visual, AppView::Forge);
    // A click focuses the rail item without a keyboard ring.
    click(visual, "rail-nav-forge");
    let clicked = visual.update(|window, cx| (window.focused(cx), window.focus_visible()));
    assert!(clicked.0.is_some() && !clicked.1);
    visual.simulate_keystrokes("tab");
    let first = visual.update(|window, cx| (window.focused(cx), window.focus_visible()));
    assert!(first.1, "Tab shows the keyboard ring");
    assert_ne!(first.0, clicked.0, "Tab moved the focus");
    visual.simulate_keystrokes("shift-tab");
    let back = visual.update(|window, cx| window.focused(cx));
    assert_eq!(back, clicked.0, "Shift-Tab walks back");
    // With Settings open, Tab never leaves the modal, however many times it is pressed.
    open_settings(&model, visual, Some(SettingsPage::General));
    let mut saw_tooltip = false;
    for press in 0..40 {
        visual.simulate_keystrokes("tab");
        visual
            .executor()
            .advance_clock(std::time::Duration::from_millis(50));
        visual.run_until_parked();
        let inside = visual.update(|window, cx| model.read(cx).settings_focus_contains(window, cx));
        assert!(inside, "press {press} left the Settings modal");
        saw_tooltip |= visual.debug_bounds("tooltip").is_some();
    }
    assert!(
        saw_tooltip,
        "an \"i\" reached with Tab opens its explanation"
    );
}

#[gpui::test]
fn backlog_task_actions_sit_at_the_card_top_right(cx: &mut TestAppContext) {
    use ikore_core::feature_forge::{TaskDraft, TaskKind, WorkTask};
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    model.update(visual, |m, cx| {
        m.forge.snapshot.tasks = vec![WorkTask {
            id: "task-layout".into(),
            project_id: "layout-project".into(),
            revision: 1,
            draft: TaskDraft {
                title: "Export records as CSV".into(),
                kind: TaskKind::Feature,
                description: "Add a CSV export to the records page with the visible columns."
                    .into(),
                criteria: vec![],
                dependencies: vec![],
                feature_id: None,
            },
            feature_revision: None,
            state: "ready".into(),
            active_pulsation_id: None,
            completed_pulsation_id: None,
        }];
        cx.notify();
    });
    for (width, height) in [(960.0, 640.0), (1440.0, 900.0)] {
        visual.simulate_resize(size(px(width), px(height)));
        select(&model, visual, AppView::Forge);
        let card = visual.debug_bounds("task-card-task-layout").unwrap();
        let actions = visual.debug_bounds("task-actions-task-layout").unwrap();
        let select_button = visual.debug_bounds("select-task-task-layout").unwrap();
        assert!(actions.right() <= card.right() && card.right() - actions.right() <= px(16.0));
        assert!(
            actions.top() - card.top() <= px(16.0),
            "{width}: {actions:?} vs {card:?}"
        );
        assert!(select_button.top() >= actions.top() && select_button.right() <= actions.right());
    }
}

#[gpui::test]
fn companion_list_pins_folds_the_archive_searches_and_queues(cx: &mut TestAppContext) {
    use ikore_core::chat_service::{
        ChatConversation, ChatConversationMatch, ChatMessage, ChatTransportKind, ConversationMarks,
    };
    let conversation = |id: &str, title: &str| ChatConversation {
        id: id.into(),
        project_id: "layout-project".into(),
        connection_id: None,
        provider_id: "openai".into(),
        model_id: "gpt-5".into(),
        transport: ChatTransportKind::Cli,
        cli_backend: Some(CliBackend::Codex),
        model_profile: None,
        model_source: CliModelSelectionSource::Default,
        title: title.into(),
        connection_available: true,
        created_at: "now".into(),
        updated_at: "now".into(),
    };
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    select(&model, visual, AppView::Chat);
    model.update(visual, |m, cx| {
        m.chat.update(cx, |chat, _| {
            chat.project_id = Some("layout-project".into());
            chat.conversations = vec![
                conversation("recent", "Recent work"),
                conversation("older", "Older work"),
                conversation("old", "Old notes"),
            ];
            chat.marks = ConversationMarks {
                pinned: vec!["older".into()],
                archived: vec!["old".into()],
            };
            chat.active_conversation = Some("recent".into());
            chat.messages_by_conversation.insert(
                "recent".into(),
                vec![ChatMessage {
                    id: "message-0".into(),
                    conversation_id: "recent".into(),
                    ordinal: 0,
                    role: "user".into(),
                    content: "A question.".into(),
                    status: "completed".into(),
                    execution_id: None,
                    generation_id: None,
                    last_sequence: 0,
                    input_tokens: None,
                    output_tokens: None,
                    estimated_cost_usd: None,
                    error_code: None,
                    error_message: None,
                    target_label: None,
                    model_evidence: None,
                    created_at: "now".into(),
                    updated_at: "now".into(),
                }],
            );
            chat.sync_panel_with_active_conversation();
            chat.enqueue("recent", "A follow-up written while it replied".into());
        });
        cx.notify();
    });
    visual.run_until_parked();
    // The pinned conversation comes first; the archived one waits under its fold.
    let pinned = visual.debug_bounds("chat-conversation:older").unwrap();
    let recent = visual.debug_bounds("chat-conversation:recent").unwrap();
    let fold = visual.debug_bounds("chat-archived-toggle").unwrap();
    assert!(pinned.bottom() <= recent.top(), "{pinned:?} {recent:?}");
    assert!(recent.bottom() <= fold.top());
    // Pin, archive and delete fit in the row, after the title.
    for action in [
        "chat-pin:recent",
        "chat-archive:recent",
        "chat-delete:recent",
    ] {
        let bounds = visual.debug_bounds(action).unwrap();
        assert!(
            bounds.left() >= recent.left() && bounds.right() <= recent.right(),
            "{action} {bounds:?} leaves {recent:?}"
        );
    }
    click(visual, "chat-archived-toggle");
    assert!(model.read_with(visual, |m, cx| m.chat.read(cx).show_archived));
    let archived = visual.debug_bounds("chat-conversation:old").unwrap();
    assert!(archived.top() >= fold.bottom());
    // A finished message can be branched; the queue sits above the composer's field.
    let message = visual.debug_bounds("chat-message:message-0").unwrap();
    let branch = visual.debug_bounds("chat-branch:message-0").unwrap();
    assert!(branch.top() >= message.top() && branch.bottom() <= message.bottom());
    let composer = visual.debug_bounds("chat-composer").unwrap();
    let queue = visual.debug_bounds("chat-queue").unwrap();
    let queued = visual.debug_bounds("chat-queued:0").unwrap();
    assert!(queue.top() >= composer.top() && queued.bottom() <= composer.bottom());
    // Free again with a message waiting, the conversation offers to send it now.
    assert!(visual.debug_bounds("chat-queued-send:0").is_some());
    // Search results replace the list while they are shown.
    model.update(visual, |m, cx| {
        m.chat.update(cx, |chat, _| {
            chat.conversation_search = Some(crate::chat::ConversationSearch {
                query: "work".into(),
                matches: vec![ChatConversationMatch {
                    conversation_id: "older".into(),
                    title: "Older work".into(),
                    excerpt: Some("…the work on the login…".into()),
                    updated_at: "now".into(),
                }],
                searching: false,
            });
        });
        cx.notify();
    });
    visual.run_until_parked();
    let field = visual.debug_bounds("chat-search-field").unwrap();
    let found = visual.debug_bounds("chat-search-result:older").unwrap();
    assert!(found.top() > field.bottom());
    assert!(found.left() >= field.left() - px(1.0) && found.right() <= field.right() + px(1.0));
    click(visual, "chat-search-result:older");
    assert_eq!(
        model.read_with(visual, |m, cx| m
            .chat
            .read(cx)
            .active_conversation
            .as_ref()
            .map(ToString::to_string)),
        Some("older".to_string())
    );
    click(visual, "chat-search-clear");
    assert!(model.read_with(visual, |m, cx| {
        m.chat.read(cx).conversation_search.is_none()
    }));
}

#[gpui::test]
fn companion_conversation_is_a_centred_reading_column(cx: &mut TestAppContext) {
    use ikore_core::chat_service::{
        ChatConversation, ChatMessage, ChatMessageModelEvidence, ChatModelVerification,
        ChatTransportKind,
    };
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(1920.0), px(1080.0)));
    // Opening Chat hydrates it for the project; the conversation is injected afterwards.
    select(&model, visual, AppView::Chat);
    model.update(visual, |m, cx| {
        m.chat.update(cx, |chat, _| {
            chat.project_id = Some("layout-project".into());
            chat.conversations = vec![ChatConversation {
                id: "conversation-1".into(),
                project_id: "layout-project".into(),
                connection_id: None,
                provider_id: "openai".into(),
                model_id: "gpt-5".into(),
                transport: ChatTransportKind::Cli,
                cli_backend: Some(CliBackend::Codex),
                model_profile: None,
                model_source: CliModelSelectionSource::Default,
                title: "Architecture questions".into(),
                connection_available: true,
                created_at: "now".into(),
                updated_at: "now".into(),
            }];
            chat.active_conversation = Some("conversation-1".into());
            chat.messages_by_conversation.insert(
                "conversation-1".into(),
                ["user", "assistant"]
                    .into_iter()
                    .enumerate()
                    .map(|(ordinal, role)| ChatMessage {
                        id: format!("message-{ordinal}"),
                        conversation_id: "conversation-1".into(),
                        ordinal: ordinal as i64,
                        role: role.into(),
                        content: format!("A {role} message."),
                        status: "completed".into(),
                        execution_id: None,
                        generation_id: None,
                        last_sequence: 0,
                        input_tokens: None,
                        output_tokens: None,
                        estimated_cost_usd: None,
                        error_code: None,
                        error_message: None,
                        target_label: None,
                        model_evidence: (role == "assistant").then(|| ChatMessageModelEvidence {
                            transport: ChatTransportKind::Cli,
                            provider_id: "codex".into(),
                            requested_model: "gpt-5".into(),
                            model_profile: Some("high".into()),
                            observed_model: Some("gpt-5".into()),
                            verification: ChatModelVerification::Verified,
                        }),
                        created_at: "now".into(),
                        updated_at: "now".into(),
                    })
                    .collect(),
            );
            chat.sync_panel_with_active_conversation();
        });
        cx.notify();
    });
    visual.run_until_parked();
    // The model line closes the answer, under its header and inside the message. A user
    // message has none.
    let answer = visual.debug_bounds("chat-message:message-1").unwrap();
    let answer_copy = visual.debug_bounds("chat-copy-message:message-1").unwrap();
    let model_line = visual.debug_bounds("chat-message-model:message-1").unwrap();
    assert!(
        model_line.top() >= answer_copy.bottom() && model_line.bottom() <= answer.bottom(),
        "model line {model_line:?} is not under the answer {answer:?}"
    );
    assert!(model_line.left() >= answer.left() && model_line.right() <= answer.right());
    assert!(
        visual
            .debug_bounds("chat-message-model:message-0")
            .is_none()
    );
    let conversation = visual
        .debug_bounds("chat-conversation:conversation-1")
        .unwrap();
    let delete = visual.debug_bounds("chat-delete:conversation-1").unwrap();
    assert!(delete.right() <= conversation.right() && delete.top() >= conversation.top());
    assert!(delete.size.width <= px(36.0) && delete.size.height <= px(36.0));
    click(visual, "chat-delete:conversation-1");
    model.read_with(visual, |model, cx| {
        assert_eq!(
            model
                .chat
                .read(cx)
                .pending_delete_conversation_id
                .as_ref()
                .map(ToString::to_string)
                .as_deref(),
            Some("conversation-1")
        );
    });
    assert_eq!(
        visual
            .debug_bounds("chat-delete:conversation-1")
            .unwrap()
            .size,
        delete.size
    );
    click(visual, "chat-copy-message:message-0");
    visual.update(|_, cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().as_deref(),
            Some("A user message.")
        );
    });
    click(visual, "chat-copy-message:message-1");
    visual.update(|_, cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().as_deref(),
            Some("A assistant message.")
        );
    });
    model.update(visual, |m, cx| {
        m.chat.update(cx, |chat, _| {
            let answer = &mut chat
                .messages_by_conversation
                .get_mut("conversation-1")
                .unwrap()[1];
            answer.content.clear();
            answer.error_message =
                Some("Companion CLI returned an invalid control envelope".into());
            answer.status = "failed".into();
        });
        cx.notify();
    });
    visual.run_until_parked();
    click(visual, "chat-copy-message:message-1");
    visual.update(|_, cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().as_deref(),
            Some("Companion CLI returned an invalid control envelope")
        );
    });
    let composer = visual.debug_bounds("chat-composer").unwrap();
    let header = visual.debug_bounds("chat-header").unwrap();
    let scroll = visual.debug_bounds("chat-messages-scroll").unwrap();
    assert!(composer.size.width <= px(760.0), "{composer:?}");
    assert_eq!(composer.left(), header.left());
    assert_eq!(composer.right(), header.right());
    // Centred in the conversation pane, which is wider than the column.
    assert!(scroll.size.width > px(900.0), "{scroll:?}");
    let offset = (composer.center().x - scroll.center().x).abs();
    assert!(offset <= px(2.0), "composer off centre by {offset:?}");
    // The composer sits under the conversation, not beside it.
    assert!(composer.top() >= scroll.bottom());
    // Model, effort and route are chosen in the composer, left of the keyboard hint and Send.
    let target = visual.debug_bounds("chat-composer-target").unwrap();
    let keys = visual.debug_bounds("info:chat-composer-keys").unwrap();
    assert!(target.left() >= composer.left() && target.bottom() <= composer.bottom());
    assert!(
        target.right() <= keys.left() && keys.right() <= composer.right(),
        "{target:?} overlaps {keys:?}"
    );
    for control in [
        "chat-composer-model",
        "chat-composer-profile",
        "chat-composer-route",
    ] {
        let bounds = visual.debug_bounds(control).unwrap();
        assert!(
            bounds.left() >= target.left()
                && bounds.right() <= target.right()
                && bounds.top() >= target.top()
                && bounds.bottom() <= target.bottom(),
            "{control} {bounds:?} leaves the composer target {target:?}"
        );
    }
    assert!(
        visual
            .debug_bounds("chat-composer-target-deferred")
            .is_none()
    );
    // A long draft grows the card with it: the Send row never spills past the border, whatever
    // the number of wrapped lines (the input caps at six rows and then scrolls).
    let sentence = "A long question about the architecture of this repository, typed at length. ";
    for repeats in [1, 2, 4, 6, 8, 12, 40] {
        let text = sentence.repeat(repeats);
        model.update(visual, |m, cx| {
            m.chat_draft_input
                .update(cx, |input, cx| input.set_value(text, cx))
        });
        visual.run_until_parked();
        let composer = visual.debug_bounds("chat-composer").unwrap();
        let actions = visual.debug_bounds("chat-composer-actions").unwrap();
        assert!(
            actions.bottom() <= composer.bottom(),
            "{repeats} sentences: actions {actions:?} spill out of composer {composer:?}"
        );
    }
    // Long answers are measured at the column's width too, so the list reaches its last line.
    model.update(visual, |m, cx| {
        m.chat.update(cx, |chat, _| {
            let messages = chat
                .messages_by_conversation
                .get_mut("conversation-1")
                .unwrap();
            messages[1].content = sentence.repeat(60);
        });
        cx.notify();
    });
    visual.run_until_parked();
    let column = visual.debug_bounds("chat-messages-column").unwrap();
    let answer = visual.debug_bounds("chat-message:message-1").unwrap();
    assert!(
        answer.bottom() <= column.bottom(),
        "answer {answer:?} overflows its column {column:?}"
    );
    let scroll = visual.debug_bounds("chat-messages-scroll").unwrap();
    visual.simulate_event(gpui::ScrollWheelEvent {
        position: scroll.center(),
        delta: gpui::ScrollDelta::Pixels(point(px(0.0), px(-10_000.0))),
        ..Default::default()
    });
    visual.run_until_parked();
    let answer = visual.debug_bounds("chat-message:message-1").unwrap();
    assert!(
        answer.bottom() <= scroll.bottom(),
        "the end of {answer:?} cannot be scrolled into {scroll:?}"
    );
}

#[gpui::test]
fn companion_without_a_conversation_chooses_its_target_in_the_start_card(cx: &mut TestAppContext) {
    use ikore_core::chat_service::ChatTransportKind;
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    select(&model, visual, AppView::Chat);
    model.update(visual, |m, cx| {
        m.chat.update(cx, |chat, _| {
            chat.project_id = Some("layout-project".into());
            chat.conversations.clear();
            chat.active_conversation = None;
        });
        cx.notify();
    });
    visual.run_until_parked();
    // The left column lists conversations and starts one; it no longer chooses the target.
    assert!(visual.debug_bounds("chat-new-conversation").is_some());
    for removed in [
        "chat-toggle-config",
        "chat-transport",
        "chat-cli-backend",
        "chat-cli-profile",
        "chat-cycle-connection",
    ] {
        assert!(visual.debug_bounds(removed).is_none(), "{removed} is back");
    }
    // The start card holds the composer's controls and the button that starts the conversation.
    let start = visual.debug_bounds("chat-start").unwrap();
    assert!(start.size.width <= px(760.0), "{start:?}");
    assert!(visual.debug_bounds("chat-composer").is_none());
    let target = visual.debug_bounds("chat-composer-target").unwrap();
    let begin = visual.debug_bounds("chat-start-conversation").unwrap();
    for (name, bounds) in [("target", target), ("start button", begin)] {
        assert!(
            bounds.left() >= start.left()
                && bounds.right() <= start.right()
                && bounds.top() >= start.top()
                && bounds.bottom() <= start.bottom(),
            "{name} {bounds:?} leaves the start card {start:?}"
        );
    }
    assert!(
        target.right() <= begin.left(),
        "{target:?} overlaps {begin:?}"
    );
    for control in ["chat-composer-model", "chat-composer-route"] {
        let bounds = visual.debug_bounds(control).unwrap();
        assert!(
            bounds.left() >= target.left() && bounds.right() <= target.right(),
            "{control} {bounds:?} leaves {target:?}"
        );
    }
    // With no conversation open, the route chosen here is where the next one starts.
    click(visual, "chat-composer-route");
    click(visual, "choice:cli:claude");
    model.read_with(visual, |m, cx| {
        let chat = m.chat.read(cx);
        assert_eq!(chat.selected_transport, ChatTransportKind::Cli);
        assert_eq!(chat.selected_cli_backend, CliBackend::Claude);
        assert!(m.choice_menu.is_none());
    });
    assert!(visual.debug_bounds("chat-composer-profile").is_some());
}

/// Opens a conversation of `pairs` questions, `message-0`, `message-2`…, each followed by an
/// answer of `answer_sentences` sentences.
fn open_conversation_of(
    model: &Entity<AppModel>,
    visual: &mut VisualTestContext,
    pairs: usize,
    answer_sentences: usize,
) {
    use ikore_core::chat_service::{ChatConversation, ChatMessage, ChatTransportKind};
    model.update(visual, |m, cx| {
        m.chat.update(cx, |chat, _| {
            chat.project_id = Some("layout-project".into());
            chat.conversations = vec![ChatConversation {
                id: "conversation-1".into(),
                project_id: "layout-project".into(),
                connection_id: None,
                provider_id: "codex".into(),
                model_id: "gpt-5".into(),
                transport: ChatTransportKind::Cli,
                cli_backend: Some(CliBackend::Codex),
                model_profile: None,
                model_source: CliModelSelectionSource::Default,
                title: "Questions".into(),
                connection_available: true,
                created_at: "now".into(),
                updated_at: "now".into(),
            }];
            chat.active_conversation = Some("conversation-1".into());
            chat.history_expanded = None;
            chat.messages_by_conversation.insert(
                "conversation-1".into(),
                (0..pairs * 2)
                    .map(|ordinal| {
                        let question = ordinal % 2 == 0;
                        ChatMessage {
                            id: format!("message-{ordinal}"),
                            conversation_id: "conversation-1".into(),
                            ordinal: ordinal as i64,
                            role: if question { "user" } else { "assistant" }.into(),
                            content: if question {
                                format!("Question number {}?", ordinal / 2)
                            } else {
                                format!("Answer number {}. ", ordinal / 2)
                                    + &"It goes on for a while. ".repeat(answer_sentences)
                            },
                            status: "completed".into(),
                            execution_id: None,
                            generation_id: None,
                            last_sequence: 0,
                            input_tokens: None,
                            output_tokens: None,
                            estimated_cost_usd: None,
                            error_code: None,
                            error_message: None,
                            target_label: None,
                            model_evidence: None,
                            created_at: "now".into(),
                            updated_at: "now".into(),
                        }
                    })
                    .collect(),
            );
            chat.sync_panel_with_active_conversation();
        });
        m.chat_question_layout.borrow_mut().reset();
        m.chat_scroll.set_offset(point(px(0.0), px(0.0)));
        cx.notify();
    });
    visual.run_until_parked();
}

/// How far below the top of the conversation's viewport a message is drawn.
fn message_offset(visual: &mut VisualTestContext, message: &'static str) -> gpui::Pixels {
    let scroll = visual.debug_bounds("chat-messages-scroll").unwrap();
    let message = visual
        .debug_bounds(message)
        .unwrap_or_else(|| panic!("missing {message}"));
    message.top() - scroll.top()
}

#[gpui::test]
fn companion_question_rail_goes_from_one_question_to_another(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.update(|_, cx| crate::shell::register_navigation_keybindings(cx));
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    select(&model, visual, AppView::Chat);
    // One question has nowhere to go, and neither do two that fit the viewport.
    open_conversation_of(&model, visual, 1, 200);
    assert!(visual.debug_bounds("chat-question-rail").is_none());
    open_conversation_of(&model, visual, 2, 1);
    assert!(visual.debug_bounds("chat-question-rail").is_none());

    open_conversation_of(&model, visual, 6, 120);
    let rail = visual.debug_bounds("chat-question-rail").unwrap();
    let scroll = visual.debug_bounds("chat-messages-scroll").unwrap();
    let composer = visual.debug_bounds("chat-composer").unwrap();
    assert_eq!(rail.size.width, px(26.0));
    assert!(rail.right() <= scroll.left(), "{rail:?} covers {scroll:?}");
    assert!(rail.top() >= scroll.top() && rail.bottom() <= scroll.bottom());
    // A gutter of the same width on the other side keeps the conversation over the composer.
    let offset = (composer.center().x - scroll.center().x).abs();
    assert!(offset <= px(2.0), "conversation off centre by {offset:?}");
    // One marker per question, top to bottom, inside the rail.
    let mut previous = rail.top();
    for marker in [
        "chat-question:0",
        "chat-question:1",
        "chat-question:2",
        "chat-question:3",
        "chat-question:4",
        "chat-question:5",
    ] {
        let bounds = visual
            .debug_bounds(marker)
            .unwrap_or_else(|| panic!("missing {marker}"));
        assert!(
            bounds.left() >= rail.left()
                && bounds.right() <= rail.right()
                && bounds.top() >= previous
                && bounds.bottom() <= rail.bottom(),
            "{marker} {bounds:?} leaves the rail {rail:?}"
        );
        previous = bounds.bottom();
    }
    assert!(visual.debug_bounds("chat-question:6").is_none());
    let active = |model: &Entity<AppModel>, visual: &mut VisualTestContext| {
        model.read_with(visual, |m, _| {
            m.chat_question_layout
                .borrow()
                .active(-m.chat_scroll.offset().y)
        })
    };
    assert_eq!(active(&model, visual), Some(0));

    // A click brings the question under the top of the conversation and makes it the active one.
    click(visual, "chat-question:2");
    let landed = message_offset(visual, "chat-message:message-4");
    assert!((landed - px(24.0)).abs() <= px(1.0), "landed at {landed:?}");
    assert_eq!(active(&model, visual), Some(2));
    // Reaching a marker with the keyboard previews its exchange under it, without a wait.
    visual.simulate_keystrokes("shift-tab");
    visual.simulate_keystrokes("tab");
    visual.run_until_parked();
    assert!(visual.update(|window, _| window.focus_visible()));
    let focused = visual.debug_bounds("chat-question:2").unwrap();
    let keyboard_preview = visual
        .debug_bounds("chat-question-preview")
        .expect("the preview must follow keyboard focus");
    assert!(
        keyboard_preview.top() >= focused.bottom() && keyboard_preview.size.width <= px(320.0),
        "{keyboard_preview:?} is not under {focused:?}"
    );
    // Alt+Down and Alt+Up walk the questions from wherever the focus is.
    visual.simulate_keystrokes("alt-down");
    visual.run_until_parked();
    let landed = message_offset(visual, "chat-message:message-6");
    assert!((landed - px(24.0)).abs() <= px(1.0), "landed at {landed:?}");
    assert_eq!(active(&model, visual), Some(3));
    visual.simulate_keystrokes("alt-up alt-up");
    visual.run_until_parked();
    let landed = message_offset(visual, "chat-message:message-2");
    assert!((landed - px(24.0)).abs() <= px(1.0), "landed at {landed:?}");
    assert_eq!(active(&model, visual), Some(1));
    // The same keys work while the developer is typing in the composer.
    click(visual, "native-input:Message to Companion");
    visual.simulate_keystrokes("alt-down");
    visual.run_until_parked();
    let landed = message_offset(visual, "chat-message:message-4");
    assert!((landed - px(24.0)).abs() <= px(1.0), "landed at {landed:?}");
    assert_eq!(active(&model, visual), Some(2));
    model.read_with(visual, |m, cx| {
        assert_eq!(m.chat_draft_input.read(cx).value(), "");
    });
    // The last question cannot reach the top: the conversation stops at its end, question in view.
    click(visual, "chat-question:5");
    let scroll = visual.debug_bounds("chat-messages-scroll").unwrap();
    let last = visual.debug_bounds("chat-message:message-10").unwrap();
    assert!(
        last.top() >= scroll.top() && last.top() < scroll.bottom(),
        "{last:?} is outside {scroll:?}"
    );
    assert_eq!(active(&model, visual), Some(5));
    visual.simulate_keystrokes("alt-down");
    visual.run_until_parked();
    assert_eq!(active(&model, visual), Some(5));

    // Resting on a marker previews the question and the start of its answer.
    let marker = visual.debug_bounds("chat-question:1").unwrap();
    visual.simulate_mouse_move(marker.center(), None, Modifiers::default());
    visual
        .executor()
        .advance_clock(std::time::Duration::from_millis(600));
    visual.run_until_parked();
    let hover_preview = visual
        .debug_bounds("chat-question-preview")
        .expect("the preview must appear on hover");
    assert_ne!(
        hover_preview, keyboard_preview,
        "the hover opened no preview"
    );
}

#[gpui::test]
fn companion_question_rail_reaches_the_history_that_is_not_rendered(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    select(&model, visual, AppView::Chat);
    // Sixty messages: the first twenty, ten questions, are outside the rendered window.
    open_conversation_of(&model, visual, 30, 4);
    assert!(visual.debug_bounds("chat-message:message-0").is_none());
    assert!(visual.debug_bounds("chat-message:message-20").is_some());
    let rail = visual.debug_bounds("chat-question-rail").unwrap();
    let first = visual.debug_bounds("chat-question:0").unwrap();
    let last = visual.debug_bounds("chat-question:29").unwrap();
    // Thirty markers still fit the rail, each one tall enough to point at.
    assert!(first.top() >= rail.top() && last.bottom() <= rail.bottom());
    assert!(first.size.height >= px(3.0), "{first:?}");
    // Markers count the whole conversation: the rendered window opens on the eleventh question.
    model.read_with(visual, |m, _| {
        assert_eq!(
            m.chat_question_layout
                .borrow()
                .active(-m.chat_scroll.offset().y),
            Some(10)
        );
    });
    click(visual, "chat-question:3");
    model.read_with(visual, |m, cx| {
        assert_eq!(
            m.chat
                .read(cx)
                .history_expanded
                .as_ref()
                .map(ToString::to_string)
                .as_deref(),
            Some("conversation-1")
        );
    });
    let landed = message_offset(visual, "chat-message:message-6");
    assert!((landed - px(24.0)).abs() <= px(1.0), "landed at {landed:?}");
}

#[gpui::test]
fn companion_composer_offers_the_effort_of_the_model_as_chips(cx: &mut TestAppContext) {
    use ikore_core::chat_service::{ChatConversation, ChatTransportKind};
    use ikore_core::terminals::{
        CliModelCatalogEntry, CliModelCatalogSnapshot, CliModelCatalogSource, CliModelCatalogState,
    };
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    select(&model, visual, AppView::Chat);
    model.update(visual, |m, cx| {
        m.agents.update(cx, |agents, _| {
            agents.model_catalogs.insert(
                CliBackend::Codex,
                CliModelCatalogSnapshot {
                    project_id: "layout-project".into(),
                    backend: CliBackend::Codex,
                    adapter_contract_version: 2,
                    state: CliModelCatalogState::Ready,
                    source: CliModelCatalogSource::Cli,
                    cli_version: None,
                    diagnostic: None,
                    refreshed_at: "now".into(),
                    models: vec![CliModelCatalogEntry {
                        provider_id: Some("openai".into()),
                        model_id: Some("gpt-5".into()),
                        display_name: "GPT-5".into(),
                        context_window: None,
                        input_cost_usd_per_million: None,
                        output_cost_usd_per_million: None,
                        capabilities: vec![],
                        profiles: vec!["low".into(), "medium".into(), "high".into()],
                        raw_metadata: Default::default(),
                    }],
                },
            );
        });
        m.chat.update(cx, |chat, _| {
            chat.project_id = Some("layout-project".into());
            chat.conversations = vec![ChatConversation {
                id: "conversation-1".into(),
                project_id: "layout-project".into(),
                connection_id: None,
                provider_id: "codex".into(),
                model_id: "gpt-5".into(),
                transport: ChatTransportKind::Cli,
                cli_backend: Some(CliBackend::Codex),
                model_profile: Some("medium".into()),
                model_source: CliModelSelectionSource::Catalog,
                title: "Effort".into(),
                connection_available: true,
                created_at: "now".into(),
                updated_at: "now".into(),
            }];
            chat.active_conversation = Some("conversation-1".into());
            chat.sync_panel_with_active_conversation();
        });
        cx.notify();
    });
    visual.run_until_parked();
    // The levels sit side by side in the composer's target row, `Default` first.
    let target = visual.debug_bounds("chat-composer-target").unwrap();
    let track = visual.debug_bounds("chat-composer-profile").unwrap();
    assert!(
        track.left() >= target.left()
            && track.right() <= target.right()
            && track.top() >= target.top()
            && track.bottom() <= target.bottom(),
        "{track:?} leaves {target:?}"
    );
    let mut previous = track.left();
    for chip in [
        "chat-composer-profile:",
        "chat-composer-profile:low",
        "chat-composer-profile:medium",
        "chat-composer-profile:high",
    ] {
        let bounds = visual
            .debug_bounds(chip)
            .unwrap_or_else(|| panic!("missing {chip}"));
        assert!(
            bounds.left() >= previous
                && bounds.right() <= track.right()
                && bounds.top() >= track.top()
                && bounds.bottom() <= track.bottom(),
            "{chip} {bounds:?} leaves {track:?}"
        );
        previous = bounds.right();
    }
    // The chips are no taller than the buttons beside them, and the row stays on one line.
    let model_control = visual.debug_bounds("chat-composer-model").unwrap();
    let route = visual.debug_bounds("chat-composer-route").unwrap();
    assert!(track.size.height <= model_control.size.height, "{track:?}");
    assert!(
        model_control.right() <= track.left() && track.right() <= route.left(),
        "{model_control:?} {track:?} {route:?}"
    );
    assert!(route.top() < model_control.bottom(), "the route wrapped");
    // One click changes the effort of the next message.
    click(visual, "chat-composer-profile:high");
    model.read_with(visual, |m, cx| {
        assert_eq!(
            m.chat.read(cx).selected_cli_model_profile.as_deref(),
            Some("high")
        );
    });
    click(visual, "chat-composer-profile:");
    model.read_with(visual, |m, cx| {
        assert_eq!(m.chat.read(cx).selected_cli_model_profile, None);
    });
}

#[gpui::test]
fn projects_sidebar_lists_every_project_beside_the_rail(cx: &mut TestAppContext) {
    use ikore_core::persistence_service::{
        Project, ProjectSessions, SessionEntry, SessionKind, SessionState,
    };
    let project = |id: &str, name: &str| Project {
        id: id.into(),
        name: name.into(),
        workspace_path: format!("C:/work/{id}"),
        is_archived: false,
        created_at: "now".into(),
        updated_at: "now".into(),
        last_opened_at: None,
    };
    let session = |kind, id: &str, detail: &str, state| SessionEntry {
        kind,
        id: id.into(),
        title: format!("Session {id}"),
        detail: detail.into(),
        state,
        pinned: id == "chat-a",
        updated_at: "now".into(),
    };
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.update(|_, cx| crate::shell::register_navigation_keybindings(cx));
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    select(&model, visual, AppView::Agents);
    assert!(visual.debug_bounds("projects-sidebar").is_none());
    let rail = visual.debug_bounds("rail").unwrap();
    assert_eq!(
        visual.debug_bounds("shell-center").unwrap().left(),
        rail.right()
    );
    assert_eq!(
        visual.debug_bounds("context-rail").unwrap().size.width,
        px(256.0)
    );

    model.update(visual, |m, cx| {
        m.projects_sidebar.projects = vec![
            ProjectSessions {
                project: project("layout-project", "Open project"),
                sessions: vec![
                    session(
                        SessionKind::Pulsation,
                        "pulse-a",
                        "code",
                        SessionState::Running,
                    ),
                    session(
                        SessionKind::Feature,
                        "feature-a",
                        "incubating",
                        SessionState::Idle,
                    ),
                    session(
                        SessionKind::Conversation,
                        "chat-a",
                        "gpt-5",
                        SessionState::Idle,
                    ),
                ],
                state: SessionState::Running,
            },
            ProjectSessions {
                project: project("other-project", "Other project"),
                sessions: vec![session(
                    SessionKind::Pulsation,
                    "pulse-z",
                    "test",
                    SessionState::Waiting,
                )],
                state: SessionState::Waiting,
            },
        ];
        cx.notify();
    });
    click(visual, "rail-projects");
    // The sidebar opens between the rail and the centre, which keeps its minimum width.
    let sidebar = visual.debug_bounds("projects-sidebar").unwrap();
    let center = visual.debug_bounds("shell-center").unwrap();
    assert_eq!(sidebar.size.width, px(240.0));
    assert_eq!(sidebar.left(), rail.right());
    assert_eq!(center.left(), sidebar.right());
    assert!(center.size.width >= px(660.0), "{center:?}");
    // This window has room for both side panels.
    assert_eq!(
        visual.debug_bounds("context-rail").unwrap().size.width,
        px(256.0)
    );
    // The open project shows its work; the other one is folded, marked and can be opened.
    assert!(visual.debug_bounds("sidebar-session:pulse-a").is_some());
    assert!(visual.debug_bounds("sidebar-session:pulse-z").is_none());
    assert!(
        visual
            .debug_bounds("sidebar-project-state:other-project")
            .is_some()
    );
    assert!(visual.debug_bounds("sidebar-open:other-project").is_some());
    assert!(visual.debug_bounds("sidebar-open:layout-project").is_none());
    // A pinned conversation says so on its row.
    let pin = visual.debug_bounds("sidebar-pinned:chat-a").unwrap();
    let pinned_row = visual.debug_bounds("sidebar-session:chat-a").unwrap();
    assert!(pin.left() >= pinned_row.left() && pin.right() <= pinned_row.right());
    click(visual, "sidebar-project:other-project");
    for row in [
        "sidebar-project:layout-project",
        "sidebar-session:pulse-a",
        "sidebar-session:chat-a",
        "sidebar-project:other-project",
        "sidebar-open:other-project",
        "sidebar-session:pulse-z",
    ] {
        let bounds = visual
            .debug_bounds(row)
            .unwrap_or_else(|| panic!("missing {row}"));
        assert!(
            bounds.left() >= sidebar.left() && bounds.right() <= sidebar.right(),
            "{row} {bounds:?} leaves {sidebar:?}"
        );
    }
    // A session of the open project goes to its view.
    click(visual, "sidebar-session:chat-a");
    assert_eq!(
        model.read_with(visual, |m, _| m.active_view()),
        AppView::Chat
    );
    click(visual, "sidebar-session:feature-a");
    assert_eq!(
        model.read_with(visual, |m, _| (
            m.active_view(),
            m.forge.selected_feature.clone()
        )),
        (AppView::Forge, Some("feature-a".to_string()))
    );
    click(visual, "sidebar-session:pulse-a");
    assert_eq!(
        model.read_with(visual, |m, _| m.active_view()),
        AppView::Pulsations
    );
    // In a narrower window the context rail folds first, and the sidebar stays.
    visual.simulate_resize(size(px(1280.0), px(800.0)));
    visual.run_until_parked();
    assert_eq!(
        visual.debug_bounds("context-rail").unwrap().size.width,
        px(48.0)
    );
    let sidebar = visual.debug_bounds("projects-sidebar").unwrap();
    let center = visual.debug_bounds("shell-center").unwrap();
    assert_eq!(center.left(), sidebar.right());
    assert!(center.size.width >= px(660.0), "{center:?}");
    // At the minimum window the sidebar gives its room back to the centre.
    visual.simulate_resize(size(px(960.0), px(640.0)));
    visual.run_until_parked();
    let rail = visual.debug_bounds("rail").unwrap();
    let center = visual.debug_bounds("shell-center").unwrap();
    assert_eq!(center.left(), rail.right());
    assert!(model.read_with(visual, |m, _| m.projects_sidebar.open));
    // The row that had the focus is gone: the rail button took it, so the shortcut still
    // reaches the shell and closes what the button opened.
    assert!(visual.update(|window, cx| window.focused(cx).is_some()));
    visual.simulate_keystrokes("ctrl-shift-e");
    visual.run_until_parked();
    assert!(!model.read_with(visual, |m, _| m.projects_sidebar.open));
    // Closed from inside the sidebar, the same shortcut opens it again.
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    visual.simulate_keystrokes("ctrl-shift-e");
    visual.run_until_parked();
    click(visual, "sidebar-session:chat-a");
    // GPUI keeps the bounds of a selector that is no longer drawn, so the centre tells where
    // the sidebar is.
    let center_left = |visual: &mut VisualTestContext| {
        visual.debug_bounds("shell-center").unwrap().left()
            - visual.debug_bounds("rail").unwrap().right()
    };
    assert_eq!(center_left(visual), px(240.0));
    visual.simulate_keystrokes("ctrl-shift-e");
    visual.run_until_parked();
    assert_eq!(center_left(visual), px(0.0));
    visual.simulate_keystrokes("ctrl-shift-e");
    visual.run_until_parked();
    assert_eq!(center_left(visual), px(240.0));
}

#[gpui::test]
fn work_panel_tabs_reach_the_project_files_from_any_view(cx: &mut TestAppContext) {
    use gpui::Focusable as _;
    use ikore_core::workspace_service::{FileNode, FileNodeKind};
    use std::sync::Arc;
    let file = |path: &str| FileNode {
        name: path.rsplit('/').next().unwrap().into(),
        relative_path: path.into(),
        kind: FileNodeKind::File,
        children: Vec::new(),
    };
    let backend = Arc::new(crate::work_panel::tests::SearchOnly::default());
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.update(|_, cx| crate::shell::register_navigation_keybindings(cx));
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    select(&model, visual, AppView::Agents);
    model.update(visual, |m, cx| {
        m.vault_backend = Some(backend.clone());
        m.workspace.update(cx, |workspace, _| {
            workspace.root_path = Some("C:/work/layout".into());
            workspace.tree = vec![
                FileNode {
                    name: "docs".into(),
                    relative_path: "docs".into(),
                    kind: FileNodeKind::Directory,
                    children: vec![file("docs/a.md")],
                },
                file("README.md"),
            ];
            workspace.open_tab_evicting("src/beta.rs", "fn main() {}\n".into(), "r1".into());
        });
        cx.notify();
    });
    visual.run_until_parked();
    // General is the panel as it was: the context cards, under the tabs.
    let panel = visual.debug_bounds("context-rail").unwrap();
    assert_eq!(panel.size.width, crate::sizing::CONTEXT_RAIL_WIDTH);
    let toggle = visual.debug_bounds("context-rail-toggle").unwrap();
    for tab in ["work-tab:general", "work-tab:files", "work-tab:search"] {
        let bounds = visual.debug_bounds(tab).unwrap();
        assert!(
            bounds.left() >= panel.left() && bounds.right() <= toggle.left(),
            "{tab} {bounds:?} overlaps the toggle {toggle:?}"
        );
    }
    let pipeline = visual.debug_bounds("rail-card-pipeline").unwrap();
    assert!(pipeline.top() >= toggle.bottom(), "{pipeline:?}");

    // Files: the project's tree, in the panel, and the view stays.
    click(visual, "work-tab:files");
    for row in [
        "work-file:docs",
        "work-file:docs/a.md",
        "work-file:README.md",
    ] {
        let bounds = visual
            .debug_bounds(row)
            .unwrap_or_else(|| panic!("missing {row}"));
        assert!(
            bounds.left() >= panel.left() && bounds.right() <= panel.right(),
            "{row} {bounds:?} leaves {panel:?}"
        );
    }
    assert_eq!(
        model.read_with(visual, |m, _| m.active_view()),
        AppView::Agents
    );
    // One tree: a folder folded here is folded in Workspace too.
    click(visual, "work-file:docs");
    assert!(model.read_with(visual, |m, cx| {
        m.workspace.read(cx).folders.toggled.contains("docs")
    }));

    // Search: the field takes the focus, Enter searches, and the matches stay in the panel.
    click(visual, "work-tab:search");
    let input = model.read_with(visual, |m, _| m.work_panel.search_input().unwrap());
    assert!(visual.update(|window, cx| input.read(cx).focus_handle(cx).is_focused(window)));
    model.update(visual, |_, cx| {
        input.update(cx, |input, cx| input.set_value("alpha beta", cx));
    });
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();
    assert_eq!(
        *backend.queries.lock().unwrap(),
        vec![("alpha beta".to_string(), 100)]
    );
    assert_eq!(
        input.read_with(visual, |input, _| input.value().to_owned()),
        "alpha beta",
        "Enter searches; it does not type into the field"
    );
    let field = visual.debug_bounds("work-search-field").unwrap();
    assert!(field.left() >= panel.left() && field.right() <= panel.right());
    for result in ["work-result:0", "work-result:1"] {
        let bounds = visual
            .debug_bounds(result)
            .unwrap_or_else(|| panic!("missing {result}"));
        assert!(
            bounds.left() >= panel.left() && bounds.right() <= panel.right(),
            "{result} {bounds:?} leaves {panel:?}"
        );
        assert!(bounds.top() > field.bottom());
    }
    // A match opens its file in Workspace.
    click(visual, "work-result:1");
    assert_eq!(
        model.read_with(visual, |m, cx| (
            m.active_view(),
            m.workspace
                .read(cx)
                .active_relative_path
                .as_ref()
                .map(ToString::to_string)
        )),
        (AppView::Workspace, Some("src/beta.rs".to_string()))
    );

    // Folded, the strip opens the panel on a tab, and the focus stays in the shell: the
    // shortcut that folded the panel unfolds it again.
    visual.simulate_keystrokes("ctrl-b");
    visual.run_until_parked();
    assert!(model.read_with(visual, |m, cx| m.context_rail_collapsed(cx)));
    click(visual, "rail-strip-files");
    assert_eq!(
        model.read_with(visual, |m, cx| (
            m.context_rail_collapsed(cx),
            m.work_panel.tab
        )),
        (false, crate::work_panel::WorkPanelTab::Files)
    );
    assert!(visual.update(|window, cx| window.focused(cx).is_some()));
    visual.simulate_keystrokes("ctrl-b");
    visual.run_until_parked();
    visual.simulate_keystrokes("ctrl-b");
    visual.run_until_parked();
    assert!(!model.read_with(visual, |m, cx| m.context_rail_collapsed(cx)));
    // The search shortcut opens the Search tab, from Home too, with the field focused.
    click(visual, "rail-nav-home");
    visual.simulate_keystrokes("ctrl-shift-f");
    visual.run_until_parked();
    assert_eq!(
        model.read_with(visual, |m, _| (m.active_view(), m.work_panel.tab)),
        (AppView::Workspace, crate::work_panel::WorkPanelTab::Search)
    );
    assert!(visual.update(|window, cx| input.read(cx).focus_handle(cx).is_focused(window)));
    // A window too narrow for the panel says so instead of opening it.
    visual.simulate_resize(size(px(960.0), px(640.0)));
    visual.run_until_parked();
    model.update(visual, |m, cx| {
        m.work_panel.tab = crate::work_panel::WorkPanelTab::General;
        cx.notify();
    });
    visual.simulate_keystrokes("ctrl-shift-f");
    visual.run_until_parked();
    assert_eq!(
        model.read_with(visual, |m, _| m.work_panel.tab),
        crate::work_panel::WorkPanelTab::General
    );
    assert!(
        model
            .read_with(visual, |m, _| m
                .current_notice()
                .map(|notice| notice.message.clone()))
            .is_some_and(|message| message.contains("Widen the window"))
    );
}

#[gpui::test]
fn settings_models_page_lists_each_model_once_with_its_routes(cx: &mut TestAppContext) {
    use ikore_core::llm::ProviderConnection;
    use ikore_core::terminals::{
        CliModelCatalogEntry, CliModelCatalogSnapshot, CliModelCatalogSource, CliModelCatalogState,
    };
    let connection = |id: &str, provider: &str, name: &str, model: &str| ProviderConnection {
        id: id.into(),
        provider_id: provider.into(),
        display_name: name.into(),
        endpoint_url: None,
        default_model: model.into(),
        enabled: true,
        state: "active".into(),
        blocked_reason: None,
        cooldown_until: None,
        max_concurrency: 1,
        version: 1,
        has_api_key: true,
        chat_eligible: true,
        created_at: "now".into(),
        updated_at: "now".into(),
    };
    let listed = |id: &str, profiles: &[&str]| CliModelCatalogEntry {
        provider_id: Some("openai".into()),
        model_id: Some(id.into()),
        display_name: id.into(),
        context_window: Some(400_000),
        input_cost_usd_per_million: None,
        output_cost_usd_per_million: None,
        capabilities: vec![],
        profiles: profiles.iter().map(ToString::to_string).collect(),
        raw_metadata: Default::default(),
    };
    let (model, visual) = cx.add_window_view(|_, cx| {
        let model = fixture(cx);
        model.settings.update(cx, |settings, _| {
            settings.snapshot.as_mut().unwrap().connections = vec![
                connection("key", "openai", "OpenAI key", "gpt-5"),
                connection(
                    "router",
                    "openrouter",
                    "OpenRouter",
                    "anthropic/claude-sonnet-4-5",
                ),
            ];
        });
        model.agents.update(cx, |agents, _| {
            agents.model_catalogs.insert(
                CliBackend::Codex,
                CliModelCatalogSnapshot {
                    project_id: "layout-project".into(),
                    backend: CliBackend::Codex,
                    adapter_contract_version: 2,
                    state: CliModelCatalogState::Ready,
                    source: CliModelCatalogSource::Cli,
                    cli_version: None,
                    diagnostic: None,
                    refreshed_at: "now".into(),
                    models: vec![
                        listed("gpt-5", &["low", "high"]),
                        listed("gpt-5-codex", &[]),
                    ],
                },
            );
        });
        model
    });
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    open_settings(&model, visual, Some(SettingsPage::Models));
    let sources = visual.debug_bounds("settings-section:Sources").unwrap();
    assert!(visual.debug_bounds("settings-row:models-sources").is_some());
    assert!(
        visual
            .debug_bounds("settings-refresh-cli-catalogs")
            .is_some()
    );
    // One section per vendor, in a fixed order, under the sources.
    let openai = visual.debug_bounds("settings-section:OpenAI").unwrap();
    let anthropic = visual.debug_bounds("settings-section:Anthropic").unwrap();
    assert!(sources.bottom() <= openai.top() && openai.bottom() <= anthropic.top());
    // `gpt-5` is one row, though the Codex CLI and the OpenAI key both list it.
    let shared = visual
        .debug_bounds("settings-row:model:openai:gpt-5")
        .unwrap();
    let cli_only = visual
        .debug_bounds("settings-row:model:openai:gpt-5-codex")
        .unwrap();
    for row in [shared, cli_only] {
        // Half a pixel of rounding separates a last row from the end of its section.
        assert!(
            row.top() >= openai.top() && row.bottom() <= openai.bottom() + px(1.0),
            "{row:?} leaves {openai:?}"
        );
        assert!(row.size.width <= px(720.0), "{row:?}");
    }
    // A gateway's namespace is not part of the model: this one sits under its vendor.
    let through_gateway = visual
        .debug_bounds("settings-row:model:anthropic:claude-sonnet-4-5")
        .unwrap();
    assert!(
        through_gateway.top() >= anthropic.top()
            && through_gateway.bottom() <= anthropic.bottom() + px(1.0)
    );
    assert!(
        visual
            .debug_bounds("settings-row:model:other:anthropic/claude-sonnet-4-5")
            .is_none()
    );
}

#[gpui::test]
fn a_keyboard_focus_ring_never_resizes_its_control(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.update(|_, cx| crate::shell::register_navigation_keybindings(cx));
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    select(&model, visual, AppView::Forge);
    // A click focuses the tab without a ring; Shift-Tab then Tab bring the focus back to it
    // from the keyboard, ring on.
    click(visual, "forge-section-triage");
    let tab = visual.debug_bounds("forge-section-triage").unwrap();
    let tip = visual.debug_bounds("info:forge-section").unwrap();
    visual.simulate_keystrokes("shift-tab");
    visual.simulate_keystrokes("tab");
    assert!(visual.update(|window, _| window.focus_visible()));
    assert_eq!(visual.debug_bounds("forge-section-triage").unwrap(), tab);
    assert_eq!(visual.debug_bounds("info:forge-section").unwrap(), tip);
}

// ---------------------------------------------------------------------------
// ADR-027 M5: the Visualizer's note on the Brief
// ---------------------------------------------------------------------------

fn visualizer_window(
    cx: &mut TestAppContext,
    visualization: ikore_core::orchestration::review_brief::CheckpointVisualization,
) -> (Entity<AppModel>, &mut VisualTestContext) {
    let (model, visual) = open_specification_brief(cx);
    model.update(visual, |model, cx| {
        model.pulsations.update(cx, |state, _| {
            state.hitl.visualization = Some(visualization);
        });
    });
    (model, visual)
}

#[gpui::test]
fn the_visualizer_note_sits_under_the_decision_line_and_marks_its_emphasis(
    cx: &mut TestAppContext,
) {
    use ikore_core::orchestration::review_brief::{
        CheckpointVisualization, ResolvedNote, ResolvedVisualization, VisualizationState,
    };
    let note = CheckpointVisualization {
        state: VisualizationState::Validated,
        note: Some(ResolvedVisualization {
            headline: "AC-4 rests on reading alone; confirm the header-only early return before approving.".into(),
            notes: vec![
                ResolvedNote {
                    text: "The empty-range criterion is proven only by reading write_usage_csv; no host check executes it.".into(),
                    refs: vec!["AC-4".into(), "crates/ikore-core/src/usage/export.rs".into()],
                },
                ResolvedNote {
                    text: "The Usage view gains an Export button, the only interface you already own that changes.".into(),
                    refs: vec!["crates/ikore-ui/src/usage.rs".into()],
                },
            ],
            emphasis: vec![1],
        }),
        diagnostic: None,
    };
    let (model, visual) = visualizer_window(cx, note);
    for width in [960.0, 1440.0] {
        visual.simulate_resize(size(px(width), px(2400.0)));
        select(&model, visual, AppView::Pulsations);
        let reader = visual.debug_bounds("review-document-reader").unwrap();
        let decision = visual.debug_bounds("brief-decision").unwrap();
        let card = visual.debug_bounds("brief-visualizer").unwrap();
        let judgement = visual.debug_bounds("brief-judgement").unwrap();
        assert!(
            decision.bottom() <= card.top() && card.bottom() <= judgement.top(),
            "the note sits between the decision line and the judgement at {width}"
        );
        assert!(
            card.left() >= reader.left() && card.right() <= reader.right() + px(1.0),
            "the note overflows the reader at {width}: {card:?} vs {reader:?}"
        );
        for selector in [
            "brief-visualizer-headline",
            "brief-visualizer-note-0",
            "brief-visualizer-note-1",
        ] {
            let bounds = visual.debug_bounds(selector).unwrap();
            assert!(
                bounds.left() >= card.left() && bounds.right() <= card.right() + px(1.0),
                "{selector} overflows at {width}"
            );
        }
        // The emphasis badge is inside the item it points at, and only there.
        let item = visual.debug_bounds("brief-item-1").unwrap();
        let badge = visual.debug_bounds("brief-emphasis-1").unwrap();
        assert!(badge.top() >= item.top() && badge.bottom() <= item.bottom());
        assert!(badge.right() <= item.right() + px(1.0));
    }
    assert!(visual.debug_bounds("brief-emphasis-0").is_none());
    model.read_with(visual, |model, cx| {
        assert!(model.pulsations.read(cx).hitl.can_approve());
    });
}

#[gpui::test]
fn a_pending_or_omitted_visualizer_note_is_one_quiet_line(cx: &mut TestAppContext) {
    use ikore_core::orchestration::review_brief::{CheckpointVisualization, VisualizationState};
    let (model, visual) = visualizer_window(
        cx,
        CheckpointVisualization {
            state: VisualizationState::Pending,
            note: None,
            diagnostic: None,
        },
    );
    visual.simulate_resize(size(px(1440.0), px(1800.0)));
    select(&model, visual, AppView::Pulsations);
    let line = visual.debug_bounds("brief-visualizer-pending").unwrap();
    assert!(line.size.height <= px(40.0), "{line:?}");
    assert!(visual.debug_bounds("brief-visualizer").is_none());

    let (model, visual) = visualizer_window(
        cx,
        CheckpointVisualization {
            state: VisualizationState::Omitted,
            note: None,
            diagnostic: Some("The Visualizer's answer was rejected twice: Note 1 cites `R99`, which is not in the input.".into()),
        },
    );
    visual.simulate_resize(size(px(960.0), px(1800.0)));
    select(&model, visual, AppView::Pulsations);
    let reader = visual.debug_bounds("review-document-reader").unwrap();
    let line = visual.debug_bounds("brief-visualizer-omitted").unwrap();
    assert!(line.left() >= reader.left() && line.right() <= reader.right() + px(1.0));
    assert!(visual.debug_bounds("brief-visualizer").is_none());
    assert!(visual.debug_bounds("brief-visualizer-pending").is_none());
}

// ---------------------------------------------------------------------------
// ADR-029: the Notes view
// ---------------------------------------------------------------------------

fn layout_note(
    id: &str,
    status: ikore_core::companion_notes::NoteStatus,
) -> ikore_core::companion_notes::CompanionNote {
    use ikore_core::companion_notes::{CompanionNote, NoteSource, NoteStatus, NoteTag};
    let ready = status == NoteStatus::Ready;
    CompanionNote {
        id: id.into(),
        project_id: "layout-project".into(),
        revision: if ready { 2 } else { 1 },
        status,
        source_type: NoteSource::Text,
        raw_input: format!("Loose thought {id}: the save button disappears in dark mode"),
        raw_redactions: 1,
        title: ready.then(|| format!("Save button contrast {id}")),
        summary: ready.then(|| "The save button is invisible on the dark theme.".into()),
        content_markdown: ready
            .then(|| "### Problem\n- Low contrast\n- Users miss the action".into()),
        tags: if ready {
            vec![NoteTag::UiUx, NoteTag::Bug]
        } else {
            vec![]
        },
        output_language: ready.then(|| "en".into()),
        created_at: "2026-10-22T12:00:00Z".into(),
        updated_at: "2026-10-22T12:00:00Z".into(),
    }
}

fn notes_fixture(
    model: &Entity<AppModel>,
    visual: &mut VisualTestContext,
    notes: Vec<ikore_core::companion_notes::CompanionNote>,
    selected: Option<&str>,
) {
    select(model, visual, AppView::Notes);
    model.update(visual, |m, cx| {
        m.notes.project_id = Some("layout-project".into());
        m.notes.snapshot.notes = notes;
        m.notes.selected = selected.map(Into::into);
        cx.notify();
    });
    visual.run_until_parked();
}

#[gpui::test]
fn notes_master_and_detail_fit_both_supported_window_sizes(cx: &mut TestAppContext) {
    use ikore_core::companion_notes::NoteStatus;
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    for (width, height) in [(960.0, 640.0), (1440.0, 900.0)] {
        visual.simulate_resize(size(px(width), px(height)));
        notes_fixture(
            &model,
            visual,
            vec![
                layout_note("n1", NoteStatus::Ready),
                layout_note("n2", NoteStatus::Structuring),
            ],
            Some("n1"),
        );
        let left = visual.debug_bounds("notes-columns-left").unwrap();
        let right = visual.debug_bounds("notes-columns-right").unwrap();
        let rail = visual.debug_bounds("rail").unwrap();
        let status_bar = visual.debug_bounds("status-bar").unwrap();
        assert!(left.left() >= rail.right() && left.right() <= right.left());
        assert!(
            right.right() <= px(width),
            "detail overflows at {width}: {right:?}"
        );
        assert!(
            right.size.width >= px(359.0),
            "the detail column is squeezed at {width}: {right:?}"
        );
        for selector in [
            "notes-capture",
            "notes-capture-save",
            "notes-search",
            "notes-tag-filter",
            "notes-list",
            "note-n1",
            "note-n2",
        ] {
            let bounds = visual
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("missing {selector} at {width}"));
            assert!(
                bounds.left() >= left.left() && bounds.right() <= left.right() + px(1.0),
                "{selector} escaped the list column at {width}: {bounds:?}"
            );
        }
        for selector in [
            "notes-detail",
            "note-title",
            "note-summary",
            "note-content",
            "note-agent-label",
            "note-raw-toggle",
            "note-delete",
            "notes-refine",
            "notes-refine-send",
        ] {
            let bounds = visual
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("missing {selector} at {width}"));
            assert!(
                bounds.left() >= right.left() && bounds.right() <= right.right() + px(1.0),
                "{selector} escaped the detail column at {width}: {bounds:?}"
            );
        }
        let composer = visual.debug_bounds("notes-refine").unwrap();
        assert!(
            composer.bottom() <= status_bar.top(),
            "the refinement composer is pushed off screen at {width}: {composer:?}, detail {:?}, pane {right:?}, list column {left:?}",
            visual.debug_bounds("notes-detail")
        );
    }
}

#[gpui::test]
fn notes_empty_states_guide_the_first_capture_and_clear_filters(cx: &mut TestAppContext) {
    use ikore_core::companion_notes::{NoteStatus, NoteTag};
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    notes_fixture(&model, visual, vec![], None);
    assert!(visual.debug_bounds("notes-empty").is_some());
    assert!(visual.debug_bounds("notes-detail").is_some());

    notes_fixture(
        &model,
        visual,
        vec![layout_note("n1", NoteStatus::Ready)],
        None,
    );
    model.update(visual, |m, cx| {
        m.notes.tag_filter.insert(NoteTag::Performance);
        cx.notify();
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("notes-no-matches").is_some());
    click(visual, "notes-clear-filters");
    assert!(model.read_with(visual, |m, _| m.notes.tag_filter.is_empty()));
    assert!(visual.debug_bounds("note-n1").is_some());

    click(visual, "notes-tag-ui_ux");
    assert!(model.read_with(visual, |m, _| m.notes.tag_filter.contains(&NoteTag::UiUx)));
    assert!(
        visual.debug_bounds("note-n1").is_some(),
        "the note carries the tag"
    );
    // Voice capture is a later phase: no source filter until a spoken note exists. It never
    // rendered in this window, so its selector cannot be a stale one.
    assert!(!model.read_with(visual, |m, _| m.notes.has_voice_notes()));
    assert!(visual.debug_bounds("notes-source-all").is_none());
}

#[gpui::test]
fn a_note_card_selects_the_note_and_the_drawer_shows_the_raw_text(cx: &mut TestAppContext) {
    use ikore_core::companion_notes::NoteStatus;
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    notes_fixture(
        &model,
        visual,
        vec![
            layout_note("n1", NoteStatus::Ready),
            layout_note("n2", NoteStatus::Structuring),
        ],
        None,
    );
    assert!(
        visual.debug_bounds("Structuring").is_some(),
        "the pending note says so"
    );
    click(visual, "note-n1");
    assert_eq!(
        model.read_with(visual, |m, _| m.notes.selected.clone()),
        Some("n1".into())
    );
    assert!(visual.debug_bounds("note-summary").is_some());
    click(visual, "note-raw-toggle");
    assert!(model.read_with(visual, |m, _| m.notes.raw_open));
    assert!(visual.debug_bounds("note-raw").is_some());
}

#[gpui::test]
fn a_running_note_offers_cancel_and_holds_the_composer(cx: &mut TestAppContext) {
    use ikore_core::companion_notes::NoteStatus;
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(960.0), px(640.0)));
    notes_fixture(
        &model,
        visual,
        vec![layout_note("n1", NoteStatus::Ready)],
        Some("n1"),
    );
    model.update(visual, |m, cx| {
        m.notes
            .snapshot
            .running
            .insert("n1".into(), "turn-1".into());
        m.notes
            .refine
            .update(cx, |input, cx| input.set_value("Shorter", cx));
        cx.notify();
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("note-working").is_some());
    assert!(visual.debug_bounds("note-cancel").is_some());
    assert_eq!(
        model.read_with(visual, |m, cx| m.sidebar_badges(cx).notes_running),
        1
    );
    // Send is disabled while the Scribe works; a click changes nothing.
    click(visual, "notes-refine-send");
    assert_eq!(
        model.read_with(visual, |m, cx| m.notes.refine.read(cx).value().to_string()),
        "Shorter"
    );
}

#[gpui::test]
fn a_failed_note_explains_itself_and_offers_to_structure_again(cx: &mut TestAppContext) {
    use ikore_core::companion_notes::{NoteStatus, NoteTurn, NoteTurnKind};
    use ikore_core::feature_forge::{TurnFailure, TurnFailureKind};
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    notes_fixture(
        &model,
        visual,
        vec![layout_note("n1", NoteStatus::Failed)],
        Some("n1"),
    );
    model.update(visual, |m, cx| {
        m.notes.history = vec![NoteTurn {
            id: "t1".into(),
            note_id: "n1".into(),
            note_revision: 1,
            ordinal: 1,
            kind: NoteTurnKind::Structure,
            status: "failed".into(),
            user_message: None,
            user_redactions: 0,
            assistant_message: None,
            failure: Some(TurnFailure::new(
                TurnFailureKind::ProviderSetup,
                "the provider refused the request",
            )),
            notices: vec![],
            calls: 1,
            created_at: "2026-10-22T12:00:00Z".into(),
        }];
        cx.notify();
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("note-failed").is_some());
    assert!(visual.debug_bounds("note-retry-structure").is_some());
    click(visual, "note-failure-detail");
    assert!(model.read_with(visual, |m, _| m.notes.expanded_failures.contains("t1")));
    // Refinement waits for a structured note.
    assert!(visual.debug_bounds("notes-refine").is_some());
    assert!(visual.debug_bounds("note-summary").is_none());
}

#[gpui::test]
fn deleting_a_note_takes_two_clicks_and_navigation_disarms_it(cx: &mut TestAppContext) {
    use ikore_core::companion_notes::NoteStatus;
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    notes_fixture(
        &model,
        visual,
        vec![layout_note("n1", NoteStatus::Ready)],
        Some("n1"),
    );
    let before = visual.debug_bounds("note-delete").unwrap();
    click(visual, "note-delete");
    assert_eq!(
        model.read_with(visual, |m, _| m.notes.confirm_delete.clone()),
        Some("n1".into())
    );
    let armed = visual.debug_bounds("note-delete").unwrap();
    assert_eq!(armed.top(), before.top(), "arming does not move the button");
    select(&model, visual, AppView::Chat);
    select(&model, visual, AppView::Notes);
    assert!(model.read_with(visual, |m, _| m.notes.confirm_delete.is_none()));
}

// ---------------------------------------------------------------------------
// ADR-029: voice dictation settings
// ---------------------------------------------------------------------------

fn voice_status(
    model_ready: bool,
    setup: Option<ikore_core::voice::SetupProgress>,
    push_to_talk: bool,
) -> ikore_core::voice::VoiceStatus {
    use ikore_core::voice::{Dictation, Hotkey, RuntimeStatus, VoiceSettings, VoiceStatus};
    VoiceStatus {
        runtime: RuntimeStatus {
            platform_supported: true,
            runtime_ready: model_ready,
            model_ready,
        },
        runtime_version: "0.2.0".into(),
        model_name: "Nemotron 3.5 ASR Streaming 0.6B".into(),
        model_bytes: 741_548_352,
        setup,
        setup_error: None,
        settings: VoiceSettings {
            push_to_talk,
            hotkey: Hotkey::default(),
            ..VoiceSettings::default()
        },
        listening: push_to_talk && model_ready,
        hotkey_error: None,
        dictation: Dictation::Idle,
    }
}

#[gpui::test]
fn the_voice_page_downloads_the_model_then_offers_push_to_talk(cx: &mut TestAppContext) {
    use ikore_core::voice::{SetupProgress, SetupStage};
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    for (width, height) in [(960.0, 640.0), (1440.0, 900.0)] {
        visual.simulate_resize(size(px(width), px(height)));
        model.update(visual, |m, cx| {
            m.voice_status = Some(voice_status(false, None, false));
            m.open_settings(Some(SettingsPage::Voice), cx);
        });
        visual.run_until_parked();
        let content = visual.debug_bounds("settings-content-scroll").unwrap();
        for selector in [
            "settings-row:voice-model",
            "voice-backend",
            "settings-row:voice-setup",
            "settings-voice-download",
            "settings-row:voice-push-to-talk",
            "settings-row:voice-shortcut",
            "settings-voice-status",
        ] {
            let bounds = visual
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("missing {selector} at {width}"));
            assert!(
                bounds.left() >= content.left() && bounds.right() <= content.right() + px(1.0),
                "{selector} escaped the settings pane at {width}: {bounds:?}"
            );
        }

        model.update(visual, |m, cx| {
            m.voice_status = Some(voice_status(
                false,
                Some(SetupProgress {
                    stage: SetupStage::Model,
                    received: 370_000_000,
                    total: 741_548_352,
                }),
                false,
            ));
            cx.notify();
        });
        visual.run_until_parked();
        assert!(visual.debug_bounds("settings-voice-cancel").is_some());

        model.update(visual, |m, cx| {
            m.voice_status = Some(voice_status(true, None, true));
            cx.notify();
        });
        visual.run_until_parked();
        assert!(visual.debug_bounds("settings-voice-push-to-talk").is_some());
        model.update(visual, |m, cx| m.close_settings(false, cx));
        visual.run_until_parked();
    }
}

#[gpui::test]
fn the_shortcut_menus_offer_every_modifier_set_and_f1_to_f10(cx: &mut TestAppContext) {
    let (model, _visual) = cx.add_window_view(|_, cx| fixture(cx));
    model.update(_visual, |m, _| {
        m.voice_status = Some(voice_status(true, None, true));
        let modifiers = m.voice_choices("voice-modifiers").unwrap();
        assert_eq!(modifiers.len(), 4);
        assert_eq!(
            modifiers
                .iter()
                .filter(|c| c.selected)
                .map(|c| c.value.to_string())
                .collect::<Vec<_>>(),
            ["ctrl+shift"]
        );
        let keys = m.voice_choices("voice-key").unwrap();
        assert_eq!(keys.len(), 10);
        assert_eq!(
            keys.first().map(|c| c.value.to_string()).as_deref(),
            Some("f1")
        );
        assert_eq!(
            keys.iter()
                .filter(|c| c.selected)
                .map(|c| c.value.to_string())
                .collect::<Vec<_>>(),
            ["f9"]
        );
        assert!(m.voice_choices("settings-context-strategy").is_none());
    });
}

#[gpui::test]
fn the_voice_model_menu_offers_both_backends_and_tracks_the_selection(cx: &mut TestAppContext) {
    use ikore_core::voice::VoiceBackend;
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    for backend in VoiceBackend::ALL {
        model.update(visual, |m, cx| {
            let mut status = voice_status(true, None, true);
            status.settings.backend = backend;
            if backend == VoiceBackend::WhisperLargeV3Turbo {
                status.model_name = "Whisper large-v3-turbo Q5_0 · Portuguese (pt)".into();
                status.model_bytes = 574_041_195;
                status.runtime_version = "1.9.5".into();
            }
            m.voice_status = Some(status);
            let choices = m.voice_choices("voice-backend").unwrap();
            assert_eq!(
                choices.iter().map(|c| c.value.as_str()).collect::<Vec<_>>(),
                ["nemotron", "whisper-large-v3-turbo"]
            );
            assert_eq!(choices[1].label, "Whisper large-v3-turbo Q5_0 · Portuguese");
            assert_eq!(
                choices
                    .iter()
                    .filter(|c| c.selected)
                    .map(|c| c.value.as_str())
                    .collect::<Vec<_>>(),
                [backend.id()]
            );
            m.open_settings(Some(SettingsPage::Voice), cx);
        });
        for (width, height) in [(960.0, 640.0), (1440.0, 900.0)] {
            visual.simulate_resize(size(px(width), px(height)));
            visual.run_until_parked();
            let content = visual.debug_bounds("settings-content-scroll").unwrap();
            let selector = visual.debug_bounds("voice-backend").unwrap();
            assert!(
                selector.left() >= content.left() && selector.right() <= content.right() + px(1.0),
                "{backend:?} model selector escaped the settings pane at {width}: {selector:?}"
            );
            assert!(visual.debug_bounds("settings-voice-download").is_none());
        }
    }
}

/// Parks the Integrator brief on the Review tab with the reviewer's recorded
/// inputs and the round count read with it (ADR-032).
fn open_integrator_review(
    cx: &mut TestAppContext,
    inputs: ikore_core::orchestration::composition::review_boundary::CheckpointReviewInputs,
    convergence: Option<ikore_core::persistence_service::ConvergenceStatus>,
    with_brief: bool,
) -> (Entity<AppModel>, &mut VisualTestContext) {
    use ikore_core::persistence_service::{HitlCheckpoint, HitlReviewDocument};
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.update(|_, cx| crate::hitl::register_hitl_keybindings(cx));
    model.update(visual, |m, cx| {
        m.pulsations.update(cx, |state, _| {
            state.pulsations[0].status = PulsationStatus::Checkpoint;
            let pulse = state.pulsations[0].clone();
            let checkpoint = HitlCheckpoint {
                pulsation_id: pulse.id.clone(),
                checkpoint_id: "integrator-checkpoint".into(),
                state_version: 1,
            };
            state.hitl.load_checkpoint(Some(&pulse), Some(checkpoint));
            let (generation, checkpoint) = state.hitl.begin_documents().unwrap();
            state.hitl.finish_documents(
                generation,
                &checkpoint,
                Ok(vec![HitlReviewDocument {
                    artifact_id: "integration-report".into(),
                    name: "integration report".into(),
                    kind: "integration_report".into(),
                    relative_path: None,
                    content: "# Integration report\n".into(),
                }]),
            );
            state.hitl.finish_brief(
                generation,
                &checkpoint,
                Ok(with_brief.then(integrator_brief)),
            );
            assert!(state.hitl.finish_review_context(
                generation,
                &checkpoint,
                Ok(Some(inputs)),
                Ok(convergence),
            ));
            state.detail_section = PulsationDetailSection::Review;
        });
    });
    (model, visual)
}

fn recorded_review_inputs()
-> ikore_core::orchestration::composition::review_boundary::CheckpointReviewInputs {
    let manifest = serde_json::from_value(serde_json::json!({
        "schemaVersion": 1,
        "toolScope": "host_context_only",
        "sanitizationPolicy": "hvc-secret-projection-v1",
        "sources": [
            {"path": "crates/ikore-core/src/usage/export.rs", "class": "candidate",
             "origin": {"kind": "stage_artifact", "stage": "code", "artifactKind": "code"},
             "sha256": "a", "delivered": true, "owned": false},
            {"path": "knowledge/obsidian/features/usage-export-to-csv.md", "class": "stage_document",
             "origin": {"kind": "stage_artifact", "stage": "specification", "artifactKind": "specification"},
             "sha256": "b", "delivered": false, "owned": false}
        ],
        "candidates": [
            {"path": "crates/ikore-core/src/usage/export.rs", "base": "present", "baseSha256": "c",
             "publishedSha256": "a", "liveSha256": "d", "binding": "changed_outside_pipeline", "diff": "included"},
            {"path": "crates/ikore-core/tests/usage_export_header_only_test_with_a_long_name.rs", "base": "absent",
             "publishedSha256": "e", "liveSha256": "e", "binding": "fresh", "diff": "created"}
        ],
        "checks": [],
        "policy": {"state": "loaded", "files": [
            {"path": "REVIEW.md", "state": "loaded", "sha256": "0123456789abcdef", "bytes": 120}
        ]},
        "gaps": [
            {"code": "candidate_changed_outside_pipeline",
             "subject": "`crates/ikore-core/src/usage/export.rs` changed after this pulsation published it; checks and earlier stages saw other bytes.",
             "scope": {"kind": "paths", "items": ["crates/ikore-core/src/usage/export.rs"]},
             "affectedCriteria": ["crit_1_1"]}
        ],
        "authorClaims": 3,
        "limits": {"diffCharsPerFile": 8000, "diffCharsTotal": 32000, "claimChars": 400, "claimsCharsTotal": 8000}
    }))
    .unwrap();
    ikore_core::orchestration::composition::review_boundary::CheckpointReviewInputs::Recorded {
        manifest,
        changed_policy_files: vec!["REVIEW.md".into()],
    }
}

#[gpui::test]
fn the_integrator_brief_leads_with_the_reviewer_inputs_and_requires_a_decision_beyond_the_threshold(
    cx: &mut TestAppContext,
) {
    let convergence = ikore_core::persistence_service::ConvergenceStatus {
        applies: true,
        input_revision: 1,
        threshold: 3,
        rounds: 3,
        technical_attempts: 9,
        ..Default::default()
    };
    let (model, visual) =
        open_integrator_review(cx, recorded_review_inputs(), Some(convergence), true);
    for (width, height) in [(960.0, 640.0), (1440.0, 900.0)] {
        visual.simulate_resize(size(px(width), px(height)));
        select(&model, visual, AppView::Pulsations);
        let reader = visual.debug_bounds("review-document-reader").unwrap();
        let inputs = visual.debug_bounds("review-inputs").unwrap();
        let brief = visual.debug_bounds("review-brief").unwrap();
        assert!(
            inputs.left() >= reader.left() && inputs.right() <= reader.right() + px(1.0),
            "the inputs card overflows at {width}: {inputs:?} vs {reader:?}"
        );
        assert!(
            inputs.bottom() <= brief.top(),
            "the inputs card leads the brief"
        );
        for head in [
            "review-inputs-policy",
            "review-inputs-candidate-0",
            "review-inputs-gap-0",
        ] {
            let bounds = visual
                .debug_bounds(head)
                .unwrap_or_else(|| panic!("missing {head} at {width}"));
            assert!(bounds.size.height <= px(64.0), "{head} is {bounds:?}");
        }
        let actions = visual.debug_bounds("review-decision-bar").unwrap();
        let rounds = visual.debug_bounds("convergence-rounds").unwrap();
        assert!(
            rounds.bottom() <= actions.top() + px(1.0)
                && rounds.left() >= actions.left()
                && rounds.right() <= actions.right(),
            "the round line {rounds:?} sits above the bar {actions:?} at {width}"
        );
        for action in [
            "hitl-approve",
            "hitl-continue-revision",
            "hitl-replan",
            "hitl-reject",
        ] {
            let bounds = visual
                .debug_bounds(action)
                .unwrap_or_else(|| panic!("missing {action} at {width}"));
            assert!(
                bounds.left() >= actions.left() && bounds.right() <= actions.right(),
                "{action} {bounds:?} leaves the bar {actions:?} at {width}"
            );
            assert!(bounds.bottom() <= px(height));
        }
    }
    assert!(
        visual.debug_bounds("hitl-revision").is_none(),
        "beyond the threshold the plain revision is gone"
    );
    // The keyboard's plain revision explains the choice instead of sending.
    model.update(visual, |model, cx| {
        model.apply_hitl_decision(
            ikore_core::persistence_service::HitlDecision::RequestRevision,
            cx,
        )
    });
    model.read_with(visual, |model, cx| {
        let hitl = &model.pulsations.read(cx).hitl;
        assert!(!hitl.busy);
        assert!(hitl.show_feedback);
        assert!(
            hitl.error
                .as_deref()
                .is_some_and(|error| error.contains("revision-round threshold")),
            "{:?}",
            hitl.error
        );
    });
    // Continue without a reason asks for one and sends nothing.
    click(visual, "hitl-continue-revision");
    model.read_with(visual, |model, cx| {
        let hitl = &model.pulsations.read(cx).hitl;
        assert!(!hitl.busy);
        assert!(
            hitl.error
                .as_deref()
                .is_some_and(|error| error.contains("needs a reason")),
            "{:?}",
            hitl.error
        );
    });
}

#[gpui::test]
fn a_legacy_integrator_review_says_the_policy_does_not_apply(cx: &mut TestAppContext) {
    let (model, visual) = open_integrator_review(
        cx,
        ikore_core::orchestration::composition::review_boundary::CheckpointReviewInputs::LegacyPipeline,
        None,
        false,
    );
    visual.simulate_resize(size(px(960.0), px(640.0)));
    select(&model, visual, AppView::Pulsations);
    let notice = visual.debug_bounds("review-inputs-legacy").unwrap();
    let reader = visual.debug_bounds("review-document-reader").unwrap();
    assert!(
        notice.bottom() <= reader.top(),
        "the notice stands above the documents"
    );
    assert!(notice.size.height <= px(48.0), "{notice:?}");
    assert!(visual.debug_bounds("review-inputs").is_none());
    assert!(visual.debug_bounds("convergence-rounds").is_none());
    assert!(visual.debug_bounds("hitl-revision").is_some());
}

fn high_risk_status() -> ikore_core::persistence_service::CheckpointRiskStatus {
    serde_json::from_value(serde_json::json!({
        "applies": true,
        "assessment": {
            "level": "high",
            "reasons": [
                {"signal": "sensitive_path", "level": "high",
                 "subject": "crates/ikore-core/src/usage/export_with_a_rather_long_auth_token_name.rs"}
            ],
            "unknownInputs": ["intake:task_2"],
            "policyVersion": 1,
            "policySha256": "abc",
            "projectPolicyState": "builtin_only"
        },
        "matrix": {
            "level": "high",
            "rows": [
                {"kind": "criterion", "subject": "crit_1_1", "state": "model_only",
                 "detail": "supported by the model's review only"},
                {"kind": "check", "subject": "unit", "state": "not_run", "detail": "not_run"}
            ],
            "missingKinds": ["lint"]
        },
        "acknowledgementRequired": true,
        "acknowledgements": []
    }))
    .unwrap()
}

/// ADR-034 M3: the risk card leads the Integrator's brief, fits the reader
/// at both window sizes, and an Approve that needs an acknowledgement asks
/// for a reason instead of sending.
#[gpui::test]
fn a_high_risk_integrator_review_shows_its_coverage_and_asks_for_a_reason(cx: &mut TestAppContext) {
    let (model, visual) = open_integrator_review(cx, recorded_review_inputs(), None, true);
    model.update(visual, |m, cx| {
        m.pulsations.update(cx, |state, _| {
            let checkpoint = state.hitl.expected_checkpoint.clone().unwrap();
            let generation = state.hitl.current_load_generation();
            assert!(
                state
                    .hitl
                    .finish_risk(generation, &checkpoint, Ok(Some(high_risk_status())))
            );
        });
    });
    for (width, height) in [(960.0, 640.0), (1440.0, 900.0)] {
        visual.simulate_resize(size(px(width), px(height)));
        select(&model, visual, AppView::Pulsations);
        let reader = visual.debug_bounds("review-document-reader").unwrap();
        let risk = visual.debug_bounds("review-risk").unwrap();
        let inputs = visual.debug_bounds("review-inputs").unwrap();
        assert!(
            risk.left() >= reader.left() && risk.right() <= reader.right() + px(1.0),
            "the risk card overflows at {width}: {risk:?} vs {reader:?}"
        );
        assert!(risk.bottom() <= inputs.top(), "risk leads the inputs");
        for part in [
            "review-risk-level",
            "review-risk-missing",
            "review-risk-row-0",
            "review-risk-row-1",
            "review-risk-acknowledgement",
        ] {
            let bounds = visual
                .debug_bounds(part)
                .unwrap_or_else(|| panic!("missing {part} at {width}"));
            assert!(
                bounds.right() <= reader.right() + px(1.0),
                "{part} {bounds:?}"
            );
        }
        let actions = visual.debug_bounds("review-decision-bar").unwrap();
        let approve = visual.debug_bounds("hitl-approve").unwrap();
        assert!(approve.left() >= actions.left() && approve.right() <= actions.right());
        assert!(approve.bottom() <= px(height));
    }
    // Approve without a reason explains instead of sending.
    model.update(visual, |model, cx| {
        model.apply_hitl_decision(ikore_core::persistence_service::HitlDecision::Approve, cx)
    });
    model.read_with(visual, |model, cx| {
        let hitl = &model.pulsations.read(cx).hitl;
        assert!(!hitl.busy);
        assert!(hitl.show_feedback);
        assert_eq!(
            hitl.error.as_deref(),
            Some(crate::app_model::RISK_REASON_REQUIRED)
        );
    });
}

/// A preview of the mandate an Approve would issue, with `paths` granted and
/// the first one marked sensitive.
fn grants_preview(
    enforcement: ikore_core::orchestration::mandate::Enforcement,
    paths: usize,
) -> ikore_core::persistence_service::MandatePreview {
    use ikore_core::orchestration::mandate::{
        BudgetParameters, IssueInputs, IssuedBy, StageOutputLimit, TransportCapability, WriteClass,
        WriteGrant, derive_issued,
    };
    let mut mandate = derive_issued(&IssueInputs {
        pulsation_id: "pulse",
        project_id: "project",
        enforcement,
        title: "Usage export to CSV",
        issued_by: IssuedBy {
            checkpoint_id: "brief-checkpoint".into(),
            stage: "SPECIFICATION".into(),
            state_version: 1,
            input_revision: 1,
        },
        previous: None,
        manifest: None,
        clarification_paths: &[],
        specification: None,
        accepted_artifact_sha256s: &[],
        review_policy_snapshot_sha256: None,
        project_sensitive_sha256: None,
        transports: vec![TransportCapability {
            role: "backend_agent".into(),
            transport: "cli".into(),
            backend: Some("claude".into()),
        }],
        checks: Vec::new(),
        budget: BudgetParameters {
            stage_output_limits: vec![StageOutputLimit {
                stage: "Code".into(),
                output_tokens: 16_000,
            }],
            context_chars_limit: 16_000,
            extra_invocation_limit: 2,
            per_unit_extra_invocation_limit: 1,
            max_in_flight_units: 1,
        },
    })
    .unwrap();
    mandate.scope.grants = (0..paths)
        .map(|index| WriteGrant {
            path: format!(
                "crates/ikore-core/src/usage/export_with_a_rather_long_module_name_{index:02}.rs"
            ),
            origins: vec![WriteClass::TaskPath, WriteClass::SpecificationReference],
            unit_key: None,
            amendment_revision: None,
        })
        .collect();
    let sensitive = mandate
        .scope
        .grants
        .first()
        .map(|grant| vec![grant.path.clone()])
        .unwrap_or_default();
    ikore_core::persistence_service::MandatePreview {
        mandate_sha256: mandate.sha256().unwrap().as_str().to_owned(),
        mandate,
        sensitive_paths: sensitive,
    }
}

/// ADR-035 M1: "What approving grants" leads the Specification brief, keeps
/// every row inside the reader at both window sizes, summarizes what it does
/// not draw, and a legacy pipeline draws no card.
#[gpui::test]
fn the_specification_brief_shows_what_approving_grants(cx: &mut TestAppContext) {
    use ikore_core::orchestration::mandate::Enforcement;
    let (model, visual) = open_specification_brief(cx);
    model.update(visual, |m, cx| {
        m.pulsations.update(cx, |state, _| {
            let checkpoint = state.hitl.expected_checkpoint.clone().unwrap();
            let generation = state.hitl.current_load_generation();
            assert!(state.hitl.finish_grants(
                generation,
                &checkpoint,
                Ok(Some(grants_preview(Enforcement::EnforcedV4, 70)))
            ));
        });
    });
    for (width, height) in [(960.0, 640.0), (1440.0, 900.0)] {
        visual.simulate_resize(size(px(width), px(height)));
        select(&model, visual, AppView::Pulsations);
        let reader = visual.debug_bounds("review-document-reader").unwrap();
        let grants = visual
            .debug_bounds("review-grants")
            .unwrap_or_else(|| panic!("missing grants card at {width}"));
        assert!(
            grants.left() >= reader.left() && grants.right() <= reader.right() + px(1.0),
            "the grants card overflows at {width}: {grants:?} vs {reader:?}"
        );
        for part in [
            "review-grants-sensitive",
            "review-grants-path-0",
            "review-grants-path-63",
            "review-grants-more",
            "review-grants-digest",
        ] {
            let bounds = visual
                .debug_bounds(part)
                .unwrap_or_else(|| panic!("missing {part} at {width}"));
            assert!(
                bounds.right() <= reader.right() + px(1.0),
                "{part} {bounds:?}"
            );
        }
        assert!(
            visual.debug_bounds("review-grants-path-64").is_none(),
            "only the first 64 paths are drawn"
        );
        assert_readable(visual, "review-grants-summary", 200.0, 80.0);
        assert_readable(visual, "review-grants-path-text-0", 120.0, 160.0);
        assert_readable(visual, "review-grants-transport-0", 40.0, 40.0);
    }
    model.update(visual, |m, cx| {
        m.pulsations.update(cx, |state, _| {
            let checkpoint = state.hitl.expected_checkpoint.clone().unwrap();
            let generation = state.hitl.current_load_generation();
            assert!(state.hitl.finish_grants(
                generation,
                &checkpoint,
                Ok(Some(grants_preview(Enforcement::NotAppliedLegacy, 0)))
            ));
            assert!(
                state.hitl.grants.is_none(),
                "a legacy approval has no envelope"
            );
        });
    });
}

/// A legacy Specification approval has no envelope, so no grants card is
/// drawn. A fresh window: absence is only reliable before the first frame
/// that held the card.
#[gpui::test]
fn a_legacy_specification_review_draws_no_grants_card(cx: &mut TestAppContext) {
    use ikore_core::orchestration::mandate::Enforcement;
    let (model, visual) = open_specification_brief(cx);
    model.update(visual, |m, cx| {
        m.pulsations.update(cx, |state, _| {
            let checkpoint = state.hitl.expected_checkpoint.clone().unwrap();
            let generation = state.hitl.current_load_generation();
            assert!(state.hitl.finish_grants(
                generation,
                &checkpoint,
                Ok(Some(grants_preview(Enforcement::NotAppliedLegacy, 0)))
            ));
        });
    });
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    select(&model, visual, AppView::Pulsations);
    assert!(visual.debug_bounds("review-document-reader").is_some());
    assert!(visual.debug_bounds("review-grants").is_none());
}

/// Layers with every row flagged or not: 70 sensitive grants (one a
/// 260-character path, sorted first) and, when `flagged`, a model other than
/// the one requested.
fn layers_snapshot(flagged: bool) -> ikore_core::orchestration::layers::PulsationLayersSnapshot {
    use ikore_core::orchestration::layers::*;
    use ikore_core::orchestration::mandate::MandateCause;
    let long = format!("src/a/{}deep.rs", "segment_".repeat(31));
    derive_layers(&LayerInputs {
        composed: true,
        past_specification: true,
        mandate: Some(MandateFact {
            revision: 1,
            cause: MandateCause::Issued,
            granted: 70,
            transport_fallback: false,
        }),
        sensitive_granted: if flagged {
            std::iter::once(long)
                .chain((0..69).map(|index| format!("src/auth_{index:02}.rs")))
                .collect()
        } else {
            Vec::new()
        },
        runs: vec![RunFact {
            actor: "code_implementer".into(),
            transport: "api".into(),
            provider: Some("openai".into()),
            requested_model: Some("gpt-4.1-mini".into()),
            observed_model: Some(if flagged { "gpt-4.1" } else { "gpt-4.1-mini" }.into()),
            adapter_version: None,
            completed: true,
        }],
        usage: UsageFact {
            complete: 2,
            ..UsageFact::default()
        },
        criteria: Some(CriteriaFact {
            total: 1,
            assigned: 1,
            validated: 1,
            without_check: Vec::new(),
        }),
        ..LayerInputs::default()
    })
}

fn with_layers(
    model: &Entity<AppModel>,
    visual: &mut VisualTestContext,
    stage: PulsationStage,
    layers: ikore_core::orchestration::layers::PulsationLayersSnapshot,
) {
    model.update(visual, |m, cx| {
        m.pulsations.update(cx, |state, _| {
            state.pulsations[0].stage = stage;
            let pulse = state.pulsations[0].clone();
            let mut detail = composed_detail(&pulse);
            detail.stage = stage;
            detail.layers = Some(layers);
            state.details = vec![detail];
        });
        cx.notify();
    });
}

#[gpui::test]
fn the_layers_panel_leads_the_overview_and_opens_a_layer(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    with_layers(&model, visual, PulsationStage::Code, layers_snapshot(true));
    model.update(visual, |m, cx| {
        m.pulsations.update(cx, |state, _| {
            state.detail_section = PulsationDetailSection::Overview;
        });
        cx.notify();
    });
    for (width, height) in [(960.0, 640.0), (1440.0, 900.0)] {
        visual.simulate_resize(size(px(width), px(height)));
        select(&model, visual, AppView::Pulsations);
        let panel = visual
            .debug_bounds("layers-panel")
            .unwrap_or_else(|| panic!("missing layers panel at {width}"));
        for row in [
            "layers-row-identity",
            "layers-row-scope",
            "layers-row-guardrails",
            "layers-row-intent",
        ] {
            let bounds = visual
                .debug_bounds(row)
                .unwrap_or_else(|| panic!("missing {row} at {width}"));
            assert!(
                bounds.left() >= panel.left() && bounds.right() <= panel.right() + px(1.0),
                "{row} overflows at {width}: {bounds:?} vs {panel:?}"
            );
        }
        let toggle = visual.debug_bounds("execution-details-toggle").unwrap();
        assert!(
            panel.bottom() <= toggle.top(),
            "the layers lead the overview and the execution details follow"
        );
    }
    click(visual, "layers-row-scope");
    let panel = visual.debug_bounds("layers-panel").unwrap();
    for part in [
        "layers-items-scope",
        "layers-item-scope-0",
        "layers-item-scope-1",
        "layers-item-scope-49",
        "layers-more-scope",
    ] {
        let bounds = visual
            .debug_bounds(part)
            .unwrap_or_else(|| panic!("missing {part}"));
        assert!(
            bounds.right() <= panel.right() + px(1.0),
            "{part} overflows: {bounds:?} vs {panel:?}"
        );
    }
    assert!(
        visual.debug_bounds("layers-item-scope-50").is_none(),
        "fifty items, then the count"
    );
    assert_readable(visual, "layers-item-label-scope-0", 80.0, 60.0);
    assert_readable(visual, "layers-item-value-scope-0", 80.0, 60.0);
    click(visual, "layers-row-scope");
    model.read_with(visual, |m, cx| {
        assert!(
            m.pulsations.read(cx).layers_expanded.is_empty(),
            "a second click closes the layer"
        );
    });
}

#[gpui::test]
fn the_header_marks_the_worst_layer_only_when_one_needs_a_look(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    with_layers(&model, visual, PulsationStage::Code, layers_snapshot(true));
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    select(&model, visual, AppView::Pulsations);
    let pill = visual.debug_bounds("header-context").unwrap();
    let dot = visual.debug_bounds("header-layers-dot").unwrap();
    assert!(dot.left() >= pill.left() && dot.right() <= pill.right());
    // The header is in every view; check absence on one without split panes,
    // which `assert_absent`'s second window needs.
    with_layers(&model, visual, PulsationStage::Code, layers_snapshot(false));
    select(&model, visual, AppView::Home);
    assert_absent(&model, visual, "header-layers-dot");
}

#[gpui::test]
fn the_integrator_brief_shows_the_layers_card(cx: &mut TestAppContext) {
    let (model, visual) = open_integrator_review(cx, recorded_review_inputs(), None, true);
    with_layers(
        &model,
        visual,
        PulsationStage::Integrator,
        layers_snapshot(true),
    );
    for (width, height) in [(960.0, 640.0), (1440.0, 900.0)] {
        visual.simulate_resize(size(px(width), px(height)));
        select(&model, visual, AppView::Pulsations);
        let reader = visual.debug_bounds("review-document-reader").unwrap();
        let card = visual
            .debug_bounds("review-layers")
            .unwrap_or_else(|| panic!("missing layers card at {width}"));
        assert!(
            card.left() >= reader.left() && card.right() <= reader.right() + px(1.0),
            "the layers card overflows at {width}: {card:?} vs {reader:?}"
        );
        for row in [
            "review-layers-row-identity",
            "review-layers-row-scope",
            "review-layers-row-guardrails",
            "review-layers-row-intent",
        ] {
            assert!(
                visual.debug_bounds(row).is_some(),
                "missing {row} at {width}"
            );
        }
        for headline in [
            "review-layers-headline-identity",
            "review-layers-headline-scope",
            "review-layers-headline-guardrails",
            "review-layers-headline-intent",
        ] {
            assert_readable(visual, headline, 120.0, 60.0);
        }
    }
}

/// The Specification's Scope is its grants card: no layers card there. A
/// fresh window, as absence needs.
#[gpui::test]
fn a_specification_brief_draws_no_layers_card(cx: &mut TestAppContext) {
    let (model, visual) = open_specification_brief(cx);
    with_layers(
        &model,
        visual,
        PulsationStage::Specification,
        layers_snapshot(true),
    );
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    select(&model, visual, AppView::Pulsations);
    assert!(visual.debug_bounds("review-document-reader").is_some());
    assert!(visual.debug_bounds("review-layers").is_none());
}

#[gpui::test]
fn forge_triage_shows_the_conformance_card_and_holds_approval_on_a_blocking_conflict(
    cx: &mut TestAppContext,
) {
    use ikore_core::feature_forge::FeatureState;
    use ikore_core::feature_forge::preflight::PreflightResolution;
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    forge_feature_fixture(&model, visual, &[FeatureState::InTriage]);
    for (width, height) in [(960.0, 640.0), (1440.0, 900.0)] {
        visual.simulate_resize(size(px(width), px(height)));
        model.update(visual, |m, cx| {
            let mut finding = crate::test_support::blocking_finding("feature-0", 2);
            finding.resolution = PreflightResolution::Pending;
            m.forge.snapshot.preflight = vec![finding];
            m.forge.section = crate::forge::ForgeSection::Triage;
            m.select_view(AppView::Forge, cx);
            cx.notify();
        });
        visual.run_until_parked();
        let detail = visual.debug_bounds("forge-detail").unwrap();
        let card = visual.debug_bounds("preflight-card").unwrap();
        assert!(
            card.left() >= detail.left() && card.right() <= detail.right(),
            "the conformance card overflows the triage column at {width}x{height}: {card:?}"
        );
        for selector in [
            "preflight-conflict-0",
            "preflight-adjust",
            "preflight-propose-adr",
            "preflight-dismiss",
        ] {
            assert!(
                visual.debug_bounds(selector).is_some(),
                "{selector} is missing at {width}x{height}"
            );
        }
    }

    // The approval control states why it is held.
    assert!(visual.debug_bounds("triage-approve").is_some());
    assert!(visual.debug_bounds("preflight-approval-blocked").is_some());

    // Once dismissed, the card keeps the record without the decisions or the
    // hint. Debug bounds outlive the frame that drew them, so the dismissed
    // state is drawn in a fresh window.
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    forge_feature_fixture(&model, visual, &[FeatureState::InTriage]);
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    model.update(visual, |m, cx| {
        let mut finding = crate::test_support::blocking_finding("feature-0", 2);
        finding.resolution = PreflightResolution::Dismissed;
        finding.reason = Some("Only the release script uses it.".into());
        m.forge.snapshot.preflight = vec![finding];
        m.forge.section = crate::forge::ForgeSection::Triage;
        m.select_view(AppView::Forge, cx);
        cx.notify();
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("preflight-card").is_some());
    assert!(visual.debug_bounds("triage-approve").is_some());
    assert!(visual.debug_bounds("preflight-dismiss").is_none());
    assert!(visual.debug_bounds("preflight-approval-blocked").is_none());
}

/// A Vault window drawn once with one drifted note (ADR-034 M5).
fn open_doc_drift(
    cx: &mut TestAppContext,
    with_proposal: bool,
    open_note: bool,
    width: f32,
) -> (Entity<AppModel>, &mut VisualTestContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    model.update(visual, |m, cx| {
        m.vault.update(cx, |vault, _| {
            vault.workspace_root = Some("C:\\vault".into());
        });
        m.doc_drift.project_id = Some("layout-project".into());
        m.doc_drift.overview = crate::test_support::drift_overview(with_proposal);
        if open_note {
            m.doc_drift.open_note = Some("knowledge/obsidian/features/api.md".into());
        }
    });
    visual.simulate_resize(size(px(width), px(800.0)));
    select(&model, visual, AppView::Vault);
    (model, visual)
}

/// ADR-034 M5: the indicator, the Vault list and the comparison. Debug bounds
/// outlive the frame that drew them, so each state is drawn in a fresh window.
#[gpui::test]
fn documentation_drift_shows_the_indicator_the_list_and_the_comparison(cx: &mut TestAppContext) {
    for width in [960.0, 1440.0] {
        let (_, visual) = open_doc_drift(cx, false, false, width);
        let bar = visual.debug_bounds("status-bar").unwrap();
        let segment = visual
            .debug_bounds("status-doc-drift")
            .expect("the indicator");
        assert!(segment.right() <= bar.right() && segment.bottom() <= bar.bottom());
        assert!(visual.debug_bounds("doc-drift-card").is_some());
        assert!(
            visual
                .debug_bounds("doc-drift-note:knowledge/obsidian/features/api.md")
                .is_some()
        );
        assert!(
            visual
                .debug_bounds(
                    "doc-drift-unsilence:knowledge/obsidian/features/api.md:src/legacy.rs"
                )
                .is_some()
        );
        assert!(visual.debug_bounds("doc-drift-comparison").is_none());
    }

    // Without a proposal, the comparison offers to ask for one.
    let (_, visual) = open_doc_drift(cx, false, true, 1440.0);
    assert!(visual.debug_bounds("doc-drift-comparison").is_some());
    assert!(visual.debug_bounds("doc-drift-propose").is_some());
    assert!(visual.debug_bounds("doc-drift-approve").is_none());
    assert!(
        visual
            .debug_bounds("doc-drift-silence:src/api.rs")
            .is_some()
    );
    assert!(visual.debug_bounds("doc-drift-ignore").is_some());

    // With one, both sides sit next to each other inside the column.
    for width in [960.0, 1440.0] {
        let (_, visual) = open_doc_drift(cx, true, true, width);
        let panel = visual.debug_bounds("doc-drift-comparison").unwrap();
        let code = visual.debug_bounds("doc-drift-code").unwrap();
        let proposal = visual.debug_bounds("doc-drift-proposal").unwrap();
        for side in [code, proposal] {
            assert!(
                side.left() >= panel.left() && side.right() <= panel.right(),
                "a side overflows the comparison at {width}: {side:?}"
            );
        }
        for selector in ["doc-drift-approve", "doc-drift-edit", "doc-drift-reject"] {
            assert!(
                visual.debug_bounds(selector).is_some(),
                "{selector} at {width}"
            );
        }
        assert!(visual.debug_bounds("doc-drift-propose").is_none());
    }
}

fn attestation(
    state: ikore_core::attestation::AttestationState,
) -> ikore_core::attestation::AttestationStatus {
    use ikore_core::attestation::*;
    let sealed = !matches!(
        state,
        AttestationState::NotSealed
            | AttestationState::Pending
            | AttestationState::Skipped
            | AttestationState::Failed
    );
    AttestationStatus {
        state,
        key_id: (state == AttestationState::Signed)
            .then(|| format!("hvc-ed25519:{}", "0f1e2d3c".repeat(8))),
        statement_sha256: sealed.then(|| "ab".repeat(32)),
        subject_count: if sealed { 12 } else { 0 },
        outcome: sealed.then(|| "done".to_owned()),
        sealed_at: sealed.then(|| "2026-10-26T12:00:00Z".to_owned()),
        signed_at: None,
        diagnostic: (state == AttestationState::Failed)
            .then(|| "Runtime error: the coverage matrix could not be read.".to_owned()),
        verification: None,
    }
}

fn with_attestation(
    model: &Entity<AppModel>,
    visual: &mut VisualTestContext,
    status: ikore_core::attestation::AttestationStatus,
) {
    model.update(visual, |m, cx| {
        m.pulsations.update(cx, |state, _| {
            state.pulsations[0].status = PulsationStatus::Done;
            state.detail_section = PulsationDetailSection::Overview;
            let pulse = state.pulsations[0].clone();
            let mut detail = composed_detail(&pulse);
            detail.attested = status.state == ikore_core::attestation::AttestationState::Signed;
            detail.attestation = Some(status);
            state.details = vec![detail];
        });
        cx.notify();
    });
}

#[gpui::test]
fn the_attestation_card_shows_a_signed_record_and_what_changed_since(cx: &mut TestAppContext) {
    use ikore_core::attestation::*;
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    let mut status = attestation(AttestationState::Signed);
    let long = format!("src/a/{}deep.rs", "segment_".repeat(31));
    status.verification = Some(AttestationVerification {
        checked_at: "2026-10-26T12:05:00Z".into(),
        signature: SignatureCheck::Valid,
        rederivation: RederivationCheck::Matches,
        difference: None,
        workspace: std::iter::once(long)
            .chain((0..11).map(|index| format!("src/file_{index:02}.rs")))
            .enumerate()
            .map(|(index, path)| SubjectCheck {
                path,
                state: if index < 10 {
                    WorkspaceCheck::Modified
                } else {
                    WorkspaceCheck::Unchanged
                },
            })
            .collect(),
    });
    with_attestation(&model, visual, status);
    model.update(visual, |m, cx| {
        m.pulsations.update(cx, |state, _| {
            if !state.group_expanded(PulsationStatus::Done) {
                state.toggle_group(PulsationStatus::Done);
            }
        });
        cx.notify();
    });
    for (width, height) in [(960.0, 640.0), (1440.0, 900.0)] {
        visual.simulate_resize(size(px(width), px(height)));
        select(&model, visual, AppView::Pulsations);
        let card = visual
            .debug_bounds("pulsation-attestation")
            .unwrap_or_else(|| panic!("missing the card at {width}"));
        for part in [
            "attestation-headline",
            "attestation-digest",
            "attestation-verification",
            "attestation-workspace-0",
            "attestation-workspace-7",
            "attestation-verify",
            "attestation-save",
            "attestation-trailer",
        ] {
            let bounds = visual
                .debug_bounds(part)
                .unwrap_or_else(|| panic!("missing {part} at {width}"));
            assert!(
                bounds.left() >= card.left() && bounds.right() <= card.right() + px(1.0),
                "{part} overflows at {width}: {bounds:?} vs {card:?}"
            );
        }
        assert!(
            visual.debug_bounds("attestation-workspace-8").is_none(),
            "eight paths, then the count"
        );
        assert!(visual.debug_bounds("attestation-sign").is_none());
        let report = visual.debug_bounds("pulsation-report").unwrap();
        assert!(
            card.bottom() <= report.top(),
            "the record comes before the report"
        );
        assert!(
            visual.debug_bounds("pulsation-attested").is_some(),
            "the list marks the attested pulsation"
        );
    }
    let pulse = model.read_with(visual, |m, cx| {
        m.pulsations.read(cx).pulsations[0].id.clone()
    });
    // The agent dock's strip takes the bottom of the window (ADR-040 M3); hidden, the trailer
    // is in view without scrolling, as this test expects.
    model.update(visual, |m, cx| {
        m.set_agent_dock_mode(crate::agent_dock::DockMode::Hidden, cx)
    });
    visual.run_until_parked();
    click(visual, "attestation-trailer");
    visual.update(|_, cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text(),
            Some(format!(
                "HVC-Pulsation: {pulse}\nHVC-Attestation: sha256:{}",
                "ab".repeat(32)
            ))
        );
    });
    assert!(visual.debug_bounds("attestation-notice").is_some());
}

#[gpui::test]
fn the_attestation_card_offers_what_each_state_allows(cx: &mut TestAppContext) {
    use ikore_core::attestation::AttestationState;
    let buttons = [
        "attestation-verify",
        "attestation-save",
        "attestation-trailer",
        "attestation-sign",
        "attestation-retry",
    ];
    for (state, offered) in [
        (
            AttestationState::Unsigned,
            vec!["attestation-verify", "attestation-sign"],
        ),
        (AttestationState::TooLarge, vec!["attestation-verify"]),
        (AttestationState::Failed, vec!["attestation-retry"]),
        (AttestationState::Pending, vec![]),
        (AttestationState::Skipped, vec![]),
    ] {
        let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
        with_attestation(&model, visual, attestation(state));
        visual.simulate_resize(size(px(1440.0), px(900.0)));
        select(&model, visual, AppView::Pulsations);
        assert!(
            visual.debug_bounds("attestation-headline").is_some(),
            "{state:?}"
        );
        for button in buttons {
            assert_eq!(
                visual.debug_bounds(button).is_some(),
                offered.contains(&button),
                "{state:?}: {button}"
            );
        }
        assert!(
            visual.debug_bounds("pulsation-attested").is_none(),
            "only a signed record is marked: {state:?}"
        );
    }
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    with_attestation(&model, visual, attestation(AttestationState::NotSealed));
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    select(&model, visual, AppView::Pulsations);
    assert!(
        visual.debug_bounds("pulsation-attestation").is_none(),
        "no card before the pulsation is sealed"
    );
}

#[gpui::test]
fn the_security_page_shows_the_key_and_asks_before_a_new_one(cx: &mut TestAppContext) {
    use ikore_core::attestation::*;
    let (model, visual) = cx.add_window_view(|_, cx| {
        let mut model = fixture(cx);
        model.settings.update(cx, |settings, _| {
            settings.snapshot.as_mut().unwrap().attestation = Some(AttestationKeyStatus {
                state: AttestationKeyState::KeyMissing,
                keys: vec![
                    AttestationKeyRecord {
                        key_id: format!("hvc-ed25519:{}", "1".repeat(64)),
                        state: "active".into(),
                        created_at: "2026-10-26T12:00:00Z".into(),
                        retired_at: None,
                    },
                    AttestationKeyRecord {
                        key_id: format!("hvc-ed25519:{}", "2".repeat(64)),
                        state: "retired".into(),
                        created_at: "2026-10-01T12:00:00Z".into(),
                        retired_at: Some("2026-10-26T12:00:00Z".into()),
                    },
                ],
                sealing_enabled: true,
            });
            settings.attestation_file_check = Some(Ok(AttestationFileCheck {
                signature: SignatureCheck::Valid,
                key: FileKey::UnknownKey,
                key_id: Some(format!("hvc-ed25519:{}", "3".repeat(64))),
                pulsation_id: Some("pulse-a".into()),
                statement_sha256: Some("cd".repeat(32)),
                subject_count: 3,
            }));
        });
        model.open_settings(Some(SettingsPage::Security), cx);
        model
    });
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    visual.run_until_parked();
    for part in [
        "attestation-key-state",
        "attestation-key-0",
        "attestation-key-1",
        "attestation-create-key",
        "attestation-sealing",
        "attestation-verify-file",
        "attestation-file-check",
    ] {
        assert!(visual.debug_bounds(part).is_some(), "missing {part}");
    }
    assert!(
        visual
            .debug_bounds("attestation-create-key-cancel")
            .is_none()
    );
    click(visual, "attestation-create-key");
    model.read_with(visual, |m, cx| {
        assert!(
            m.settings.read(cx).attestation_rotate_confirm,
            "the first click only asks"
        );
    });
    assert!(
        visual
            .debug_bounds("attestation-create-key-cancel")
            .is_some()
    );
    click(visual, "attestation-create-key-cancel");
    model.read_with(visual, |m, cx| {
        assert!(!m.settings.read(cx).attestation_rotate_confirm);
    });
}

fn scope_request(state: &str) -> ikore_core::persistence_service::ScopeRequestView {
    use ikore_core::orchestration::composition::{Handle, Risk, SourceRole};
    use ikore_core::orchestration::mandate::{ScopePathV1, ScopeRefusalCode, ScopeRefusalV1};
    let long = format!("src/a/{}deep.rs", "segment_".repeat(31));
    ikore_core::persistence_service::ScopeRequestView {
        request_id: "scope-1".into(),
        checkpoint_id: "layout-failure".into(),
        stage: PulsationStage::Code,
        unit_key: "implement_expiry".into(),
        accepted: vec![
            ScopePathV1 {
                handle: Handle::new("src_2").unwrap(),
                path: long,
                role: SourceRole::Source,
                sha256: "ab".repeat(32),
                sensitive: false,
            },
            ScopePathV1 {
                handle: Handle::new("src_3").unwrap(),
                path: "src/auth/keys.rs".into(),
                role: SourceRole::Source,
                sha256: "cd".repeat(32),
                sensitive: true,
            },
        ],
        refused: vec![ScopeRefusalV1 {
            handle: Handle::new("src_4").unwrap(),
            path: Some("src/beta.rs".into()),
            code: ScopeRefusalCode::ScopeOwnedByOtherUnit,
        }],
        risk_before: Risk::Medium,
        risk_after: Risk::High,
        agent_message: Some("The key loader this unit reads must change too.".into()),
        derivation_sha256: "ef".repeat(32),
        state: state.into(),
        remaining_extra_invocations: 1,
    }
}

fn parked_scope_window<'a>(
    cx: &'a mut TestAppContext,
    state: &str,
    window: (f32, f32),
) -> (Entity<AppModel>, &'a mut VisualTestContext) {
    let (model, visual) = parked_failure_window(cx, "model_reported_failure", false, true, window);
    let request = scope_request(state);
    model.update(visual, |m, cx| {
        m.pulsations.update(cx, |pulsations, _| {
            let mut checkpoint = pulsations.failure.checkpoint.clone().unwrap();
            checkpoint.stage = PulsationStage::Code;
            checkpoint.scope_request = Some(request);
            pulsations.failure.load_checkpoint(Some(checkpoint));
        });
        cx.notify();
    });
    visual.run_until_parked();
    (model, visual)
}

#[gpui::test]
fn a_scope_request_shows_its_files_and_leads_the_decisions_with_the_grant(cx: &mut TestAppContext) {
    for (width, height) in [(960.0, 640.0), (1440.0, 900.0)] {
        let (_model, visual) = parked_scope_window(cx, "pending", (width, height));
        let card = visual
            .debug_bounds("scope-request")
            .unwrap_or_else(|| panic!("missing the card at {width}"));
        for part in [
            "scope-request-path-0",
            "scope-request-path-1",
            "scope-request-refused",
            "scope-request-message",
            "scope-request-facts",
        ] {
            let bounds = visual
                .debug_bounds(part)
                .unwrap_or_else(|| panic!("missing {part} at {width}"));
            assert!(
                bounds.left() >= card.left() && bounds.right() <= card.right() + px(1.0),
                "{part} overflows at {width}: {bounds:?} vs {card:?}"
            );
            assert!(
                bounds.top() >= card.top() && bounds.bottom() <= card.bottom() + px(1.0),
                "{part} spills out of the card at {width}: {bounds:?} vs {card:?}"
            );
        }
        // Text squeezed by its row wraps one character per line.
        let unit = visual.debug_bounds("scope-request-unit").unwrap();
        assert!(
            unit.size.width > px(120.0),
            "unit label squeezed at {width}: {unit:?}"
        );
        let short = visual.debug_bounds("scope-request-path-text-1").unwrap();
        assert!(
            short.size.width > px(120.0),
            "path squeezed at {width}: {short:?}"
        );
        assert!(
            short.size.height < px(30.0),
            "a short path wraps at {width}: {short:?}"
        );
        let long = visual.debug_bounds("scope-request-path-text-0").unwrap();
        assert!(
            long.size.width > px(300.0),
            "long path squeezed at {width}: {long:?}"
        );
        assert!(visual.debug_bounds("scope-request-state").is_none());
        let bar = visual.debug_bounds("failure-decision-bar").unwrap();
        let grant = visual.debug_bounds("failure-grant-scope").unwrap();
        assert!(grant.left() >= bar.left() && grant.right() <= bar.right());
        assert!(bar.bottom() <= px(height), "bar off screen at {width}");
        assert!(
            visual.debug_bounds("failure-retry").is_none(),
            "the host offered no plain retry"
        );
        visual.debug_bounds("failure-revision").unwrap();
        visual.debug_bounds("failure-abort").unwrap();
        let first = visual.debug_bounds("failure-choice-0").unwrap();
        let choices = visual.debug_bounds("failure-choices").unwrap();
        assert!(card.bottom() <= choices.top(), "the request comes first");
        assert!(first.top() >= choices.top());
    }
}

#[gpui::test]
fn a_stale_scope_request_says_so_and_offers_no_grant(cx: &mut TestAppContext) {
    let (_model, visual) = parked_scope_window(cx, "stale", (1440.0, 900.0));
    visual.debug_bounds("scope-request").unwrap();
    visual.debug_bounds("scope-request-state").unwrap();
    assert!(visual.debug_bounds("failure-grant-scope").is_none());
    visual.debug_bounds("failure-revision").unwrap();
    visual.debug_bounds("failure-abort").unwrap();
}

#[gpui::test]
fn granting_without_a_core_reports_why_and_keeps_the_checkpoint(cx: &mut TestAppContext) {
    let (model, visual) = parked_scope_window(cx, "pending", (1440.0, 900.0));
    click(visual, "failure-grant-scope");
    model.read_with(visual, |m, cx| {
        let failure = &m.pulsations.read(cx).failure;
        assert!(failure.checkpoint.is_some());
        assert!(!failure.busy);
        assert!(
            failure
                .error
                .as_deref()
                .is_some_and(|error| error.contains("Scope grant unavailable")),
            "{:?}",
            failure.error
        );
    });
}

/// Feature 25, M2b: a global child's synthesis carries the `Global` and
/// `Based on` badges, `Remove from this project` and `Detach`; a box waiting
/// for a newer version says so; the header offers the global pipelines the
/// project does not show, and `Add` lists them.
#[gpui::test]
fn global_and_child_pipelines_show_their_badges_and_actions(cx: &mut TestAppContext) {
    use ikore_core::custom_pipelines::{
        AgentStep, CustomPipelineDefinitionV1, CustomPipelineView, GlobalPipelineSummary,
        PipelineStep, PipelineTrigger, StepAgent, StepOutput, StepTransport,
    };
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    select(&model, visual, AppView::Agents);
    let view = CustomPipelineView {
        id: "child".into(),
        scope: "global".into(),
        project_id: None,
        name: "Shared review plus".into(),
        description: String::new(),
        current_version: 3,
        adopted_version: 2,
        position: 0,
        ready: true,
        needs_edit_reason: None,
        archived: false,
        definition: CustomPipelineDefinitionV1 {
            schema_version: 1,
            trigger: PipelineTrigger::Manual,
            steps: vec![PipelineStep::Agent(AgentStep {
                id: "review".into(),
                label: "Review".into(),
                agent: StepAgent {
                    transport: StepTransport::Cli,
                    backend: Some("codex".into()),
                    model: Some("gpt-5".into()),
                    model_profile: None,
                    thinking_level: "default".into(),
                    api_route_id: None,
                },
                instructions: "Review.".into(),
                output: StepOutput::Text,
            })],
        },
        run_count: 0,
        updated_at: "now".into(),
        parent_pipeline_id: Some("parent".into()),
        parent_name: Some("Shared review".into()),
        adopted_parent_version: Some(4),
        children: 0,
        projects: 2,
        trigger_override: None,
    };
    model.update(visual, |model, cx| {
        model.agents.update(cx, |agents, _| {
            agents.pipelines.project_id = "layout-project".into();
            agents.pipelines.pipelines = vec![view.clone()];
            agents.pipelines.available_globals = vec![GlobalPipelineSummary {
                id: "other".into(),
                name: "Release notes".into(),
                description: "Drafts the notes.".into(),
                current_version: 1,
                parent_name: None,
            }];
        });
        model.select_canvas_pipeline("child".into(), cx);
    });
    visual.run_until_parked();
    for selector in [
        "agents-pipeline-badges",
        "pipeline-remove",
        "pipeline-detach",
        "pipeline-move-to-project",
        "agents-pipeline-behind",
        "agents-add-global-pipeline",
    ] {
        assert!(visual.debug_bounds(selector).is_some(), "{selector}");
    }
    for selector in ["pipeline-make-global", "pipeline-create-child"] {
        assert!(visual.debug_bounds(selector).is_none(), "{selector}");
    }
    click(visual, "agents-add-global-pipeline");
    assert!(visual.debug_bounds("global-pipeline:other").is_some());
    assert!(model.read_with(visual, |model, cx| {
        let agents = model.agents.read(cx);
        agents.show_global_pipelines && agents.selected_pipeline.is_none()
    }));
    assert_eq!(
        crate::agents_view::pipeline_editor::parent_label(&view).as_deref(),
        Some("Shared review v4")
    );
}

/// Feature 25, M3: the editor offers `Manual`, `Stage passed` with its stage
/// and `Pulsation done`, and keeps the pipeline's own trigger when editing; a
/// global pipeline's synthesis shows this project's trigger with `Change`; a
/// run an event started says so, with "Source pulsation failed".
#[gpui::test]
fn pipeline_triggers_show_in_the_editor_the_synthesis_and_the_runs(cx: &mut TestAppContext) {
    use ikore_core::custom_pipelines::{
        CustomPipelineDefinitionV1, CustomPipelineView, DefaultEvent, GateStep, PipelineRunStatus,
        PipelineRunView, PipelineStep, PipelineTrigger,
    };
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    select(&model, visual, AppView::Agents);
    let after_code = PipelineTrigger::DefaultEvent {
        event: DefaultEvent::StagePassed,
        stage: Some("Code".into()),
    };
    let done = PipelineTrigger::DefaultEvent {
        event: DefaultEvent::PulsationDone,
        stage: None,
    };
    let definition = CustomPipelineDefinitionV1 {
        schema_version: 1,
        trigger: after_code.clone(),
        steps: vec![PipelineStep::Gate(GateStep {
            id: "look".into(),
            label: "Look".into(),
            prompt: "Read it.".into(),
        })],
    };
    let view = CustomPipelineView {
        id: "shared".into(),
        scope: "global".into(),
        project_id: None,
        name: "Shared checks".into(),
        description: String::new(),
        current_version: 1,
        adopted_version: 1,
        position: 0,
        ready: true,
        needs_edit_reason: None,
        archived: false,
        definition: definition.clone(),
        run_count: 1,
        updated_at: "now".into(),
        parent_pipeline_id: None,
        parent_name: None,
        adopted_parent_version: None,
        children: 0,
        projects: 2,
        trigger_override: Some(done.clone()),
    };
    let run = PipelineRunView {
        id: "run-1".into(),
        pipeline_id: "shared".into(),
        version: 1,
        trigger_kind: "default_event".into(),
        status: PipelineRunStatus::WaitingGate,
        current_step: 0,
        error: None,
        created_at: "2026-11-23T10:00:00Z".into(),
        updated_at: "2026-11-23T10:00:00Z".into(),
        finished_at: None,
        definition,
        steps: Vec::new(),
        source_pulsation_id: Some("pulse".into()),
        source_pulsation_title: Some("Review tab navigation and artifact layout".into()),
        trigger_label: Some("Stage passed \u{b7} Code".into()),
        source_pulsation_failed: true,
    };
    model.update(visual, |model, cx| {
        model.agents.update(cx, |agents, _| {
            agents.pipelines.project_id = "layout-project".into();
            agents.pipelines.pipelines = vec![view.clone()];
            agents.pipelines.runs = vec![run];
        });
        model.select_canvas_pipeline("shared".into(), cx);
    });
    visual.run_until_parked();
    for selector in [
        "agents-pipeline-trigger",
        "pipeline-trigger-change",
        "pipeline-run-row:1",
        "pipeline-run-trigger",
        "pipeline-run-source-failed",
    ] {
        assert!(visual.debug_bounds(selector).is_some(), "{selector}");
    }
    assert!(visual.debug_bounds("pipeline-trigger-here").is_none());
    // A long pulsation title wraps; the badge stays inside the run's card.
    let card = visual.debug_bounds("pipeline-run-detail").unwrap();
    for selector in ["pipeline-run-trigger", "pipeline-run-source-failed"] {
        let bounds = visual.debug_bounds(selector).unwrap();
        assert!(
            bounds.right() <= card.right(),
            "{selector} {bounds:?} {card:?}"
        );
    }
    // The button stays inside the synthesis, next to a text that wraps.
    let panel = visual.debug_bounds("agents-pipeline-synthesis").unwrap();
    let change = visual.debug_bounds("pipeline-trigger-change").unwrap();
    assert!(change.right() <= panel.right(), "{change:?} {panel:?}");
    click(visual, "pipeline-trigger-change");
    assert!(
        model.read_with(visual, |model, cx| {
            model.agents.read(cx).editing_pipeline_trigger.is_some()
        }),
        "Change opens the choices"
    );
    for selector in [
        "pipeline-trigger-here",
        "pipeline-trigger-here-manual",
        "pipeline-trigger-here-stage-passed",
        "pipeline-trigger-here-pulsation-done",
        "pipeline-trigger-reset",
    ] {
        assert!(visual.debug_bounds(selector).is_some(), "{selector}");
    }
    // `Pulsation done` takes no stage.
    assert!(
        visual
            .debug_bounds("pipeline-trigger-here-stage:Code")
            .is_none()
    );
    assert!(model.read_with(visual, |model, cx| {
        model.agents.read(cx).editing_pipeline_trigger.as_deref() == Some("shared")
    }));

    // The editor starts from the pipeline's own trigger, not this project's.
    model.update(visual, |model, cx| model.edit_pipeline("shared".into(), cx));
    visual.run_until_parked();
    assert!(
        model.read_with(visual, |model, _| {
            model
                .pipeline_editor
                .as_ref()
                .map(|editor| editor.trigger.clone())
        }) == Some(after_code)
    );
    for selector in [
        "pipeline-editor-trigger-manual",
        "pipeline-editor-trigger-stage-passed",
        "pipeline-editor-trigger-pulsation-done",
        "pipeline-editor-trigger-stage:Intake",
        "pipeline-editor-trigger-stage:SPECIFICATION",
        "pipeline-editor-trigger-stage:Test",
    ] {
        assert!(visual.debug_bounds(selector).is_some(), "{selector}");
    }
    assert!(
        visual
            .debug_bounds("pipeline-editor-trigger-stage:Integrator")
            .is_none()
    );
    click(visual, "pipeline-editor-trigger-stage:Intake");
    assert!(
        model.read_with(visual, |model, _| {
            model
                .pipeline_editor
                .as_ref()
                .map(|editor| editor.trigger.clone())
        }) == Some(PipelineTrigger::DefaultEvent {
            event: DefaultEvent::StagePassed,
            stage: Some("Intake".into()),
        })
    );
    click(visual, "pipeline-editor-trigger-pulsation-done");
    assert!(
        model.read_with(visual, |model, _| {
            model
                .pipeline_editor
                .as_ref()
                .map(|editor| editor.trigger.clone())
        }) == Some(done)
    );
}

#[gpui::test]
fn forge_redesigned_tabs_fit_supported_window_sizes_and_wire_direct_controls(
    cx: &mut TestAppContext,
) {
    use crate::forge::{BacklogKindFilter, ForgeAction, ForgeSection};
    use ikore_core::feature_forge::*;
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    model.update(visual, |m, cx| {
        m.forge.snapshot.current_wave_limit = 3;
        m.forge.snapshot.features = vec![
            Feature {
                id: "feature-0".into(),
                project_id: "layout-project".into(),
                title: "Local storage foundation".into(),
                original_idea: "Persist notes in SQLite".into(),
                revision: 2,
                approved_revision: Some(2),
                state: FeatureState::Approved,
                brief: FeatureBrief {
                    objective: "Store notes locally".into(),
                    audience: "Developers".into(),
                    behavior: "Save and retrieve notes quickly".into(),
                    scope: "Local SQLite tables".into(),
                    criteria: vec![AcceptanceCriterion {
                        id: "fc-1".into(),
                        description: "Notes survive restart".into(),
                    }],
                    tasks: vec![TaskDraft {
                        title: "Create schema".into(),
                        kind: TaskKind::Foundation,
                        description: "Create local SQLite schema".into(),
                        criteria: vec![AcceptanceCriterion {
                            id: "tc-1".into(),
                            description: "Schema migrates cleanly".into(),
                        }],
                        dependencies: vec![],
                        feature_id: None,
                    }],
                    coverage: "complete".into(),
                    ..Default::default()
                },
                evaluation: FeatureEvaluation {
                    impact: Some(5),
                    confidence: Some(4),
                    effort: Some(2),
                    ..Default::default()
                },
                structural_level: 0,
                wave: Some(Wave::Current),
                position: 0,
                archive_reason: None,
            },
            Feature {
                id: "feature-1".into(),
                project_id: "layout-project".into(),
                title: "Tag and search notes".into(),
                original_idea: "Filter notes with tags".into(),
                revision: 2,
                approved_revision: Some(2),
                state: FeatureState::Executing,
                brief: FeatureBrief {
                    objective: "Tag and search notes".into(),
                    audience: "Developers".into(),
                    behavior: "Tag notes and query them".into(),
                    scope: "Tagging and FTS".into(),
                    criteria: vec![AcceptanceCriterion {
                        id: "c1".into(),
                        description: "Tags filter notes".into(),
                    }],
                    tasks: vec![],
                    coverage: "complete".into(),
                    ..Default::default()
                },
                evaluation: FeatureEvaluation {
                    impact: Some(4),
                    confidence: Some(4),
                    effort: Some(2),
                    ..Default::default()
                },
                structural_level: 2,
                wave: Some(Wave::Current),
                position: 1,
                archive_reason: None,
            },
            Feature {
                id: "feature-2".into(),
                project_id: "layout-project".into(),
                title: "Note templates".into(),
                original_idea: "Reusable templates".into(),
                revision: 1,
                approved_revision: None,
                state: FeatureState::InTriage,
                brief: FeatureBrief {
                    objective: "Provide ADR templates".into(),
                    ..Default::default()
                },
                evaluation: FeatureEvaluation::default(),
                structural_level: 1,
                wave: None,
                position: 2,
                archive_reason: None,
            },
        ];
        m.forge.snapshot.dependencies = vec![FeatureDependency {
            feature_id: "feature-1".into(),
            prerequisite_id: "feature-0".into(),
            rationale: "Needs local SQLite schema".into(),
            origin: DependencyOrigin::Human,
        }];
        m.forge.snapshot.tasks = vec![
            WorkTask {
                id: "task-bug".into(),
                project_id: "layout-project".into(),
                revision: 1,
                draft: TaskDraft {
                    title: "Fix expired token check".into(),
                    kind: TaskKind::Bug,
                    description: "Reject expired tokens".into(),
                    criteria: vec![AcceptanceCriterion {
                        id: "tb-1".into(),
                        description: "Expired tokens return 401".into(),
                    }],
                    dependencies: vec![],
                    feature_id: None,
                },
                feature_revision: None,
                state: "ready".into(),
                active_pulsation_id: None,
                completed_pulsation_id: None,
            },
            WorkTask {
                id: "task-done".into(),
                project_id: "layout-project".into(),
                revision: 1,
                draft: TaskDraft {
                    title: "Add tag query".into(),
                    kind: TaskKind::Feature,
                    description: "Query notes by tag".into(),
                    criteria: vec![AcceptanceCriterion {
                        id: "c1".into(),
                        description: "Tags filter notes".into(),
                    }],
                    dependencies: vec![],
                    feature_id: Some("feature-1".into()),
                },
                feature_revision: Some(2),
                state: "done".into(),
                active_pulsation_id: None,
                completed_pulsation_id: Some("pulse-a".into()),
            },
            WorkTask {
                id: "task-ready".into(),
                project_id: "layout-project".into(),
                revision: 1,
                draft: TaskDraft {
                    title: "Wire tag UI".into(),
                    kind: TaskKind::Feature,
                    description: "Add tag pills in UI".into(),
                    criteria: vec![AcceptanceCriterion {
                        id: "c2".into(),
                        description: "Tag pills render".into(),
                    }],
                    dependencies: vec!["task-done".into()],
                    feature_id: Some("feature-1".into()),
                },
                feature_revision: Some(2),
                state: "ready".into(),
                active_pulsation_id: None,
                completed_pulsation_id: None,
            },
        ];
        m.forge.snapshot.integration_evidence = vec![TaskEvidence {
            task_id: "task-done".into(),
            criterion_ids: vec!["c1".into()],
            artifact_positions: vec![0],
            summary: "Integrated tag query with passing unit check.".into(),
        }];
        m.forge.selected_feature = Some("feature-1".into());
        m.select_view(AppView::Forge, cx);
    });

    for (width, height) in [(960.0, 640.0), (1440.0, 900.0)] {
        visual.simulate_resize(size(px(width), px(height)));

        // 1. Backlog
        model.update(visual, |m, cx| {
            m.forge_action(ForgeAction::Section(ForgeSection::Backlog), cx);
        });
        visual.run_until_parked();
        for selector in [
            "backlog-filter-kind-all",
            "backlog-filter-kind-bug",
            "backlog-filter-state-ready",
            "backlog-select-all-ready",
            "task-card-task-bug",
            "batch-composer",
            "dispatch-batch",
        ] {
            let bounds = visual.debug_bounds(selector).unwrap_or_else(|| {
                panic!("missing backlog selector {selector} at {width}x{height}")
            });
            assert!(
                bounds.right() <= px(width) && bounds.bottom() <= px(height),
                "{selector} overflows {width}x{height}: {bounds:?}"
            );
        }

        // 2. Incubation
        model.update(visual, |m, cx| {
            m.forge.selected_feature = Some("feature-0".into());
            m.forge_action(ForgeAction::Section(ForgeSection::Incubation), cx);
        });
        visual.run_until_parked();
        for selector in [
            "forge-detail",
            "forge-incubation-aside",
            "forge-scope-readiness",
            "forge-message-composer",
            "send-feature-message",
            "approve-feature",
        ] {
            let bounds = visual.debug_bounds(selector).unwrap_or_else(|| {
                panic!("missing incubation selector {selector} at {width}x{height}")
            });
            assert!(
                bounds.right() <= px(width) && bounds.bottom() <= px(height),
                "{selector} overflows {width}x{height}: {bounds:?}"
            );
        }

        // 3. Triage (scrolls vertically inside forge-detail; check horizontal containment)
        model.update(visual, |m, cx| {
            m.forge.selected_feature = Some("feature-2".into());
            m.forge_action(ForgeAction::Section(ForgeSection::Triage), cx);
        });
        visual.run_until_parked();
        let detail = visual.debug_bounds("forge-detail").unwrap();
        assert!(detail.right() <= px(width) && detail.bottom() <= px(height));
        for selector in [
            "structural-level",
            "rating-impact-5",
            "rating-confidence-4",
            "rating-effort-2",
            "triage-approve",
        ] {
            let bounds = visual.debug_bounds(selector).unwrap_or_else(|| {
                panic!("missing triage selector {selector} at {width}x{height}")
            });
            assert!(
                bounds.left() >= detail.left() && bounds.right() <= detail.right(),
                "{selector} overflows triage column at {width}x{height}: {bounds:?} vs {detail:?}"
            );
        }

        // 4. Dependencies
        model.update(visual, |m, cx| {
            m.forge.selected_feature = Some("feature-1".into());
            m.forge_action(ForgeAction::Section(ForgeSection::Dependencies), cx);
        });
        visual.run_until_parked();
        let detail = visual.debug_bounds("forge-detail").unwrap();
        assert!(detail.right() <= px(width) && detail.bottom() <= px(height));
        for selector in [
            "dependency-focus-toggle",
            "feature-graph",
            "graph-node-feature-0",
            "graph-node-feature-1",
            "remove-edge-feature-0",
        ] {
            let bounds = visual.debug_bounds(selector).unwrap_or_else(|| {
                panic!("missing dependencies selector {selector} at {width}x{height}")
            });
            assert!(
                bounds.left() >= detail.left() && bounds.right() <= detail.right(),
                "{selector} overflows dependencies column at {width}x{height}: {bounds:?} vs {detail:?}"
            );
        }

        // 5. Waves
        model.update(visual, |m, cx| {
            m.forge_action(ForgeAction::Section(ForgeSection::Waves), cx);
        });
        visual.run_until_parked();
        let detail = visual.debug_bounds("forge-detail").unwrap();
        assert!(detail.right() <= px(width) && detail.bottom() <= px(height));
        for selector in [
            "wave-Current",
            "wave-Next",
            "wave-Horizon",
            "wave-limit-dec",
            "wave-limit-inc",
            "set-wave-limit",
        ] {
            let bounds = visual
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("missing waves selector {selector} at {width}x{height}"));
            assert!(
                bounds.left() >= detail.left() && bounds.right() <= detail.right(),
                "{selector} overflows waves column at {width}x{height}: {bounds:?} vs {detail:?}"
            );
        }

        // 6. Delivery
        model.update(visual, |m, cx| {
            m.forge.selected_feature = Some("feature-1".into());
            m.forge_action(ForgeAction::Section(ForgeSection::Delivery), cx);
        });
        visual.run_until_parked();
        let detail = visual.debug_bounds("forge-detail").unwrap();
        assert!(detail.right() <= px(width) && detail.bottom() <= px(height));
        for selector in [
            "compose-feature-tasks",
            "delivery-evidence-task-done",
            "complete-feature",
        ] {
            let bounds = visual.debug_bounds(selector).unwrap_or_else(|| {
                panic!("missing delivery selector {selector} at {width}x{height}")
            });
            assert!(
                bounds.left() >= detail.left() && bounds.right() <= detail.right(),
                "{selector} overflows delivery column at {width}x{height}: {bounds:?} vs {detail:?}"
            );
        }
    }

    // Verify interactive clicks wire properly at 1440x900
    model.update(visual, |m, cx| {
        m.forge_action(ForgeAction::Section(ForgeSection::Backlog), cx);
    });
    visual.run_until_parked();
    click(visual, "backlog-filter-kind-bug");
    model.read_with(visual, |m, _| {
        assert_eq!(m.forge.backlog_kind_filter, BacklogKindFilter::Bug);
    });
    click(visual, "toggle-task-details-task-bug");
    model.read_with(visual, |m, _| {
        assert!(m.forge.expanded_backlog_tasks.contains("task-bug"));
    });
    click(visual, "backlog-select-all-ready");
    model.read_with(visual, |m, _| {
        assert!(m.forge.selected_tasks.contains("task-bug"));
        assert!(!m.forge.selected_tasks.contains("task-ready"));
    });

    model.update(visual, |m, cx| {
        m.forge.selected_feature = Some("feature-0".into());
        m.forge_action(ForgeAction::Section(ForgeSection::Incubation), cx);
    });
    visual.run_until_parked();
    click(visual, "spec-section-toggle-overview");
    model.read_with(visual, |m, _| {
        assert!(m.forge.collapsed_spec_sections.contains("overview"));
    });

    model.update(visual, |m, cx| {
        m.forge_action(ForgeAction::Section(ForgeSection::Triage), cx);
    });
    visual.run_until_parked();
    click(visual, "rating-impact-5");
    click(visual, "rating-confidence-4");
    click(visual, "rating-effort-2");
    click(visual, "structural-level-2");
    model.read_with(visual, |m, _| {
        assert_eq!(m.forge.impact, 5);
        assert_eq!(m.forge.confidence, 4);
        assert_eq!(m.forge.effort, 2);
        assert_eq!(m.forge.level, 2);
    });

    model.update(visual, |m, cx| {
        m.forge_action(ForgeAction::Section(ForgeSection::Dependencies), cx);
    });
    visual.run_until_parked();
    click(visual, "dependency-focus-toggle");
    model.read_with(visual, |m, _| {
        assert!(m.forge.dependencies_focus_mode);
    });

    model.update(visual, |m, cx| {
        m.forge_action(ForgeAction::Section(ForgeSection::Waves), cx);
    });
    visual.run_until_parked();
    click(visual, "wave-limit-inc");
    model.read_with(visual, |m, cx| {
        assert_eq!(m.forge.wave_limit.read(cx).value(), "4");
    });
}

#[gpui::test]
fn stage_verifiers_canvas_labs_toggle_synthesis_width_and_checkpoints(cx: &mut TestAppContext) {
    use ikore_core::custom_pipelines::{
        FindingSeverity, StageVerdictV1, StepAgent, StepTransport, Verdict, VerdictFinding,
    };
    use ikore_core::persistence_service::{StageFailureCheckpoint, StageFailureDetail};
    use ikore_core::stage_verifiers::{
        StageVerifierRunView, StageVerifierView, StageVerifiersSnapshot,
        VerificationErrorCheckpoint, VerificationErrorDecision,
    };

    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.update(|_, cx| crate::failure_checkpoint::register_failure_keybindings(cx));
    let long_label = "L".repeat(60);
    let long_finding = "F".repeat(600);
    let long_summary = "S".repeat(600);

    model.update(visual, |m, cx| {
        m.agents.update(cx, |agents, _| {
            agents.verifiers = StageVerifiersSnapshot {
                project_id: "layout-project".into(),
                enabled_by_labs: false,
                verifiers: vec![StageVerifierView {
                    id: "ver-ui-1".into(),
                    project_id: "layout-project".into(),
                    stage: "CODE".into(),
                    label: long_label.clone(),
                    position: 0,
                    enabled: true,
                    reject_limit: 2,
                    agent: StepAgent {
                        transport: StepTransport::Cli,
                        backend: Some("codex".into()),
                        model: Some("gpt-5".into()),
                        model_profile: None,
                        thinking_level: "default".into(),
                        api_route_id: None,
                    },
                    instructions: "Check invariants cleanly.".into(),
                    version: 1,
                    created_at: "now".into(),
                    updated_at: "now".into(),
                    run_count: 1,
                }],
                runs: vec![StageVerifierRunView {
                    id: "vrun-1".into(),
                    verifier_id: "ver-ui-1".into(),
                    verifier_label: long_label.clone(),
                    stage: "CODE".into(),
                    pulsation_id: "pulse-layout".into(),
                    stage_job_id: "job-code".into(),
                    stage_generation: 1,
                    attempt: 1,
                    recovery_count: 0,
                    status: "rejected".into(),
                    reject_limit: 2,
                    advisory: false,
                    terminal_run_id: None,
                    llm_execution_id: None,
                    verdict: Some(StageVerdictV1 {
                        verdict: Verdict::Reject,
                        summary: long_summary.clone(),
                        findings: vec![VerdictFinding {
                            id: "f1".into(),
                            severity: FindingSeverity::Blocking,
                            message: long_finding.clone(),
                            file: Some("src/lib.rs".into()),
                            line: Some(42),
                            suggestion: Some("Keep it wrapped.".into()),
                            file_not_found: false,
                        }],
                    }),
                    created_at: "now".into(),
                    started_at: None,
                    finished_at: None,
                    error: None,
                }],
            };
        });
    });

    // Labs OFF: verifier node and "+ Verifier" trigger are hidden.
    select(&model, visual, AppView::Agents);
    assert!(
        visual
            .debug_bounds("agents-node:verifier:ver-ui-1")
            .is_none()
    );
    assert!(
        visual
            .debug_bounds("agents-stage-add-verifier:code")
            .is_none()
    );

    // Labs ON: verifier node and add-verifier triggers appear; synthesis renders within 300 px.
    model.update(visual, |m, cx| {
        m.agents.update(cx, |agents, _| {
            agents.verifiers.enabled_by_labs = true;
            agents.selected_agent = Some(AgentIdentifier::StageVerifier("ver-ui-1".into()));
            agents.detail_tab = crate::agents::AgentDetailTab::History;
        });
        cx.notify();
    });
    visual.simulate_resize(size(px(960.0), px(700.0)));
    visual.run_until_parked();
    assert!(
        visual
            .debug_bounds("agents-node:verifier:ver-ui-1")
            .is_some()
    );
    assert!(
        visual
            .debug_bounds("agents-stage-add-verifier:code")
            .is_some()
    );
    let synthesis = visual.debug_bounds("verifier-synthesis").unwrap();
    let history = visual
        .debug_bounds("agents-drawer:verifier-history")
        .unwrap();
    assert!(history.right() <= synthesis.right() + px(1.0));
    assert!(visual.debug_bounds("verifier-run-now").is_none());

    // Pulsations > Failure checkpoint: Continue anyway optional justification + >500-char block + findings card.
    let finding_run = model.read_with(visual, |m, cx| m.agents.read(cx).verifiers.runs.clone());
    model.update(visual, |m, cx| {
        m.pulsations.update(cx, |pulsations, _| {
            pulsations.pulsations = vec![Pulsation {
                terminal_reason: None,
                id: "pulse-fail".into(),
                project_id: "layout-project".into(),
                feature_name: "Verifier rejection test".into(),
                stage: PulsationStage::Code,
                status: PulsationStatus::Checkpoint,
                created_at: "now".into(),
                updated_at: "now".into(),
            }];
            pulsations.active_pulsation_id = Some("pulse-fail".into());
            pulsations.failure_checkpoints = vec![StageFailureCheckpoint {
                checkpoint_id: "cp-fail-1".into(),
                pulsation_id: "pulse-fail".into(),
                project_id: "layout-project".into(),
                feature_name: "Verifier rejection test".into(),
                stage: PulsationStage::Code,
                failure_class: "verification_rejected".into(),
                error_code: "verification_rejected".into(),
                diagnostic: "Blocking verifier rejected the output.".into(),
                allow_retry: true,
                allow_revision: true,
                allow_continue_anyway: true,
                revision_stage: Some(PulsationStage::Code),
                attempts: 3,
                state_version: 1,
                created_at: "now".into(),
                allowed_revision_targets: vec![PulsationStage::Code],
                remaining_extra_invocations: None,
                stage_execution_id: None,
                scope_request: None,
            }];
            pulsations
                .failure
                .load_checkpoint(pulsations.failure_checkpoints.first().cloned());
            pulsations.failure.detail = Some(StageFailureDetail {
                checkpoint_id: "cp-fail-1".into(),
                raw_output: None,
                validation_error: Some("Blocking verifier rejected the output.".into()),
                validation_context: None,
            });
            pulsations.failure.verification_findings = finding_run;
            pulsations.detail_section = PulsationDetailSection::Review;
        });
        cx.notify();
    });
    select(&model, visual, AppView::Pulsations);
    visual.simulate_resize(size(px(960.0), px(720.0)));
    visual.run_until_parked();
    assert!(visual.debug_bounds("failure-continue-anyway").is_some());
    assert!(
        visual
            .debug_bounds("failure-override-justification")
            .is_some()
    );
    let detail = visual.debug_bounds("failure-detail").unwrap();
    let findings_card = visual.debug_bounds("brief-verification-findings").unwrap();
    assert!(findings_card.right() <= detail.right() + px(1.0));
    assert!(
        visual
            .debug_bounds("failure-override-justification-error")
            .is_none()
    );

    // 501 characters triggers inline error and refuses submission.
    model.update(visual, |m, cx| {
        m.failure_override_justification_input
            .update(cx, |input, cx| input.set_value("J".repeat(501), cx));
        m.resolve_stage_failure(
            ikore_core::persistence_service::StageFailureDecision::ContinueAnyway,
            cx,
        );
    });
    visual.run_until_parked();
    assert!(
        visual
            .debug_bounds("failure-override-justification-error")
            .is_some()
    );

    // Verification error checkpoint shows all three actions.
    model.update(visual, |m, cx| {
        m.pulsations.update(cx, |pulsations, _| {
            pulsations.failure_checkpoints.clear();
            pulsations.failure.load_checkpoint(None);
            pulsations.verification_error_checkpoints = vec![VerificationErrorCheckpoint {
                checkpoint_id: "vcp-err-1".into(),
                pulsation_id: "pulse-fail".into(),
                project_id: "layout-project".into(),
                feature_name: "Verifier error test".into(),
                verifier_run_id: "vrun-err".into(),
                verifier_id: "ver-ui-1".into(),
                verifier_label: long_label,
                stage: PulsationStage::Code,
                error_message: "CLI process exited with status 1.".into(),
                attempt: 1,
                state_version: 1,
                created_at: "now".into(),
            }];
            pulsations.detail_section = PulsationDetailSection::Review;
        });
        cx.notify();
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("verification-error-panel").is_some());
    assert!(visual.debug_bounds("verification-error-retry").is_some());
    assert!(visual.debug_bounds("verification-error-skip").is_some());
    assert!(visual.debug_bounds("verification-error-abort").is_some());
    let _ = VerificationErrorDecision::SkipVerifier;
}

#[gpui::test]
fn settings_git_page_renders_status_rules_errors_and_fixed_guardrails(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| {
        let mut model = fixture(cx);
        with_settings_items(&mut model, cx);
        model
    });
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    open_settings(&model, visual, Some(SettingsPage::Git));

    for selector in [
        "settings-row:git-tool-enabled",
        "settings-row:git-status",
        "settings-row:git-preview",
        "settings-row:git-commit-rules",
        "settings-row:git-push-pull-rules",
        "settings-row:git-fixed-rules",
        "git-refresh-status",
        "git-preview-refresh",
        "git-rules-save",
        "git-rules-restore",
    ] {
        assert!(
            visual.debug_bounds(selector).is_some(),
            "missing Settings > Git selector {selector}"
        );
    }

    // Dry-run commit preview and queue entries render without any commit execution button.
    model.update(visual, |m, cx| {
        use ikore_contracts::git_tool::{
            CommitPlanGroupV1, CommitPlanV1, GitCommitPreviewV1, PulsationCommitQueueEntryV1,
            PulsationCommitQueueState, RuleViolation,
        };
        m.settings.update(cx, |settings, _| {
            settings.git.queue = vec![PulsationCommitQueueEntryV1 {
                pulsation_id: "pulse-qa-1".into(),
                project_id: "layout-project".into(),
                feature_name: "Preview QA feature".into(),
                completed_at: "2026-11-26T00:00:00Z".into(),
                state: PulsationCommitQueueState::Pending,
                skip_reason: None,
                branch: None,
                head_commit: None,
                updated_at: "2026-11-26T00:00:00Z".into(),
            }];
            settings.git.selected_preview_pulsation_id = Some("pulse-qa-1".into());
            settings.git.preview = Some(GitCommitPreviewV1 {
                project_id: "layout-project".into(),
                pulsation_id: "pulse-qa-1".into(),
                feature_name: "Preview QA feature".into(),
                queue_state: Some(PulsationCommitQueueState::Pending),
                already_in_head: false,
                plan: CommitPlanV1 {
                    project_id: "layout-project".into(),
                    pulsation_id: "pulse-qa-1".into(),
                    branch: "ikore/preview-qa".into(),
                    base_head: "abc123456789".into(),
                    groups: vec![CommitPlanGroupV1 {
                        task_ids: vec!["task-1".into()],
                        commit_type: "feat".into(),
                        scope: Some("preview-qa".into()),
                        subject: "implement preview widget".into(),
                        body: "Dry run preview body.".into(),
                        trailers: vec!["Ikore-Pulsation: pulse-qa-1".into()],
                        paths: vec!["src/lib.rs".into()],
                        counted_files: 1,
                        counted_lines: 12,
                    }],
                    warnings: vec![],
                    blocking_violations: vec![RuleViolation {
                        code: "missing_tests".into(),
                        path: Some("src/lib.rs".into()),
                        task_id: Some("task-1".into()),
                        message: "Code changed without test updates.".into(),
                    }],
                },
                rules_version: 1,
                rules_sha256: "deadbeef".into(),
            });
        });
        cx.notify();
    });
    visual.run_until_parked();
    for selector in [
        "git-queue-entry-pulse-qa-1",
        "git-preview-select-pulse-qa-1",
        "git-preview-skip-pulse-qa-1",
        "git-preview-plan",
        "git-preview-group-0",
    ] {
        assert!(
            visual.debug_bounds(selector).is_some(),
            "missing Settings > Git preview selector {selector}"
        );
    }

    // Invalid integer in subject max chars blocks save locally and displays field error.
    model.update(visual, |m, cx| {
        let subject_max = m
            .settings
            .read(cx)
            .git
            .form
            .as_ref()
            .expect("git form initialized")
            .subject_max_chars
            .clone();
        subject_max.update(cx, |input, cx| input.set_value("not-a-number", cx));
        m.save_git_rules(cx);
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("git-rules-error").is_some());
    assert!(visual.debug_bounds("git-validation-summary").is_some());
    assert!(
        visual
            .debug_bounds("git-field-error:message.subjectMaxChars")
            .is_some()
    );

    // Zero literal backticks inside git_settings_ui.rs strings/source.
    assert!(
        !include_str!("git_settings_ui.rs").contains('`'),
        "git_settings_ui.rs must never contain literal backticks"
    );

    // Without an active project, Settings > Git displays a plain project prompt cleanly.
    let (no_proj_model, no_proj_visual) = cx.add_window_view(|_, cx| {
        let model = fixture(cx);
        model
            .workspace
            .update(cx, |workspace, _| workspace.project_id = None);
        model
    });
    no_proj_visual.simulate_resize(size(px(1280.0), px(800.0)));
    open_settings(&no_proj_model, no_proj_visual, Some(SettingsPage::Git));
    assert!(
        no_proj_visual
            .debug_bounds("settings-section:Project")
            .is_some()
    );
    assert!(
        no_proj_visual
            .debug_bounds("settings-row:git-commit-rules")
            .is_none()
    );
}
