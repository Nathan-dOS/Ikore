// The flow engine: turns the current application state into "what should the user do next".
// This is the core of the navigation overhaul described in the UI flow plan (Part 3) — instead of
// the user hunting through tabs, the primary next action is always surfaced in the same place
// (the header's flow bar, see `shell.rs`).
//
// `next_steps` is a pure read over `AppModel`; it never mutates state. `run_next_step` is the only
// place that turns a `FlowTarget` into an actual navigation/action, reusing the exact same
// `select_view`/`forge_action`/`apply_hitl_decision` entry points every other button already calls.

use gpui::{App, Context, SharedString, Window};
use hvc_core::feature_forge::{FeatureState, Wave};
use hvc_core::persistence_service::{HitlDecision, PulsationStatus};
use std::collections::{HashMap, HashSet};

use crate::app_model::{AppModel, AppView};
use crate::forge::{ForgeAction, ForgeSection};
use crate::icons::Icon;
use crate::pulsations::PulsationDetailSection;

#[derive(Clone)]
pub(crate) enum FlowTarget {
    /// Open the native OS folder picker to choose a project.
    OpenProject,
    /// Switch to a top-level view (no Forge section, no pulsation involved).
    View(AppView),
    /// Switch to the Forge view and select one of its sections.
    ForgeSection(ForgeSection),
    /// Run a Forge action as-is (e.g. opening the capture overlay).
    Forge(ForgeAction),
    /// Select a pulsation and force its Review section open (used for the explicit
    /// "Review now" / "Open artifacts" jumps; a background poll uses a softer heuristic in
    /// `PulsationsModel::apply_snapshot`/`select` instead).
    PulsationSection(String, PulsationDetailSection),
    /// Apply a HITL decision on the pulsation currently open in Review.
    Hitl(HitlDecision),
    /// Open Agents with the canonical agent whose CLI fallback is waiting for a decision.
    AgentFallback(hvc_core::llm::AgentRole),
    /// Open the Settings modal on a page (over the current view).
    Settings(crate::settings::SettingsPage),
}

pub(crate) struct NextStep {
    pub id: &'static str,
    pub label: SharedString,
    pub icon: Icon,
    pub target: FlowTarget,
}

/// A conservative, deterministic batch from the cached snapshot. This is only
/// navigation guidance: dispatch still repeats the host's complete admission.
/// Build dependency order rather than calling any `ready` row executable.
fn has_dispatchable_tasks(forge: &crate::forge::ForgeUi, project: &str) -> bool {
    if forge.project_id.as_deref() != Some(project) {
        return false;
    }
    if forge.start_preview.as_ref().is_some_and(|preview| {
        preview.blockers.iter().any(|issue| {
            matches!(
                issue.code.as_str(),
                "model_configuration" | "base_needs_review"
            )
        })
    }) {
        return false;
    }
    let snapshot = &forge.snapshot;
    let features: HashMap<_, _> = snapshot
        .features
        .iter()
        .filter(|feature| feature.project_id == project)
        .map(|feature| (feature.id.as_str(), feature))
        .collect();
    let done: HashSet<_> = snapshot
        .tasks
        .iter()
        .filter(|task| task.project_id == project && task.state == "done")
        .map(|task| task.id.as_str())
        .collect();
    let candidates: Vec<_> = snapshot
        .tasks
        .iter()
        .filter(|task| {
            if task.project_id != project
                || task.state != "ready"
                || task.active_pulsation_id.is_some()
                || (snapshot.composition_enabled && task.draft.criteria.is_empty())
            {
                return false;
            }
            let Some(feature_id) = task.draft.feature_id.as_deref() else {
                return true;
            };
            let Some(feature) = features.get(feature_id) else {
                return false;
            };
            feature.approved_revision == Some(feature.revision)
                && task.feature_revision == Some(feature.revision)
                && feature.wave == Some(Wave::Current)
                && matches!(
                    feature.state,
                    FeatureState::Approved | FeatureState::Planned
                )
                && forge.start_preview.as_ref().is_none_or(|preview| {
                    preview.feature_id != feature.id
                        || preview.expected_revision != feature.revision
                        || preview.blockers.is_empty()
                })
        })
        .collect();
    let mut admitted = HashSet::new();
    loop {
        let before = admitted.len();
        for task in &candidates {
            if admitted.contains(task.id.as_str())
                || !task
                    .draft
                    .dependencies
                    .iter()
                    .all(|id| done.contains(id.as_str()) || admitted.contains(id.as_str()))
            {
                continue;
            }
            let prerequisites_ready = task.draft.feature_id.as_deref().is_none_or(|feature_id| {
                snapshot
                    .dependencies
                    .iter()
                    .filter(|edge| edge.feature_id == feature_id)
                    .all(|edge| {
                        let Some(prerequisite) = features.get(edge.prerequisite_id.as_str()) else {
                            return false;
                        };
                        if prerequisite.state == FeatureState::Done {
                            return true;
                        }
                        if prerequisite.approved_revision != Some(prerequisite.revision)
                            || prerequisite.wave != Some(Wave::Current)
                        {
                            return false;
                        }
                        let required: Vec<_> = snapshot
                            .tasks
                            .iter()
                            .filter(|task| {
                                task.project_id == project
                                    && task.draft.feature_id.as_deref()
                                        == Some(prerequisite.id.as_str())
                                    && task.feature_revision == prerequisite.approved_revision
                            })
                            .collect();
                        !required.is_empty()
                            && required
                                .iter()
                                .any(|task| admitted.contains(task.id.as_str()))
                            && required.iter().all(|task| {
                                done.contains(task.id.as_str())
                                    || admitted.contains(task.id.as_str())
                            })
                    })
            });
            if prerequisites_ready {
                admitted.insert(task.id.as_str());
                if admitted.len() == 100 {
                    return true;
                }
            }
        }
        if before == admitted.len() {
            return !admitted.is_empty();
        }
    }
}

impl AppModel {
    /// Executes a `NextStep`'s target through the same entry points every other button uses.
    pub(crate) fn run_next_step(
        &mut self,
        target: FlowTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match target {
            FlowTarget::OpenProject => self.choose_workspace(window, cx),
            FlowTarget::View(view) => self.select_view(view, cx),
            FlowTarget::ForgeSection(section) => {
                self.forge.section = section;
                self.select_view(AppView::Forge, cx);
            }
            FlowTarget::Forge(action) => self.forge_action(action, cx),
            FlowTarget::PulsationSection(id, section) => {
                self.select_pulsation(id, cx);
                self.pulsations.update(cx, |pulsations, _| {
                    pulsations.detail_section = section;
                });
                self.select_view(AppView::Pulsations, cx);
            }
            FlowTarget::Hitl(decision) => self.apply_hitl_decision(decision, cx),
            FlowTarget::AgentFallback(role) => {
                self.agents.update(cx, |agents, _| {
                    agents.selected_agent = Some(crate::agents::AgentIdentifier::Canonical(role));
                });
                self.select_view(AppView::Agents, cx);
            }
            FlowTarget::Settings(page) => self.open_settings(Some(page), cx),
        }
    }
}

/// Returns 1-3 suggested next steps, primary first. Pure function of state — see the state table in
/// the UI flow plan Part 3 for the intent behind each branch.
pub(crate) fn next_steps(model: &AppModel, cx: &App) -> Vec<NextStep> {
    let Some(project_id) = model.workspace.read(cx).project_id.as_ref() else {
        return vec![NextStep {
            id: "flow-open-project",
            label: "Open project".into(),
            icon: Icon::FolderOpen,
            target: FlowTarget::OpenProject,
        }];
    };

    // A pulsation at checkpoint, or the Review section itself, takes priority over everything
    // else: it is the one state where the runtime is actively waiting on a human decision.
    let early = {
        let pulsations = model.pulsations.read(cx);
        let checkpoint = pulsations.active().filter(|p| {
            p.status == PulsationStatus::Checkpoint
                && pulsations
                    .hitl_checkpoints
                    .iter()
                    .any(|c| c.pulsation_id == p.id)
        });
        if let Some(active) = checkpoint {
            let already_reviewing = model.active_view() == AppView::Pulsations
                && pulsations.detail_section == PulsationDetailSection::Review;
            Some(if already_reviewing {
                vec![
                    NextStep {
                        id: "flow-hitl-approve",
                        label: "Approve (Enter)".into(),
                        icon: Icon::Check,
                        target: FlowTarget::Hitl(HitlDecision::Approve),
                    },
                    NextStep {
                        id: "flow-hitl-revise",
                        label: "Request revision (R)".into(),
                        icon: Icon::RotateCw,
                        target: FlowTarget::Hitl(HitlDecision::RequestRevision),
                    },
                    NextStep {
                        id: "flow-hitl-reject",
                        label: "Reject (D)".into(),
                        icon: Icon::X,
                        target: FlowTarget::Hitl(HitlDecision::Reject),
                    },
                ]
            } else {
                vec![
                    NextStep {
                        id: "flow-review-now",
                        label: "Review now".into(),
                        icon: Icon::Eye,
                        target: FlowTarget::PulsationSection(
                            active.id.clone(),
                            PulsationDetailSection::Review,
                        ),
                    },
                    NextStep {
                        id: "flow-open-artifacts",
                        label: "Open artifacts".into(),
                        icon: Icon::Layers,
                        target: FlowTarget::PulsationSection(
                            active.id.clone(),
                            PulsationDetailSection::Artifacts,
                        ),
                    },
                ]
            })
        } else if let Some(fallback) = model.agents.read(cx).fallbacks.iter().find(|fallback| {
            pulsations
                .active()
                .is_none_or(|active| active.id == fallback.pulsation_id)
        }) {
            // A CLI fallback (updated CLI awaiting validation, or a transport failure) parks
            // the pulsation until the developer decides in Agents.
            Some(vec![
                NextStep {
                    id: "flow-cli-fallback",
                    label: if fallback.error_code
                        == hvc_core::persistence_service::CLI_VERSION_UNVALIDATED
                    {
                        "Validate updated CLI".into()
                    } else if fallback.error_code
                        == hvc_core::persistence_service::CLI_VALIDATION_FAILED
                    {
                        "Switch provider or abort".into()
                    } else if fallback.error_code == hvc_core::persistence_service::CLI_QUOTA_LOW {
                        "Decide: plan quota low".into()
                    } else {
                        "Decide CLI fallback".into()
                    },
                    icon: Icon::TriangleAlert,
                    target: FlowTarget::AgentFallback(fallback.agent_role),
                },
                NextStep {
                    id: "flow-open-logs-fallback",
                    label: "Open logs".into(),
                    icon: Icon::ScrollText,
                    target: FlowTarget::View(AppView::Logs),
                },
            ])
        } else {
            active_pulsation_steps(pulsations)
        }
    };
    if let Some(steps) = early {
        return steps;
    }

    let forge = &model.forge;
    let base_needs_review =
        model
            .pulsations
            .read(cx)
            .coordination
            .as_ref()
            .is_some_and(|snapshot| {
                snapshot.pipeline.project_id == project_id.as_ref()
                    && snapshot.pipeline.base_state
                        == hvc_core::orchestration::BaseState::NeedsReview
            });
    let has_ready_tasks = !base_needs_review && has_dispatchable_tasks(forge, project_id.as_ref());
    if has_ready_tasks {
        return vec![
            NextStep {
                id: "flow-compose-pulsation",
                label: "Compose pulsation".into(),
                icon: Icon::Play,
                target: FlowTarget::ForgeSection(ForgeSection::Backlog),
            },
            NextStep {
                id: "flow-open-backlog",
                label: "Open backlog".into(),
                icon: Icon::ListTodo,
                target: FlowTarget::ForgeSection(ForgeSection::Backlog),
            },
        ];
    }

    let unplanned_approved = forge.snapshot.features.iter().any(|feature| {
        feature.project_id == project_id.as_ref()
            && feature.approved_revision == Some(feature.revision)
            && matches!(
                feature.state,
                FeatureState::Approved | FeatureState::Planned
            )
            && feature.wave != Some(Wave::Current)
    });
    if unplanned_approved {
        return vec![
            NextStep {
                id: "flow-plan-waves",
                label: "Plan waves".into(),
                icon: Icon::Waves,
                target: FlowTarget::ForgeSection(ForgeSection::Waves),
            },
            NextStep {
                id: "flow-open-backlog-planned",
                label: "Open backlog".into(),
                icon: Icon::ListTodo,
                target: FlowTarget::ForgeSection(ForgeSection::Backlog),
            },
        ];
    }

    // A feature the FeatureAgent just replied to is waiting on the developer's next message.
    if forge.selected_feature.is_some()
        && forge
            .history
            .last()
            .is_some_and(|turn| turn.assistant_message.is_some())
    {
        return vec![NextStep {
            id: "flow-answer-feature-agent",
            label: "Answer Feature Agent".into(),
            icon: Icon::MessageSquare,
            target: FlowTarget::ForgeSection(ForgeSection::Incubation),
        }];
    }

    if forge.snapshot.tasks.is_empty() && forge.snapshot.features.is_empty() {
        return vec![
            NextStep {
                id: "flow-new-feature",
                label: "New feature".into(),
                icon: Icon::Sparkles,
                target: FlowTarget::Forge(ForgeAction::New(true)),
            },
            NextStep {
                id: "flow-add-tasks",
                label: "Add tasks".into(),
                icon: Icon::ListTodo,
                target: FlowTarget::Forge(ForgeAction::New(false)),
            },
        ];
    }

    Vec::new()
}

/// Steps driven by the active pulsation's status once a checkpoint decision isn't pending. Returns
/// `None` when the active pulsation's status doesn't call for a dedicated step (Idle, or Checkpoint
/// — already handled by the caller before reaching here).
fn active_pulsation_steps(
    pulsations: &crate::pulsations::PulsationsModel,
) -> Option<Vec<NextStep>> {
    let active = pulsations.active()?;
    match active.status {
        PulsationStatus::Running | PulsationStatus::Planning => Some(vec![
            NextStep {
                id: "flow-follow-pipeline",
                label: "Follow pipeline".into(),
                icon: Icon::Activity,
                target: FlowTarget::View(AppView::Pulsations),
            },
            NextStep {
                id: "flow-open-logs",
                label: "Open logs".into(),
                icon: Icon::ScrollText,
                target: FlowTarget::View(AppView::Logs),
            },
        ]),
        PulsationStatus::Failed => Some(vec![
            NextStep {
                id: "flow-open-logs-failed",
                label: "Open logs".into(),
                icon: Icon::ScrollText,
                target: FlowTarget::View(AppView::Logs),
            },
            NextStep {
                id: "flow-reopen-backlog",
                label: "Reopen tasks".into(),
                icon: Icon::ListTodo,
                target: FlowTarget::ForgeSection(ForgeSection::Backlog),
            },
        ]),
        PulsationStatus::Done => Some(vec![
            NextStep {
                id: "flow-open-artifacts-done",
                label: "Open artifacts".into(),
                icon: Icon::Layers,
                target: FlowTarget::PulsationSection(
                    active.id.clone(),
                    PulsationDetailSection::Artifacts,
                ),
            },
            NextStep {
                id: "flow-open-vault",
                label: "Open in Vault".into(),
                icon: Icon::Vault,
                target: FlowTarget::View(AppView::Vault),
            },
        ]),
        PulsationStatus::Idle | PulsationStatus::Checkpoint => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext, TestAppContext};
    use hvc_core::feature_forge::{
        Feature, FeatureStartBlocker, FeatureStartPreview, FeatureState, TaskDraft, TaskKind, Wave,
        WorkTask,
    };

    fn ready_task(id: &str, feature: Option<&str>) -> WorkTask {
        WorkTask {
            id: id.into(),
            project_id: "project".into(),
            revision: 1,
            draft: TaskDraft {
                title: id.into(),
                kind: TaskKind::Bug,
                description: "Validate input".into(),
                criteria: vec![],
                dependencies: vec![],
                feature_id: feature.map(str::to_owned),
            },
            feature_revision: feature.map(|_| 1),
            state: "ready".into(),
            active_pulsation_id: None,
            completed_pulsation_id: None,
        }
    }

    fn approved_feature(id: &str, wave: Option<Wave>) -> Feature {
        Feature {
            id: id.into(),
            project_id: "project".into(),
            title: id.into(),
            original_idea: "Validate input".into(),
            revision: 1,
            approved_revision: Some(1),
            state: if wave.is_some() {
                FeatureState::Planned
            } else {
                FeatureState::Approved
            },
            brief: Default::default(),
            evaluation: Default::default(),
            structural_level: 0,
            wave,
            position: 0,
            archive_reason: None,
        }
    }

    fn model(cx: &mut TestAppContext) -> gpui::Entity<AppModel> {
        let model = cx.new(AppModel::new_for_test);
        model.update(cx, |model, cx| {
            model.workspace.update(cx, |workspace, _| {
                workspace.project_id = Some("project".into())
            });
            model.forge.project_id = Some("project".into());
        });
        model
    }

    #[gpui::test]
    fn an_approved_unplanned_feature_offers_plan_waves_before_compose(cx: &mut TestAppContext) {
        let model = model(cx);
        model.update(cx, |model, cx| {
            model.forge.snapshot.features = vec![approved_feature("feature", None)];
            model.forge.snapshot.tasks = vec![ready_task("task", Some("feature"))];
            assert_eq!(next_steps(model, cx)[0].id, "flow-plan-waves");
        });
    }

    #[gpui::test]
    fn blocked_dependencies_and_reservations_never_offer_compose(cx: &mut TestAppContext) {
        let model = model(cx);
        model.update(cx, |model, cx| {
            let mut blocked = ready_task("blocked", None);
            blocked.draft.dependencies = vec!["missing".into()];
            let mut reserved = ready_task("reserved", None);
            reserved.active_pulsation_id = Some("running".into());
            model.forge.snapshot.tasks = vec![blocked, reserved];
            assert!(
                !next_steps(model, cx)
                    .iter()
                    .any(|step| step.id == "flow-compose-pulsation")
            );
        });
    }

    #[gpui::test]
    fn current_revision_batches_and_standalone_tasks_offer_compose(cx: &mut TestAppContext) {
        let model = model(cx);
        model.update(cx, |model, cx| {
            model.forge.snapshot.features = vec![approved_feature("feature", Some(Wave::Current))];
            let mut dependent = ready_task("dependent", Some("feature"));
            dependent.draft.dependencies = vec!["first".into()];
            model.forge.snapshot.tasks = vec![dependent, ready_task("first", None)];
            assert_eq!(next_steps(model, cx)[0].id, "flow-compose-pulsation");
            model.forge.snapshot.tasks = vec![ready_task("standalone", None)];
            model.forge.snapshot.features.clear();
            assert_eq!(next_steps(model, cx)[0].id, "flow-compose-pulsation");
        });
    }

    #[gpui::test]
    fn stale_feature_tasks_and_dependency_cycles_do_not_look_ready(cx: &mut TestAppContext) {
        let model = model(cx);
        model.update(cx, |model, cx| {
            model.forge.snapshot.features = vec![approved_feature("feature", Some(Wave::Current))];
            let mut stale = ready_task("stale", Some("feature"));
            stale.feature_revision = Some(0);
            let mut one = ready_task("one", None);
            one.draft.dependencies = vec!["two".into()];
            let mut two = ready_task("two", None);
            two.draft.dependencies = vec!["one".into()];
            model.forge.snapshot.tasks = vec![stale, one, two];
            assert!(
                !next_steps(model, cx)
                    .iter()
                    .any(|step| step.id == "flow-compose-pulsation")
            );
            model
                .forge
                .snapshot
                .tasks
                .push(ready_task("independent", None));
            assert_eq!(next_steps(model, cx)[0].id, "flow-compose-pulsation");
        });
    }

    #[gpui::test]
    fn matching_backend_preview_blockers_veto_composition(cx: &mut TestAppContext) {
        let model = model(cx);
        model.update(cx, |model, cx| {
            model.forge.snapshot.features = vec![approved_feature("feature", Some(Wave::Current))];
            model.forge.snapshot.tasks = vec![ready_task("task", Some("feature"))];
            model.forge.start_preview = Some(FeatureStartPreview {
                feature_id: "feature".into(),
                expected_revision: 1,
                approved: true,
                planned: true,
                blockers: vec![FeatureStartBlocker {
                    code: "model_configuration".into(),
                    message: "Configure the model first".into(),
                }],
            });
            assert!(
                !next_steps(model, cx)
                    .iter()
                    .any(|step| step.id == "flow-compose-pulsation")
            );
            model.forge.start_preview.as_mut().unwrap().blockers.clear();
            assert_eq!(next_steps(model, cx)[0].id, "flow-compose-pulsation");
        });
    }

    #[gpui::test]
    fn feature_prerequisites_must_fit_a_schedulable_batch(cx: &mut TestAppContext) {
        let model = model(cx);
        model.update(cx, |model, cx| {
            model.forge.snapshot.features = vec![
                approved_feature("dependent", Some(Wave::Current)),
                approved_feature("prerequisite", Some(Wave::Current)),
            ];
            model.forge.snapshot.dependencies = vec![hvc_core::feature_forge::FeatureDependency {
                feature_id: "dependent".into(),
                prerequisite_id: "prerequisite".into(),
                rationale: "Required first".into(),
                origin: Default::default(),
            }];
            let mut prerequisite = ready_task("prerequisite-task", Some("prerequisite"));
            prerequisite.active_pulsation_id = Some("reserved".into());
            model.forge.snapshot.tasks = vec![
                ready_task("dependent-task", Some("dependent")),
                prerequisite,
            ];
            assert!(
                !next_steps(model, cx)
                    .iter()
                    .any(|step| step.id == "flow-compose-pulsation")
            );
            model.forge.snapshot.tasks[1].active_pulsation_id = None;
            assert_eq!(next_steps(model, cx)[0].id, "flow-compose-pulsation");
        });
    }

    #[gpui::test]
    fn a_known_reconciliation_block_prevents_compose(cx: &mut TestAppContext) {
        let model = model(cx);
        model.update(cx, |model, cx| {
            model.forge.snapshot.tasks = vec![ready_task("standalone", None)];
            model.pulsations.update(cx, |pulsations, _| {
                pulsations.coordination =
                    Some(hvc_core::orchestration::ProjectCoordinationSnapshot {
                        pipeline: hvc_core::orchestration::ProjectPipelineState {
                            project_id: "project".into(),
                            active_pulsation_id: None,
                            base_epoch: 1,
                            base_state: hvc_core::orchestration::BaseState::NeedsReview,
                            version: 1,
                            blocking_reason: Some("Review the base".into()),
                        },
                        queue: vec![],
                        owner_candidates: vec![],
                        publications: vec![],
                        review_token: "review-token".into(),
                    });
            });
            assert!(
                !next_steps(model, cx)
                    .iter()
                    .any(|step| step.id == "flow-compose-pulsation")
            );
            model.pulsations.update(cx, |pulsations, _| {
                pulsations
                    .coordination
                    .as_mut()
                    .unwrap()
                    .pipeline
                    .base_state = hvc_core::orchestration::BaseState::Ready
            });
            assert_eq!(next_steps(model, cx)[0].id, "flow-compose-pulsation");
        });
    }
}
