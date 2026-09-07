// The flow engine: turns the current application state into "what should the user do next".
// This is the core of the navigation overhaul described in the UI flow plan (Part 3) — instead of
// the user hunting through tabs, the primary next action is always surfaced in the same place
// (the header's flow bar, see `shell.rs`).
//
// `next_steps` is a pure read over `AppModel`; it never mutates state. `run_next_step` is the only
// place that turns a `FlowTarget` into an actual navigation/action, reusing the exact same
// `select_view`/`forge_action`/`apply_hitl_decision` entry points every other button already calls.

use gpui::{App, Context, SharedString, Window};
use hvc_core::persistence_service::{HitlDecision, PulsationStatus};

use crate::app_model::{AppModel, AppView};
use crate::forge::{ForgeAction, ForgeSection};
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
}

pub(crate) struct NextStep {
    pub id: &'static str,
    pub label: SharedString,
    pub target: FlowTarget,
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
        }
    }
}

/// Returns 1-3 suggested next steps, primary first. Pure function of state — see the state table in
/// the UI flow plan Part 3 for the intent behind each branch.
pub(crate) fn next_steps(model: &AppModel, cx: &App) -> Vec<NextStep> {
    let Some(_project_id) = model.workspace.read(cx).project_id.as_ref() else {
        return vec![NextStep {
            id: "flow-open-project",
            label: "Open project".into(),
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
                        target: FlowTarget::Hitl(HitlDecision::Approve),
                    },
                    NextStep {
                        id: "flow-hitl-revise",
                        label: "Request revision (R)".into(),
                        target: FlowTarget::Hitl(HitlDecision::RequestRevision),
                    },
                    NextStep {
                        id: "flow-hitl-reject",
                        label: "Reject (D)".into(),
                        target: FlowTarget::Hitl(HitlDecision::Reject),
                    },
                ]
            } else {
                vec![
                    NextStep {
                        id: "flow-review-now",
                        label: "Review now".into(),
                        target: FlowTarget::PulsationSection(
                            active.id.clone(),
                            PulsationDetailSection::Review,
                        ),
                    },
                    NextStep {
                        id: "flow-open-artifacts",
                        label: "Open artifacts".into(),
                        target: FlowTarget::PulsationSection(
                            active.id.clone(),
                            PulsationDetailSection::Artifacts,
                        ),
                    },
                ]
            })
        } else {
            active_pulsation_steps(pulsations)
        }
    };
    if let Some(steps) = early {
        return steps;
    }

    let forge = &model.forge;
    let has_ready_tasks = forge
        .snapshot
        .tasks
        .iter()
        .any(|task| task.state == "ready");
    if has_ready_tasks {
        return vec![
            NextStep {
                id: "flow-compose-pulsation",
                label: "Compose pulsation".into(),
                target: FlowTarget::ForgeSection(ForgeSection::Backlog),
            },
            NextStep {
                id: "flow-open-backlog",
                label: "Open backlog".into(),
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
            target: FlowTarget::ForgeSection(ForgeSection::Incubation),
        }];
    }

    let unplanned_approved = forge.snapshot.features.iter().any(|feature| {
        feature.state == hvc_core::feature_forge::FeatureState::Approved && feature.wave.is_none()
    });
    if unplanned_approved {
        return vec![
            NextStep {
                id: "flow-plan-waves",
                label: "Plan waves".into(),
                target: FlowTarget::ForgeSection(ForgeSection::Waves),
            },
            NextStep {
                id: "flow-open-backlog-planned",
                label: "Open backlog".into(),
                target: FlowTarget::ForgeSection(ForgeSection::Backlog),
            },
        ];
    }

    if forge.snapshot.tasks.is_empty() && forge.snapshot.features.is_empty() {
        return vec![
            NextStep {
                id: "flow-new-feature",
                label: "New feature".into(),
                target: FlowTarget::Forge(ForgeAction::New(true)),
            },
            NextStep {
                id: "flow-add-tasks",
                label: "Add tasks".into(),
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
                target: FlowTarget::View(AppView::Pulsations),
            },
            NextStep {
                id: "flow-open-logs",
                label: "Open logs".into(),
                target: FlowTarget::View(AppView::Logs),
            },
        ]),
        PulsationStatus::Failed => Some(vec![
            NextStep {
                id: "flow-open-logs-failed",
                label: "Open logs".into(),
                target: FlowTarget::View(AppView::Logs),
            },
            NextStep {
                id: "flow-reopen-backlog",
                label: "Reopen tasks".into(),
                target: FlowTarget::ForgeSection(ForgeSection::Backlog),
            },
        ]),
        PulsationStatus::Done => Some(vec![
            NextStep {
                id: "flow-open-artifacts-done",
                label: "Open artifacts".into(),
                target: FlowTarget::PulsationSection(
                    active.id.clone(),
                    PulsationDetailSection::Artifacts,
                ),
            },
            NextStep {
                id: "flow-open-vault",
                label: "Open in Vault".into(),
                target: FlowTarget::View(AppView::Vault),
            },
        ]),
        PulsationStatus::Idle | PulsationStatus::Checkpoint => None,
    }
}
