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

fn specification_brief() -> hvc_core::orchestration::review_brief::ReviewBrief {
    use hvc_core::orchestration::composition::{InterfaceIntent, ProposalKind, VerificationMode};
    use hvc_core::orchestration::review_brief::*;
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
            "crates/hvc-core/src/usage.rs".into(),
            "crates/hvc-core/src/usage/export.rs".into(),
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
                label: "crates/hvc-ui/src/usage.rs".into(),
                text: "The Usage view gains an Export button that is disabled while a query loads.".into(),
                detail: None,
                refs: vec!["crates/hvc-ui/src/usage.rs".into(), "AC-6".into()],
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
                    root: "crates/hvc-core".into(),
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
                    root: "crates/hvc-ui".into(),
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
    brief: hvc_core::orchestration::review_brief::ReviewBrief,
) -> (Entity<AppModel>, &mut VisualTestContext) {
    use hvc_core::persistence_service::{HitlCheckpoint, HitlReviewDocument};
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.update(|_, cx| crate::hitl::register_hitl_keybindings(cx));
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
    use hvc_core::persistence_service::HitlCheckpoint;
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
    revision: Option<hvc_core::orchestration::review_brief::RevisionDiff>,
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
    use hvc_core::orchestration::review_brief::{
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
            "crates/hvc-ui/src/usage.rs",
            Some("intent"),
            Some("change"),
            Some("preserve"),
        ),
        change(
            ChangeKind::Removed,
            ChangeSubject::File,
            "crates/hvc-core/src/usage/legacy_export_kept_for_one_release_only.rs",
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
    use hvc_core::orchestration::review_brief::RevisionDiff;
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
    click(visual, "rail-settings");
    assert_eq!(
        model.read_with(visual, |m, _| m.active_view()),
        AppView::Agents
    );
    assert!(model.read_with(visual, |m, _| m.choice_menu.is_none()));
    click(visual, "rail-settings");
    assert_eq!(
        model.read_with(visual, |m, _| m.active_view()),
        AppView::Settings
    );
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
        assert_eq!(
            model.read_with(visual, |model, _| model.active_view()),
            AppView::Settings
        );
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
    select(&model, visual, AppView::Settings);
    click(visual, "header-context");
    assert_eq!(
        model.read_with(visual, |model, _| model.active_view()),
        AppView::Settings
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

#[gpui::test]
fn keyboard_shortcuts_follow_rail_order(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.update(|_, cx| crate::shell::register_navigation_keybindings(cx));
    visual.simulate_resize(size(px(960.0), px(640.0)));
    visual.run_until_parked();
    // Own the focus first so the keystrokes reach the shell's action handlers.
    click(visual, "rail-nav-home");
    for (index, view) in crate::shell::RAIL_VIEWS.iter().enumerate() {
        visual.simulate_keystrokes(&format!("ctrl-{}", index + 1));
        visual.run_until_parked();
        assert_eq!(
            model.read_with(visual, |model, _| model.active_view()),
            *view,
            "ctrl-{} did not select {view:?}",
            index + 1
        );
    }
    visual.simulate_keystrokes("ctrl-,");
    visual.run_until_parked();
    assert_eq!(
        model.read_with(visual, |model, _| model.active_view()),
        AppView::Settings
    );
    visual.simulate_keystrokes("ctrl-shift-]");
    visual.run_until_parked();
    assert_eq!(
        model.read_with(visual, |model, cx| model.settings.read(cx).settings_section),
        SettingsSection::Routes
    );
    let before = model.read_with(visual, |model, cx| model.context_rail_collapsed(cx));
    visual.simulate_keystrokes("ctrl-b");
    visual.run_until_parked();
    assert_ne!(
        model.read_with(visual, |model, cx| model.context_rail_collapsed(cx)),
        before
    );
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

#[gpui::test]
fn settings_section_strip_lives_inside_the_settings_panel(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    for (width, height) in [(960.0, 640.0), (1440.0, 900.0)] {
        visual.simulate_resize(size(px(width), px(height)));
        select(&model, visual, AppView::Settings);
        let rail = visual.debug_bounds("rail").unwrap();
        let header = visual.debug_bounds("header").unwrap();
        let strip = visual.debug_bounds("settings-section-strip").unwrap();
        assert!(strip.left() >= rail.right());
        assert!(strip.top() >= header.bottom());
        for selector in [
            "settings-section-connections",
            "settings-section-routes",
            "settings-section-runtime",
            "settings-section-usage",
        ] {
            let pill = visual.debug_bounds(selector).unwrap();
            assert!(pill.top() >= strip.top() && pill.bottom() <= strip.bottom());
        }
        click(visual, "settings-section-runtime");
        assert_eq!(
            model.read_with(visual, |model, cx| model.settings.read(cx).settings_section),
            SettingsSection::Runtime
        );
    }
    // Other views never show the strip.
    select(&model, visual, AppView::Vault);
    assert_absent(&model, visual, "settings-section-strip");
}

#[gpui::test]
fn context_rail_is_absent_on_home_and_present_on_workbench_views(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    visual.simulate_resize(size(px(1440.0), px(900.0)));
    for view in [
        AppView::Pulsations,
        AppView::Agents,
        AppView::Chat,
        AppView::Settings,
    ] {
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
            hvc_core::persistence_service::PulsationRuntimeSnapshot {
                details: Vec::new(),
                coordination: None,
                main_agents_count: 0,
                hitl_checkpoints: Vec::new(),
                pulsations,
                hitl_waiting_count: 0,
                failure_checkpoints: Vec::new(),
                failure_waiting_count: 0,
                background_agents_count: 0,
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
fn agents_header_shows_the_active_pulsation_pipeline(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| fixture(cx));
    select(&model, visual, AppView::Agents);
    for (width, height) in [(960.0, 640.0), (1440.0, 900.0)] {
        visual.simulate_resize(size(px(width), px(height)));
        visual.run_until_parked();
        let agents_header = visual.debug_bounds("agents-header").unwrap();
        let pipeline = visual.debug_bounds("agents-pipeline").unwrap();
        let refresh = visual.debug_bounds("agents-refresh").unwrap();
        assert!(
            pipeline.top() >= agents_header.top() && pipeline.bottom() <= agents_header.bottom(),
            "pipeline escaped header: {pipeline:?} vs {agents_header:?}"
        );
        // The stepper is centred on the window, not on whatever is left of the Refresh group.
        assert!(
            (pipeline.center().x - agents_header.center().x).abs() <= px(2.0),
            "pipeline off-centre at {width}: {pipeline:?} vs {agents_header:?}"
        );
        assert!(pipeline.right() <= refresh.left());
        assert!(refresh.right() <= px(width));
        let mut previous_right = pipeline.left();
        for selector in [
            "agents-pipeline-stage-intake",
            "agents-pipeline-stage-specification",
            "agents-pipeline-stage-contract",
            "agents-pipeline-stage-code",
            "agents-pipeline-stage-test",
            "agents-pipeline-stage-integrator",
        ] {
            let stage_bounds = visual.debug_bounds(selector).unwrap();
            assert!(
                stage_bounds.left() >= previous_right && stage_bounds.right() <= pipeline.right(),
                "{selector} out of order or bounds: {stage_bounds:?} vs {pipeline:?}"
            );
            previous_right = stage_bounds.right();
        }
    }
    // The stage · agent · state readout lives in the header. This selector was never drawn in
    // this window, so a direct lookup is enough (no fresh-window dance needed).
    assert!(visual.debug_bounds("agents-pipeline-current").is_none());
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
        model.read_with(visual, |model, cx| model
            .agents
            .read(cx)
            .selected_agent
            .clone()),
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

// ---------------------------------------------------------------------------
// ADR-024 M4: Contract, Test and Integrator briefs and the failure choices
// ---------------------------------------------------------------------------

/// The shared anatomy of a later-stage fixture: no revision, host words only.
fn later_brief(
    stage: PulsationStage,
    next_stage: Option<PulsationStage>,
    counts: &[&str],
    judgement: Vec<hvc_core::orchestration::review_brief::JudgementItem>,
    visual: hvc_core::orchestration::review_brief::StageVisual,
) -> hvc_core::orchestration::review_brief::ReviewBrief {
    hvc_core::orchestration::review_brief::ReviewBrief {
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
    kind: hvc_core::orchestration::review_brief::JudgementKind,
    label: &str,
    text: &str,
) -> hvc_core::orchestration::review_brief::JudgementItem {
    hvc_core::orchestration::review_brief::JudgementItem {
        kind,
        label: label.into(),
        text: text.into(),
        detail: Some("An agent-written explanation that is long enough to wrap inside the item at the small window size.".into()),
        refs: Vec::new(),
    }
}

fn contract_brief() -> hvc_core::orchestration::review_brief::ReviewBrief {
    use hvc_core::orchestration::composition::GroupIntent;
    use hvc_core::orchestration::review_brief::*;
    let group = |key: &str, intent, signature: &str| {
        BriefInterfaceGroup {
        key: key.into(),
        intent,
        signature: signature.into(),
        invariants: vec![
            "Returns rows in the order the Usage view shows them, so a spreadsheet matches the screen.".into(),
            "Never panics on an empty range.".into(),
        ],
        sources: vec!["crates/hvc-core/src/usage.rs".into()],
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

fn test_brief() -> hvc_core::orchestration::review_brief::ReviewBrief {
    use hvc_core::orchestration::composition::{CheckStatus, CoverageKind};
    use hvc_core::orchestration::review_brief::*;
    let row = |id: &str, coverage, tests: &[&str]| {
        BriefCoverage {
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
                    &["crates/hvc-core/tests/usage_export_header_only_test.rs"],
                ),
                row(
                    "AC-2",
                    CoverageKind::Existing,
                    &["crates/hvc-core/tests/usage_test.rs"],
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
                root: "crates/hvc-core".into(),
                files: vec![BriefFile {
                    path: "tests/usage_export_header_only_test.rs".into(),
                    planned: true,
                    interfaces: Vec::new(),
                }],
            }],
        }),
    )
}

fn integrator_brief() -> hvc_core::orchestration::review_brief::ReviewBrief {
    use hvc_core::orchestration::PublicationState;
    use hvc_core::orchestration::composition::Verdict;
    use hvc_core::orchestration::review_brief::*;
    let verdict = |id: &str, verdict| {
        BriefVerdict {
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
                    "crates/hvc-core/src/usage/export.rs",
                    PulsationStage::Code,
                    PublicationState::Applied,
                ),
                file(
                    "crates/hvc-core/tests/usage_export_header_only_test_with_a_long_name.rs",
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
    brief: hvc_core::orchestration::review_brief::ReviewBrief,
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
